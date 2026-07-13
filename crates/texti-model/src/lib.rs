use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

pub type BufferId = u64;

pub const SESSION_MANIFEST_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThemeMode {
    #[default]
    System,
    Dark,
    Light,
}

impl ThemeMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::System => "System",
            Self::Dark => "Dark",
            Self::Light => "Light",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SyntaxMode {
    #[default]
    AutoDetect,
    PlainText,
    Rust,
    Markdown,
    Toml,
    Json,
    Slint,
}

impl SyntaxMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::AutoDetect => "Auto Detect",
            Self::PlainText => "Plain Text",
            Self::Rust => "Rust",
            Self::Markdown => "Markdown",
            Self::Toml => "TOML",
            Self::Json => "JSON",
            Self::Slint => "Slint",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DocumentSource {
    File(PathBuf),
    Untitled { name: String },
    Recovery(PathBuf),
}

impl DocumentSource {
    pub fn display_name(&self) -> String {
        match self {
            Self::File(path) => path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string()),
            Self::Untitled { name } => name.clone(),
            Self::Recovery(path) => path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Recovered document".to_string()),
        }
    }

    pub fn path(&self) -> Option<&PathBuf> {
        match self {
            Self::File(path) | Self::Recovery(path) => Some(path),
            Self::Untitled { .. } => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TextEncoding {
    #[default]
    Utf8,
    Utf8Bom,
    Utf16Le,
    Utf16Be,
    Utf16LePreview,
    Utf16BePreview,
    Windows1252Preview,
    Binary,
}

impl TextEncoding {
    pub fn label(self) -> &'static str {
        match self {
            Self::Utf8 => "UTF-8",
            Self::Utf8Bom => "UTF-8 BOM",
            Self::Utf16Le => "UTF-16 LE",
            Self::Utf16Be => "UTF-16 BE",
            Self::Utf16LePreview => "UTF-16 LE preview",
            Self::Utf16BePreview => "UTF-16 BE preview",
            Self::Windows1252Preview => "Windows-1252 preview",
            Self::Binary => "Binary",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum LineEnding {
    #[default]
    Lf,
    CrLf,
}

impl LineEnding {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Lf => "\n",
            Self::CrLf => "\r\n",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReadExtent {
    Complete,
    Preview { shown_bytes: u64, total_bytes: u64 },
}

impl ReadExtent {
    pub fn is_preview(self) -> bool {
        matches!(self, Self::Preview { .. })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LargeFileMode {
    Normal,
    Opportunistic,
    Degraded,
    ReadOnlyPreview,
}

impl LargeFileMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Normal => "Off",
            Self::Opportunistic => "Opportunistic",
            Self::Degraded => "Large file",
            Self::ReadOnlyPreview => "Readonly preview",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SaveState {
    Clean,
    Dirty,
    Saving,
    SaveFailed(String),
    ExternalChangeDetected,
    ReadOnly(String),
}

impl SaveState {
    pub fn dirty(&self) -> bool {
        matches!(
            self,
            Self::Dirty | Self::SaveFailed(_) | Self::ExternalChangeDetected
        )
    }

    pub fn label(&self) -> String {
        match self {
            Self::Clean => "Saved".to_string(),
            Self::Dirty => "Dirty".to_string(),
            Self::Saving => "Saving".to_string(),
            Self::SaveFailed(message) => format!("Save failed: {message}"),
            Self::ExternalChangeDetected => "Changed on disk".to_string(),
            Self::ReadOnly(reason) => format!("Readonly: {reason}"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileKind {
    Directory,
    File,
    Symlink,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExplorerRow {
    pub path: PathBuf,
    pub name: String,
    pub depth: u16,
    pub kind: FileKind,
    pub expanded: bool,
    pub selected: bool,
    pub symlink_target: Option<PathBuf>,
}

impl ExplorerRow {
    pub fn is_dir(&self) -> bool {
        self.kind == FileKind::Directory
    }

    pub fn is_symlink(&self) -> bool {
        self.kind == FileKind::Symlink || self.symlink_target.is_some()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyntaxSpan {
    pub start_byte: usize,
    pub end_byte: usize,
    pub class_name: String,
    #[serde(default)]
    pub foreground: Option<String>,
    #[serde(default)]
    pub bold: bool,
    #[serde(default)]
    pub italic: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchResult {
    pub line_number: usize,
    pub column_number: usize,
    pub line_byte_start: usize,
    pub line_byte_end: usize,
    pub absolute_byte_start: usize,
    pub absolute_byte_end: usize,
    pub preview: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentFile {
    pub path: PathBuf,
    pub display: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewportRow {
    pub line_number: usize,
    pub text: String,
    pub spans: Vec<SyntaxSpan>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewportSnapshot {
    pub buffer_id: BufferId,
    pub revision: u64,
    pub start_line: usize,
    pub rows: Vec<ViewportRow>,
    pub total_lines: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UndoTransaction {
    pub id: Uuid,
    pub description: String,
    pub before: String,
    pub after: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutosaveRecord {
    pub id: Uuid,
    pub buffer_id: BufferId,
    pub source: DocumentSource,
    pub recovery_path: PathBuf,
    pub revision: u64,
    pub saved_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ViewState {
    pub cursor_char: usize,
    pub anchor_char: usize,
    pub preferred_column: Option<usize>,
    pub viewport_x: f32,
    pub viewport_y: f32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FileFingerprint {
    pub modified_at: Option<DateTime<Utc>>,
    pub len: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionDocument {
    pub buffer_id: BufferId,
    pub source: DocumentSource,
    #[serde(default)]
    pub encoding: TextEncoding,
    #[serde(default)]
    pub line_ending: LineEnding,
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub last_saved_revision: u64,
    #[serde(default)]
    pub dirty: bool,
    #[serde(default)]
    pub recovery_path: Option<PathBuf>,
    #[serde(default)]
    pub fingerprint: FileFingerprint,
    #[serde(default)]
    pub view_state: ViewState,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionManifest {
    pub version: u32,
    pub saved_at: DateTime<Utc>,
    pub active_buffer_id: Option<BufferId>,
    pub workspace_root: Option<PathBuf>,
    pub selected_path: Option<PathBuf>,
    pub documents: Vec<SessionDocument>,
}

impl Default for SessionManifest {
    fn default() -> Self {
        Self {
            version: SESSION_MANIFEST_VERSION,
            saved_at: Utc::now(),
            active_buffer_id: None,
            workspace_root: None,
            selected_path: None,
            documents: Vec::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseDecision {
    Save,
    Discard,
    Cancel,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CloseOutcome {
    Closed {
        buffer_id: BufferId,
    },
    NeedsDecision {
        buffer_id: BufferId,
        title: String,
        save_as_required: bool,
    },
    SaveAsRequired {
        buffer_id: BufferId,
    },
    Conflict(FileConflict),
    Cancelled {
        buffer_id: BufferId,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileConflictDecision {
    Reload,
    Overwrite,
    SaveAs(PathBuf),
    Cancel,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SaveOutcome {
    Saved { path: PathBuf },
    Reloaded { path: PathBuf },
    Conflict(FileConflict),
    SaveAsRequired { buffer_id: BufferId },
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileOp {
    CreateFile { parent: PathBuf, name: String },
    CreateFolder { parent: PathBuf, name: String },
    Rename { source: PathBuf, new_name: String },
    MoveToTrash { source: PathBuf },
    PermanentDelete { source: PathBuf },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TabInfo {
    pub id: BufferId,
    pub title: String,
    pub path: Option<PathBuf>,
    pub dirty: bool,
    pub active: bool,
    pub readonly: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusInfo {
    pub text: String,
    pub encoding: TextEncoding,
    pub language: Option<String>,
    pub large_file_mode: LargeFileMode,
    pub save_state: SaveState,
    pub cursor_line: usize,
    pub cursor_col: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileConflict {
    pub buffer_id: BufferId,
    pub path: PathBuf,
    pub message: String,
}
