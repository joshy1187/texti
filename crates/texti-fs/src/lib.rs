use chrono::Utc;
use ignore::WalkBuilder;
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use texti_model::{ExplorerRow, FileKind, ReadExtent};
use thiserror::Error;
use uuid::Uuid;

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

pub const MAX_COMPLETE_READ_BYTES: u64 = 256 * 1024 * 1024;
pub const PREVIEW_READ_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum FsError {
    #[error("path not found: {0}")]
    NotFound(PathBuf),
    #[error("permission denied: {0}")]
    PermissionDenied(PathBuf),
    #[error("name is not valid: {0}")]
    InvalidName(String),
    #[error("target already exists: {0}")]
    AlreadyExists(PathBuf),
    #[error("path is not a regular file: {0}")]
    NotRegularFile(PathBuf),
    #[error("operation failed for {path}: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error("workspace root is invalid: {0}")]
    InvalidRoot(PathBuf),
}

#[derive(Clone, Debug)]
pub struct FileSnapshot {
    pub path: PathBuf,
    pub bytes: Vec<u8>,
    pub modified: Option<std::time::SystemTime>,
    pub len: u64,
    pub extent: ReadExtent,
}

#[derive(Clone, Debug)]
pub struct WrittenFile {
    pub modified: Option<std::time::SystemTime>,
    pub len: u64,
}

pub fn read_file(path: &Path) -> Result<FileSnapshot, FsError> {
    let metadata = fs::metadata(path).map_err(|err| map_io(path, err))?;
    if !metadata.is_file() {
        return Err(FsError::NotRegularFile(path.to_path_buf()));
    }
    let len = metadata.len();
    let (bytes, extent) = if len > MAX_COMPLETE_READ_BYTES {
        let mut file = File::open(path).map_err(|err| map_io(path, err))?;
        let mut bytes = Vec::with_capacity(PREVIEW_READ_BYTES);
        Read::by_ref(&mut file)
            .take(PREVIEW_READ_BYTES as u64)
            .read_to_end(&mut bytes)
            .map_err(|err| map_io(path, err))?;
        let shown_bytes = bytes.len() as u64;
        (
            bytes,
            ReadExtent::Preview {
                shown_bytes,
                total_bytes: len,
            },
        )
    } else {
        (
            fs::read(path).map_err(|err| map_io(path, err))?,
            ReadExtent::Complete,
        )
    };
    Ok(FileSnapshot {
        path: path.to_path_buf(),
        bytes,
        modified: metadata.modified().ok(),
        len,
        extent,
    })
}

pub fn canonical_root(root: &Path) -> Result<PathBuf, FsError> {
    let canonical = fs::canonicalize(root).map_err(|err| map_io(root, err))?;
    if !canonical.is_dir() {
        return Err(FsError::InvalidRoot(root.to_path_buf()));
    }
    Ok(canonical)
}

pub fn list_tree(
    root: &Path,
    selected: Option<&Path>,
    show_hidden: bool,
    max_entries: usize,
) -> Result<Vec<ExplorerRow>, FsError> {
    let root = canonical_root(root)?;
    let selected = selected.and_then(|path| fs::canonicalize(path).ok());
    let mut rows = Vec::new();
    rows.push(row_for_path(&root, 0, selected.as_deref(), true)?);

    let walker = WalkBuilder::new(&root)
        .hidden(!show_hidden)
        .git_ignore(false)
        .git_exclude(false)
        .parents(false)
        .max_depth(Some(8))
        .sort_by_file_name(|a, b| {
            a.to_string_lossy()
                .to_lowercase()
                .cmp(&b.to_string_lossy().to_lowercase())
        })
        .build();

    for entry in walker.skip(1) {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        if rows.len() >= max_entries {
            break;
        }
        let depth = entry
            .path()
            .strip_prefix(&root)
            .ok()
            .map(|path| path.components().count() as u16)
            .unwrap_or(0);
        rows.push(row_for_path(
            entry.path(),
            depth,
            selected.as_deref(),
            true,
        )?);
    }
    Ok(rows)
}

pub fn create_file(parent: &Path, name: &str) -> Result<PathBuf, FsError> {
    validate_child_name(name)?;
    let parent = fs::canonicalize(parent).map_err(|err| map_io(parent, err))?;
    let target = parent.join(name);
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        options.mode(0o600);
    }
    options
        .open(&target)
        .map_err(|err| map_create_error(&target, err))?;
    Ok(target)
}

pub fn create_folder(parent: &Path, name: &str) -> Result<PathBuf, FsError> {
    validate_child_name(name)?;
    let parent = fs::canonicalize(parent).map_err(|err| map_io(parent, err))?;
    let target = parent.join(name);
    fs::create_dir(&target).map_err(|err| map_create_error(&target, err))?;
    Ok(target)
}

