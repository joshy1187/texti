use chrono::Utc;
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use texti_model::{
    AutosaveRecord, BufferId, DocumentSource, SESSION_MANIFEST_VERSION, SessionManifest,
    SyntaxMode, ThemeMode,
};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum SettingsError {
    #[error("could not resolve app data directories")]
    MissingProjectDirs,
    #[error("settings I/O failed at {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("settings JSON failed at {path}: {source}")]
    Json {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("session manifest version {found} is not supported (expected {expected})")]
    UnsupportedSessionVersion { found: u32, expected: u32 },
}

#[derive(Clone, Debug)]
pub struct SettingsPaths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub cache_dir: PathBuf,
}

impl SettingsPaths {
    pub fn discover() -> Result<Self, SettingsError> {
        let dirs =
            ProjectDirs::from("ai", "Texti", "Texti").ok_or(SettingsError::MissingProjectDirs)?;
        Ok(Self {
            config_dir: dirs.config_dir().to_path_buf(),
            data_dir: dirs.data_dir().to_path_buf(),
            cache_dir: dirs.cache_dir().to_path_buf(),
        })
    }

    pub fn from_root(root: &Path) -> Self {
        Self {
            config_dir: root.join("config"),
            data_dir: root.join("data"),
            cache_dir: root.join("cache"),
        }
    }

    pub fn settings_file(&self) -> PathBuf {
        self.config_dir.join("settings.json")
    }

    pub fn recovery_dir(&self) -> PathBuf {
        self.data_dir.join("recovery")
    }

    pub fn recovery_file(&self, buffer_id: BufferId) -> PathBuf {
        self.recovery_dir().join(format!("buffer-{buffer_id}.txt"))
    }