pub fn rename_path(source: &Path, new_name: &str) -> Result<PathBuf, FsError> {
    validate_child_name(new_name)?;
    let parent = source
        .parent()
        .ok_or_else(|| FsError::InvalidName(new_name.to_string()))?;
    let parent = fs::canonicalize(parent).map_err(|err| map_io(parent, err))?;
    let target = parent.join(new_name);
    if target.exists() {
        return Err(FsError::AlreadyExists(target));
    }
    fs::rename(source, &target).map_err(|err| map_io(source, err))?;
    Ok(target)
}

pub fn atomic_save(path: &Path, bytes: &[u8]) -> Result<WrittenFile, FsError> {
    let parent = path
        .parent()
        .ok_or_else(|| FsError::InvalidName(path.display().to_string()))?;
    let parent_real = fs::canonicalize(parent).map_err(|err| map_io(parent, err))?;
    let file_name = path
        .file_name()
        .ok_or_else(|| FsError::InvalidName(path.display().to_string()))?;

    #[cfg(unix)]
    let mode = match fs::symlink_metadata(path) {
        Ok(meta) if !meta.file_type().is_symlink() => meta.mode() & 0o777,
        _ => 0o600,
    };

    let tmp_path = parent_real.join(format!(
        ".{}.texti.tmp.{}",
        file_name.to_string_lossy(),
        Uuid::new_v4()
    ));

    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        options.mode(mode);
    }

    let result = (|| -> Result<(), FsError> {
        let mut tmp = options
            .open(&tmp_path)
            .map_err(|err| map_create_error(&tmp_path, err))?;
        tmp.write_all(bytes).map_err(|err| map_io(&tmp_path, err))?;
        tmp.sync_all().map_err(|err| map_io(&tmp_path, err))?;
        fs::rename(&tmp_path, path).map_err(|err| map_io(path, err))?;
        #[cfg(unix)]
        {
            if let Ok(dir) = File::open(&parent_real) {
                let _ = dir.sync_all();
            }
        }
        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_file(&tmp_path);
    }
    result?;

    let metadata = fs::metadata(path).map_err(|err| map_io(path, err))?;
    Ok(WrittenFile {
        modified: metadata.modified().ok(),
        len: metadata.len(),
    })
}

pub fn move_to_trash(source: &Path) -> Result<PathBuf, FsError> {
    let metadata = fs::symlink_metadata(source).map_err(|err| map_io(source, err))?;
    let trash_root = trash_root()?;
    let files_dir = trash_root.join("files");
    let info_dir = trash_root.join("info");
    fs::create_dir_all(&files_dir).map_err(|err| map_io(&files_dir, err))?;
    fs::create_dir_all(&info_dir).map_err(|err| map_io(&info_dir, err))?;

    let base_name = source
        .file_name()
        .and_then(OsStr::to_str)
        .filter(|name| !name.is_empty())
        .unwrap_or("trashed");
    let mut target = files_dir.join(base_name);
    if target.exists() {
        target = files_dir.join(format!("{base_name}-{}", Uuid::new_v4()));
    }

    match fs::rename(source, &target) {
        Ok(()) => {}
        Err(err) if err.raw_os_error() == Some(18) => {
            copy_path(source, &target, &metadata)?;
            remove_path(source, &metadata)?;
        }
        Err(err) => return Err(map_io(source, err)),
    }

    write_trash_info(source, &target, &info_dir)?;
    Ok(target)
}

pub fn validate_child_name(name: &str) -> Result<(), FsError> {
    if name.trim().is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\0')
    {
        return Err(FsError::InvalidName(name.to_string()));
    }
    Ok(())
}

fn row_for_path(
    path: &Path,
    depth: u16,
    selected: Option<&Path>,
    expanded: bool,
) -> Result<ExplorerRow, FsError> {
    let metadata = fs::symlink_metadata(path).map_err(|err| map_io(path, err))?;
    let file_type = metadata.file_type();
    let symlink_target = if file_type.is_symlink() {
        fs::read_link(path).ok()
    } else {
        None
    };
    let kind = if file_type.is_symlink() {
        FileKind::Symlink
    } else if file_type.is_dir() {
        FileKind::Directory
    } else if file_type.is_file() {
        FileKind::File
    } else {
        FileKind::Other
    };
    let canonical = fs::canonicalize(path).ok();
    Ok(ExplorerRow {
        path: path.to_path_buf(),
        name: path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string()),
        depth,
        kind,
        expanded,
        selected: selected
            .zip(canonical.as_deref())
            .map(|(selected, canonical)| selected == canonical)
            .unwrap_or(false),
        symlink_target,
    })
}