    pub fn session_file(&self) -> PathBuf {
        self.data_dir.join("session.json")
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct TextiSettings {
    pub theme: ThemeMode,
    pub show_hidden_files: bool,
    pub recovery_autosave: bool,
    pub real_file_autosave: bool,
    pub font_size: u32,
    pub high_contrast: bool,
    pub reduced_motion: bool,
    pub show_line_numbers: bool,
    pub show_minimap: bool,
    pub show_whitespace: bool,
    pub word_wrap: bool,
    pub status_hints: bool,
    pub syntax_highlighting: bool,
    pub default_syntax_mode: SyntaxMode,
    pub tab_size: u8,
    pub insert_spaces: bool,
    pub confirm_trash: bool,
    pub command_palette: CommandPaletteSettings,
    pub recent_files: Vec<PathBuf>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CommandPaletteSettings {
    pub hidden_commands: BTreeSet<String>,
    pub shortcut_overrides: BTreeMap<String, Option<String>>,
}

impl Default for TextiSettings {
    fn default() -> Self {
        Self {
            theme: ThemeMode::Dark,
            show_hidden_files: false,
            recovery_autosave: true,
            real_file_autosave: false,
            font_size: 14,
            high_contrast: false,
            reduced_motion: false,
            show_line_numbers: true,
            show_minimap: false,
            show_whitespace: false,
            word_wrap: false,
            status_hints: true,
            syntax_highlighting: true,
            default_syntax_mode: SyntaxMode::AutoDetect,
            tab_size: 4,
            insert_spaces: true,
            confirm_trash: true,
            command_palette: CommandPaletteSettings::default(),
            recent_files: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct SettingsStore {
    paths: SettingsPaths,
}

impl SettingsStore {
    pub fn new(paths: SettingsPaths) -> Self {
        Self { paths }
    }

    pub fn discover() -> Result<Self, SettingsError> {
        Ok(Self::new(SettingsPaths::discover()?))
    }

    pub fn paths(&self) -> &SettingsPaths {
        &self.paths
    }

    pub fn load(&self) -> Result<TextiSettings, SettingsError> {
        let path = self.paths.settings_file();
        if !path.exists() {
            return Ok(TextiSettings::default());
        }
        let bytes = fs::read(&path).map_err(|source| SettingsError::Io {
            path: path.clone(),
            source,
        })?;
        let mut settings: TextiSettings = serde_json::from_slice(&bytes)
            .map_err(|source| SettingsError::Json { path, source })?;
        settings.normalize_runtime_defaults();
        Ok(settings)
    }

    pub fn save(&self, settings: &TextiSettings) -> Result<(), SettingsError> {
        let path = self.paths.settings_file();
        let mut settings = settings.clone();
        settings.normalize_runtime_defaults();
        let bytes = serde_json::to_vec_pretty(&settings).map_err(|source| SettingsError::Json {
            path: path.clone(),
            source,
        })?;
        atomic_write(&path, &bytes)
    }

    pub fn write_recovery(
        &self,
        buffer_id: BufferId,
        source: &DocumentSource,
        revision: u64,
        text: &str,
    ) -> Result<AutosaveRecord, SettingsError> {
        let recovery_dir = self.paths.recovery_dir();
        fs::create_dir_all(&recovery_dir).map_err(|source| SettingsError::Io {
            path: recovery_dir.clone(),
            source,
        })?;
        let id = Uuid::new_v4();
        let recovery_path = self.paths.recovery_file(buffer_id);
        atomic_write(&recovery_path, text.as_bytes())?;
        self.remove_legacy_recovery_files(buffer_id)?;
        Ok(AutosaveRecord {
            id,
            buffer_id,
            source: source.clone(),
            recovery_path,
            revision,
            saved_at: Utc::now(),
        })
    }

    pub fn read_recovery(&self, path: impl AsRef<Path>) -> Result<String, SettingsError> {
        let path = path.as_ref().to_path_buf();
        fs::read_to_string(&path).map_err(|source| SettingsError::Io { path, source })
    }

    pub fn remove_recovery(&self, buffer_id: BufferId) -> Result<(), SettingsError> {
        let path = self.paths.recovery_file(buffer_id);
        remove_file_if_exists(&path)?;
        self.remove_legacy_recovery_files(buffer_id)
    }

    pub fn cleanup_recovery_except(&self, retained_paths: &[PathBuf]) -> Result<(), SettingsError> {
        let recovery_dir = self.paths.recovery_dir();
        let entries = match fs::read_dir(&recovery_dir) {
            Ok(entries) => entries,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(source) => {
                return Err(SettingsError::Io {
                    path: recovery_dir,
                    source,
                });
            }
        };
        for entry in entries {
            let entry = entry.map_err(|source| SettingsError::Io {
                path: recovery_dir.clone(),
                source,
            })?;
            let path = entry.path();
            if entry
                .file_type()
                .map_err(|source| SettingsError::Io {
                    path: path.clone(),
                    source,
                })?
                .is_file()
                && !retained_paths.iter().any(|retained| retained == &path)
            {
                remove_file_if_exists(&path)?;
            }
        }
        Ok(())
    }

    pub fn load_session(&self) -> Result<Option<SessionManifest>, SettingsError> {
        let path = self.paths.session_file();
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(SettingsError::Io {
                    path: path.clone(),
                    source,
                });
            }
        };
        let manifest: SessionManifest =
            serde_json::from_slice(&bytes).map_err(|source| SettingsError::Json {
                path: path.clone(),
                source,
            })?;
        if manifest.version != SESSION_MANIFEST_VERSION {
            return Err(SettingsError::UnsupportedSessionVersion {
                found: manifest.version,
                expected: SESSION_MANIFEST_VERSION,
            });
        }
        Ok(Some(manifest))
    }

    pub fn save_session(&self, manifest: &SessionManifest) -> Result<(), SettingsError> {
        let path = self.paths.session_file();
        let mut manifest = manifest.clone();
        manifest.version = SESSION_MANIFEST_VERSION;
        manifest.saved_at = Utc::now();
        let bytes = serde_json::to_vec_pretty(&manifest).map_err(|source| SettingsError::Json {
            path: path.clone(),
            source,
        })?;
        atomic_write(&path, &bytes)
    }

    pub fn clear_session(&self) -> Result<(), SettingsError> {
        remove_file_if_exists(&self.paths.session_file())?;
        self.cleanup_recovery_except(&[])
    }

    fn remove_legacy_recovery_files(&self, buffer_id: BufferId) -> Result<(), SettingsError> {
        let recovery_dir = self.paths.recovery_dir();
        let entries = match fs::read_dir(&recovery_dir) {
            Ok(entries) => entries,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(source) => {
                return Err(SettingsError::Io {
                    path: recovery_dir,
                    source,
                });
            }
        };
        let legacy_prefix = format!("{buffer_id}-");
        for entry in entries {
            let entry = entry.map_err(|source| SettingsError::Io {
                path: recovery_dir.clone(),
                source,
            })?;
            if entry
                .file_name()
                .to_string_lossy()
                .starts_with(&legacy_prefix)
            {
                let path = entry.path();
                if entry
                    .file_type()
                    .map_err(|source| SettingsError::Io {
                        path: path.clone(),
                        source,
                    })?
                    .is_file()
                {
                    remove_file_if_exists(&path)?;
                }
            }
        }
        Ok(())
    }
}

impl TextiSettings {
    pub fn normalize_runtime_defaults(&mut self) {
        self.theme = ThemeMode::Dark;
        self.syntax_highlighting = true;
        self.default_syntax_mode = SyntaxMode::AutoDetect;
        self.font_size = self.font_size.clamp(12, 22);
        if !matches!(self.tab_size, 2 | 4 | 8) {
            self.tab_size = 4;
        }
        self.command_palette.hidden_commands = self
            .command_palette
            .hidden_commands
            .iter()
            .map(|id| id.trim())
            .filter(|id| !id.is_empty())
            .map(str::to_string)
            .collect();
        self.command_palette.shortcut_overrides = self
            .command_palette
            .shortcut_overrides
            .iter()
            .filter_map(|(id, shortcut)| {
                let id = id.trim();
                if id.is_empty() {
                    return None;
                }
                Some((
                    id.to_string(),
                    shortcut
                        .as_ref()
                        .map(|value| value.trim().to_string())
                        .filter(|value| !value.is_empty()),
                ))
            })
            .collect();
    }
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), SettingsError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|source| SettingsError::Io {
        path: parent.to_path_buf(),
        source,
    })?;
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_default();
    let temp_path = parent.join(format!(".{file_name}.texti.tmp.{}", Uuid::new_v4()));
    let result = (|| {
        let mut temp = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
            .map_err(|source| SettingsError::Io {
                path: temp_path.clone(),
                source,
            })?;
        temp.write_all(bytes).map_err(|source| SettingsError::Io {
            path: temp_path.clone(),
            source,
        })?;
        temp.sync_all().map_err(|source| SettingsError::Io {
            path: temp_path.clone(),
            source,
        })?;
        fs::rename(&temp_path, path).map_err(|source| SettingsError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        if let Ok(parent_dir) = File::open(parent) {
            let _ = parent_dir.sync_all();
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result
}

fn remove_file_if_exists(path: &Path) -> Result<(), SettingsError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(SettingsError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_settings() {
        let dir = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(SettingsPaths::from_root(dir.path()));
        let settings = TextiSettings {
            show_hidden_files: true,
            ..TextiSettings::default()
        };
        store.save(&settings).unwrap();
        assert!(store.load().unwrap().show_hidden_files);
    }

    #[test]
    fn missing_new_fields_use_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(SettingsPaths::from_root(dir.path()));
        fs::create_dir_all(&store.paths().config_dir).unwrap();
        fs::write(
            store.paths().settings_file(),
            r#"{"show_hidden_files":true,"recovery_autosave":false,"real_file_autosave":false,"font_size":16,"high_contrast":false,"reduced_motion":false}"#,
        )
        .unwrap();

        let settings = store.load().unwrap();
        assert!(settings.show_hidden_files);
        assert!(!settings.recovery_autosave);
        assert!(settings.show_line_numbers);
        assert_eq!(settings.theme, ThemeMode::Dark);
        assert!(!settings.show_minimap);
        assert!(!settings.show_whitespace);
        assert!(settings.status_hints);
        assert_eq!(settings.default_syntax_mode, SyntaxMode::AutoDetect);
        assert_eq!(settings.tab_size, 4);
    }

    #[test]
    fn legacy_syntax_settings_are_coerced_to_auto() {
        let dir = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(SettingsPaths::from_root(dir.path()));
        fs::create_dir_all(&store.paths().config_dir).unwrap();
        fs::write(
            store.paths().settings_file(),
            r#"{"syntax_highlighting":false,"default_syntax_mode":"Rust"}"#,
        )
        .unwrap();

        let settings = store.load().unwrap();
        assert!(settings.syntax_highlighting);
        assert_eq!(settings.default_syntax_mode, SyntaxMode::AutoDetect);
    }

    #[test]
    fn legacy_light_and_system_themes_are_coerced_to_dark() {
        for legacy_theme in ["Light", "System"] {
            let dir = tempfile::tempdir().unwrap();
            let store = SettingsStore::new(SettingsPaths::from_root(dir.path()));
            fs::create_dir_all(&store.paths().config_dir).unwrap();
            fs::write(
                store.paths().settings_file(),
                format!(r#"{{"theme":"{legacy_theme}"}}"#),
            )
            .unwrap();

            let settings = store.load().unwrap();
            assert_eq!(settings.theme, ThemeMode::Dark);
            store.save(&settings).unwrap();
            assert_eq!(store.load().unwrap().theme, ThemeMode::Dark);
        }
    }

    #[test]
    fn command_customizations_round_trip_and_empty_values_clear() {
        let dir = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(SettingsPaths::from_root(dir.path()));
        let mut settings = TextiSettings::default();
        settings
            .command_palette
            .hidden_commands
            .insert("view.minimap".to_string());
        settings
            .command_palette
            .shortcut_overrides
            .insert("file.save".to_string(), Some("  Ctrl+Alt+S  ".to_string()));
        settings
            .command_palette
            .shortcut_overrides
            .insert("edit.copy".to_string(), Some("  ".to_string()));

        store.save(&settings).unwrap();
        let loaded = store.load().unwrap();
        assert!(
            loaded
                .command_palette
                .hidden_commands
                .contains("view.minimap")
        );
        assert_eq!(
            loaded.command_palette.shortcut_overrides.get("file.save"),
            Some(&Some("Ctrl+Alt+S".to_string()))
        );
        assert_eq!(
            loaded.command_palette.shortcut_overrides.get("edit.copy"),
            Some(&None)
        );
    }

    #[test]
    fn writes_recovery_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(SettingsPaths::from_root(dir.path()));
        let record = store
            .write_recovery(
                7,
                &DocumentSource::Untitled {
                    name: "draft.md".to_string(),
                },
                2,
                "hello",
            )
            .unwrap();
        assert_eq!(fs::read_to_string(record.recovery_path).unwrap(), "hello");
    }

    #[test]
    fn recovery_overwrites_one_stable_buffer_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(SettingsPaths::from_root(dir.path()));
        let source = DocumentSource::Untitled {
            name: "draft.md".to_string(),
        };
        let legacy = store.paths().recovery_dir().join("7-1-draft-old-id.txt");
        fs::create_dir_all(store.paths().recovery_dir()).unwrap();
        fs::write(&legacy, "old").unwrap();

        let first = store.write_recovery(7, &source, 1, "first").unwrap();
        let second = store.write_recovery(7, &source, 2, "second").unwrap();

        assert_eq!(first.recovery_path, second.recovery_path);
        assert_eq!(first.recovery_path, store.paths().recovery_file(7));
        assert_eq!(fs::read_to_string(&second.recovery_path).unwrap(), "second");
        assert!(!legacy.exists());
        assert_eq!(
            fs::read_dir(store.paths().recovery_dir()).unwrap().count(),
            1
        );
    }

    #[test]
    fn session_manifest_round_trips_and_rejects_future_versions() {
        let dir = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(SettingsPaths::from_root(dir.path()));
        let manifest = SessionManifest {
            active_buffer_id: Some(9),
            workspace_root: Some(dir.path().to_path_buf()),
            ..SessionManifest::default()
        };
        store.save_session(&manifest).unwrap();
        let loaded = store.load_session().unwrap().unwrap();
        assert_eq!(loaded.version, SESSION_MANIFEST_VERSION);
        assert_eq!(loaded.active_buffer_id, Some(9));

        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(store.paths().session_file()).unwrap()).unwrap();
        value["version"] = serde_json::json!(SESSION_MANIFEST_VERSION + 1);
        fs::write(
            store.paths().session_file(),
            serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();
        assert!(matches!(
            store.load_session(),
            Err(SettingsError::UnsupportedSessionVersion { .. })
        ));
    }

    #[test]
    fn normalizes_invalid_numeric_preferences() {
        let dir = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(SettingsPaths::from_root(dir.path()));
        fs::create_dir_all(&store.paths().config_dir).unwrap();
        fs::write(
            store.paths().settings_file(),
            r#"{"font_size":99,"tab_size":3}"#,
        )
        .unwrap();

        let settings = store.load().unwrap();
        assert_eq!(settings.font_size, 22);
        assert_eq!(settings.tab_size, 4);
        assert_eq!(settings.theme, ThemeMode::Dark);
    }
}