fn trash_root() -> Result<PathBuf, FsError> {
    let data_home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .ok_or_else(|| FsError::InvalidRoot(PathBuf::from("XDG_DATA_HOME")))?;
    Ok(data_home.join("Trash"))
}

fn write_trash_info(source: &Path, target: &Path, info_dir: &Path) -> Result<(), FsError> {
    let file_name = target
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or("trashed");
    let info_path = info_dir.join(format!("{file_name}.trashinfo"));
    let deletion_date = Utc::now().format("%Y-%m-%dT%H:%M:%S").to_string();
    let escaped_path = source.display().to_string().replace('\n', "%0A");
    let contents = format!("[Trash Info]\nPath={escaped_path}\nDeletionDate={deletion_date}\n");
    fs::write(&info_path, contents).map_err(|err| map_io(&info_path, err))
}

fn copy_path(source: &Path, target: &Path, metadata: &fs::Metadata) -> Result<(), FsError> {
    if metadata.is_dir() {
        fs::create_dir_all(target).map_err(|err| map_io(target, err))?;
        for entry in fs::read_dir(source).map_err(|err| map_io(source, err))? {
            let entry = entry.map_err(|err| map_io(source, err))?;
            let child_source = entry.path();
            let child_target = target.join(entry.file_name());
            let child_meta =
                fs::symlink_metadata(&child_source).map_err(|err| map_io(&child_source, err))?;
            copy_path(&child_source, &child_target, &child_meta)?;
        }
    } else {
        fs::copy(source, target).map_err(|err| map_io(source, err))?;
        #[cfg(unix)]
        {
            let permissions = fs::Permissions::from_mode(metadata.mode() & 0o777);
            let _ = fs::set_permissions(target, permissions);
        }
    }
    Ok(())
}

fn remove_path(source: &Path, metadata: &fs::Metadata) -> Result<(), FsError> {
    if metadata.is_dir() {
        fs::remove_dir_all(source).map_err(|err| map_io(source, err))
    } else {
        fs::remove_file(source).map_err(|err| map_io(source, err))
    }
}

fn map_create_error(path: &Path, err: io::Error) -> FsError {
    match err.kind() {
        io::ErrorKind::AlreadyExists => FsError::AlreadyExists(path.to_path_buf()),
        _ => map_io(path, err),
    }
}

fn map_io(path: &Path, err: io::Error) -> FsError {
    match err.kind() {
        io::ErrorKind::NotFound => FsError::NotFound(path.to_path_buf()),
        io::ErrorKind::PermissionDenied => FsError::PermissionDenied(path.to_path_buf()),
        _ => FsError::Io {
            path: path.to_path_buf(),
            source: err,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn rejects_unsafe_child_names() {
        assert!(validate_child_name("notes.md").is_ok());
        assert!(validate_child_name("../secret").is_err());
        assert!(validate_child_name("").is_err());
        assert!(validate_child_name("a/b").is_err());
    }

    #[test]
    fn atomic_save_replaces_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.txt");
        fs::write(&path, "old").unwrap();
        atomic_save(&path, b"new").unwrap();
        let mut contents = String::new();
        File::open(&path)
            .unwrap()
            .read_to_string(&mut contents)
            .unwrap();
        assert_eq!(contents, "new");
    }

    #[test]
    fn create_file_detects_conflict() {
        let dir = tempfile::tempdir().unwrap();
        create_file(dir.path(), "a.txt").unwrap();
        assert!(matches!(
            create_file(dir.path(), "a.txt"),
            Err(FsError::AlreadyExists(_))
        ));
    }

    #[test]
    fn explorer_lists_root_and_children() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("README.md"), "hi").unwrap();
        let rows = list_tree(dir.path(), None, true, 10).unwrap();
        assert!(rows.iter().any(|row| row.name == "src"));
        assert!(rows.iter().any(|row| row.name == "README.md"));
    }

    #[test]
    fn huge_files_are_read_as_bounded_previews() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("huge.log");
        let file = File::create(&path).unwrap();
        file.set_len(MAX_COMPLETE_READ_BYTES + 1024).unwrap();

        let snapshot = read_file(&path).unwrap();

        assert_eq!(snapshot.bytes.len(), PREVIEW_READ_BYTES);
        assert_eq!(snapshot.len, MAX_COMPLETE_READ_BYTES + 1024);
        assert_eq!(
            snapshot.extent,
            ReadExtent::Preview {
                shown_bytes: PREVIEW_READ_BYTES as u64,
                total_bytes: MAX_COMPLETE_READ_BYTES + 1024,
            }
        );
    }
}
