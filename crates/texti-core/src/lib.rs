use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use texti_editor::Buffer;
use texti_model::{
    AutosaveRecord, BufferId, CloseDecision, CloseOutcome, DocumentSource, ExplorerRow,
    FileConflict, FileConflictDecision, FileFingerprint, LargeFileMode, LineEnding, RecentFile,
    SaveOutcome, SaveState, SearchResult, SessionDocument, SessionManifest, StatusInfo, SyntaxMode,
    SyntaxSpan, TabInfo, TextEncoding, ViewState,
};
use texti_settings::{SettingsStore, TextiSettings};
use texti_syntax::{HighlightTheme, SyntaxService};

#[derive(Clone, Debug)]
pub struct AppSnapshot {
    pub active_buffer_id: Option<BufferId>,
    pub active_revision: u64,
    pub active_line_ending: LineEnding,
    pub editor_text: String,
    pub tabs: Vec<TabInfo>,
    pub explorer_rows: Vec<ExplorerRow>,
    pub recent_files: Vec<RecentFile>,
    pub status: StatusInfo,
    pub window_title: String,
    pub workspace_root: Option<PathBuf>,
    pub selected_path: Option<PathBuf>,
    pub syntax_spans: Vec<SyntaxSpan>,
    pub settings: TextiSettings,
    pub active_view_state: ViewState,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SyntaxCacheKey {
    revision: u64,
    theme: HighlightTheme,
    path: Option<PathBuf>,
    large_file_mode: LargeFileMode,
}

#[derive(Debug)]
struct SyntaxCacheEntry {
    key: SyntaxCacheKey,
    spans: Vec<SyntaxSpan>,
}

#[derive(Debug)]
pub struct AppState {
    buffers: Vec<Buffer>,
    active: Option<BufferId>,
    next_buffer_id: BufferId,
    workspace_root: Option<PathBuf>,
    selected_path: Option<PathBuf>,
    settings_store: SettingsStore,
    settings: TextiSettings,
    last_message: String,
    autosave_records: Vec<AutosaveRecord>,
    view_states: HashMap<BufferId, ViewState>,
    explorer_rows_cache: Vec<ExplorerRow>,
    syntax_cache: RefCell<HashMap<BufferId, SyntaxCacheEntry>>,
}

impl AppState {
    pub fn new(settings_store: SettingsStore) -> Result<Self> {
        let settings = settings_store.load().unwrap_or_default();
        let mut state = Self {
            buffers: Vec::new(),
            active: None,
            next_buffer_id: 1,
            workspace_root: None,
            selected_path: None,
            settings_store,
            settings,
            last_message: "Ready".to_string(),
            autosave_records: Vec::new(),
            view_states: HashMap::new(),
            explorer_rows_cache: Vec::new(),
            syntax_cache: RefCell::new(HashMap::new()),
        };
        match state.settings_store.load_session() {
            Ok(Some(manifest)) => {
                if let Err(error) = state.restore_session_manifest(manifest) {
                    state.last_message = format!("Could not restore previous session: {error:#}");
                }
            }
            Ok(None) => {}
            Err(error) => {
                state.last_message = format!("Ignored invalid session: {error}");
            }
        }
        state.refresh_explorer_cache();
        if state.buffers.is_empty() {
            let message = state.last_message.clone();
            state.new_untitled();
            if message != "Ready" {
                state.last_message = message;
            }
        }
        Ok(state)
    }

    pub fn settings(&self) -> &TextiSettings {
        &self.settings
    }

    pub fn open_folder(&mut self, path: impl AsRef<Path>) -> Result<()> {
        let root = texti_fs::canonical_root(path.as_ref())
            .with_context(|| format!("opening folder {}", path.as_ref().display()))?;
        self.workspace_root = Some(root.clone());
        self.selected_path = Some(root.clone());
        self.refresh_explorer_cache();
        self.last_message = format!("Opened folder {}", root.display());
        Ok(())
    }

    pub fn open_file(&mut self, path: impl AsRef<Path>) -> Result<BufferId> {
        let requested_path = path.as_ref();
        let path = std::fs::canonicalize(requested_path)
            .with_context(|| format!("resolving file {}", requested_path.display()))?;
        if let Some(existing) = self
            .buffers
            .iter()
            .find(|buffer| {
                buffer
                    .source
                    .path()
                    .is_some_and(|open_path| same_path(open_path, &path))
            })
            .map(|buffer| buffer.id)
        {
            self.active = Some(existing);
            self.selected_path = Some(path.clone());
            self.refresh_explorer_cache();
            self.remember_recent_file(&path)?;
            self.last_message = format!("Activated {}", path.display());
            return Ok(existing);
        }
        let snapshot = texti_fs::read_file(&path)
            .with_context(|| format!("reading file {}", path.display()))?;
        let id = self.allocate_id();
        let opened = texti_editor::Buffer::from_bytes(
            id,
            DocumentSource::File(snapshot.path.clone()),
            &snapshot.bytes,
            snapshot.modified,
            Some(snapshot.len),
            snapshot.extent,
        )
        .with_context(|| format!("opening document {}", path.display()))?;
        let warning = opened.warning.clone();
        if let Some(index) = self.replaceable_active_tab_index() {
            let replaced_id = self.buffers[index].id;
            self.buffers[index] = opened.buffer;
            self.view_states.remove(&replaced_id);
            self.syntax_cache.get_mut().remove(&replaced_id);
        } else {
            self.buffers.push(opened.buffer);
        }
        self.view_states.insert(id, ViewState::default());
        self.active = Some(id);
        self.selected_path = Some(snapshot.path.clone());
        self.refresh_explorer_cache();
        self.remember_recent_file(&snapshot.path)?;
        self.last_message =
            warning.unwrap_or_else(|| format!("Opened {}", snapshot.path.display()));
        Ok(id)
    }

    pub fn new_untitled(&mut self) -> BufferId {
        let id = self.allocate_id();
        let buffer = Buffer::new_untitled(id);
        self.buffers.push(buffer);
        self.view_states.insert(id, ViewState::default());
        self.active = Some(id);
        self.last_message = "New untitled document".to_string();
        id
    }

    pub fn open_startup_paths(&mut self, paths: impl IntoIterator<Item = PathBuf>) -> Result<()> {
        let blank_untitled = self.blank_untitled_buffer_id();
        let mut opened_files = 0usize;
        let mut opened_any = false;

        for path in paths {
            let metadata = std::fs::metadata(&path)
                .with_context(|| format!("inspecting {}", path.display()))?;
            if metadata.is_dir() {
                self.open_folder(&path)?;
                opened_any = true;
            } else if metadata.is_file() {
                self.open_file(&path)?;
                opened_files += 1;
                opened_any = true;
            } else {
                bail!("Unsupported startup path: {}", path.display());
            }
        }

        if opened_files > 0
            && let Some(id) = blank_untitled
            && let Some(index) = self.buffers.iter().position(|buffer| buffer.id == id)
            && self.is_blank_untitled_at(index)
        {
            self.buffers.remove(index);
            self.view_states.remove(&id);
            self.syntax_cache.get_mut().remove(&id);
        }
        if !opened_any {
            self.last_message = "New untitled document".to_string();
        }
        Ok(())
    }

    pub fn select_or_open(&mut self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        self.selected_path = Some(path.to_path_buf());
        let metadata = std::fs::symlink_metadata(path)
            .with_context(|| format!("inspecting {}", path.display()))?;
        let target_metadata = metadata
            .file_type()
            .is_symlink()
            .then(|| std::fs::metadata(path).ok())
            .flatten();
        if metadata.is_file()
            || target_metadata
                .as_ref()
                .is_some_and(|entry| entry.is_file())
        {
            if let Some(existing) = self
                .buffers
                .iter()
                .find(|buffer| buffer.source.path().is_some_and(|p| same_path(p, path)))
                .map(|buffer| buffer.id)
            {
                self.active = Some(existing);
                self.last_message = format!("Activated {}", path.display());
            } else {
                self.open_file(path)?;
            }
        } else if metadata.is_dir() {
            self.last_message = format!("Selected folder {}", path.display());
        }
        self.refresh_explorer_cache();
        Ok(())
    }

    pub fn activate_tab(&mut self, id: BufferId) -> Result<()> {
        if self.buffers.iter().any(|buffer| buffer.id == id) {
            self.active = Some(id);
            self.last_message = "Activated tab".to_string();
            Ok(())
        } else {
            bail!("Tab no longer exists")
        }
    }

    pub fn set_message(&mut self, message: impl Into<String>) {
        self.last_message = message.into();
    }

    pub fn update_active_text(&mut self, text: String) -> Result<()> {
        let buffer = self.active_buffer_mut()?;
        buffer.set_text(text, "edit")?;
        Ok(())
    }

    pub fn replace_active_range_chars(
        &mut self,
        start_char: usize,
        end_char: usize,
        text: &str,
    ) -> Result<()> {
        let buffer = self.active_buffer_mut()?;
        buffer.replace_range(start_char, end_char, text)?;
        Ok(())
    }

    pub fn replace_active_range_chars_isolated(
        &mut self,
        start_char: usize,
        end_char: usize,
        text: &str,
    ) -> Result<()> {
        let buffer = self.active_buffer_mut()?;
        buffer.replace_range_isolated(start_char, end_char, text)?;
        Ok(())
    }

    pub fn save_active(&mut self) -> Result<()> {
        match self.save_active_checked()? {
            SaveOutcome::Saved { .. } => Ok(()),
            SaveOutcome::SaveAsRequired { .. } => bail!("Save As is required for this buffer"),
            SaveOutcome::Conflict(conflict) => bail!(conflict.message),
            SaveOutcome::Reloaded { .. } | SaveOutcome::Cancelled => Ok(()),
        }
    }

    pub fn save_active_as(&mut self, path: impl AsRef<Path>) -> Result<()> {
        let id = self.active_buffer()?.id;
        self.save_buffer_as(id, path)?;
        Ok(())
    }

    pub fn save_buffer_as(
        &mut self,
        buffer_id: BufferId,
        path: impl AsRef<Path>,
    ) -> Result<SaveOutcome> {
        self.save_buffer_as_unchecked(buffer_id, path.as_ref().to_path_buf())
    }

    pub fn save_active_checked(&mut self) -> Result<SaveOutcome> {
        let id = self.active_buffer()?.id;
        self.save_buffer_checked(id)
    }

    pub fn resolve_active_file_conflict(
        &mut self,
        decision: FileConflictDecision,
    ) -> Result<SaveOutcome> {
        let id = self.active_buffer()?.id;
        self.resolve_file_conflict(id, decision)
    }

    pub fn resolve_file_conflict(
        &mut self,
        buffer_id: BufferId,
        decision: FileConflictDecision,
    ) -> Result<SaveOutcome> {
        match decision {
            FileConflictDecision::Reload => {
                let path = self.reload_buffer_from_disk(buffer_id)?;
                Ok(SaveOutcome::Reloaded { path })
            }
            FileConflictDecision::Overwrite => {
                let path = self
                    .buffer(buffer_id)?
                    .source
                    .path()
                    .cloned()
                    .context("Save As is required for this buffer")?;
                self.save_buffer_as_unchecked(buffer_id, path)
            }
            FileConflictDecision::SaveAs(path) => self.save_buffer_as_unchecked(buffer_id, path),
            FileConflictDecision::Cancel => Ok(SaveOutcome::Cancelled),
        }
    }

    pub fn reload_active_from_disk(&mut self) -> Result<()> {
        let buffer = self.active_buffer()?;
        if buffer.is_dirty() {
            bail!("Save or discard unsaved changes before reloading from disk");
        }
        let id = buffer.id;
        self.reload_buffer_from_disk(id)?;
        Ok(())
    }

    pub fn undo_active(&mut self) -> Result<()> {
        let changed = self.active_buffer_mut()?.undo();
        self.last_message = if changed {
            "Undo".to_string()
        } else {
            "Nothing to undo".to_string()
        };
        Ok(())
    }

    pub fn redo_active(&mut self) -> Result<()> {
        let changed = self.active_buffer_mut()?.redo();
        self.last_message = if changed {
            "Redo".to_string()
        } else {
            "Nothing to redo".to_string()
        };
        Ok(())
    }

    pub fn close_active(&mut self) -> Result<()> {
        let Some(active) = self.active else {
            return Ok(());
        };
        match self.request_close_buffer(active)? {
            CloseOutcome::Closed { .. } => Ok(()),
            CloseOutcome::NeedsDecision { .. } => {
                bail!("Active tab has unsaved changes; choose Save, Discard, or Cancel")
            }
            CloseOutcome::SaveAsRequired { .. } => bail!("Save As is required for this buffer"),
            CloseOutcome::Conflict(conflict) => bail!(conflict.message),
            CloseOutcome::Cancelled { .. } => Ok(()),
        }
    }

    pub fn request_close_active(&mut self) -> Result<CloseOutcome> {
        let Some(active) = self.active else {
            return Ok(CloseOutcome::Cancelled { buffer_id: 0 });
        };
        self.request_close_buffer(active)
    }

    pub fn request_close_buffer(&mut self, buffer_id: BufferId) -> Result<CloseOutcome> {
        let buffer = self.buffer(buffer_id)?;
        if buffer.is_dirty() {
            return Ok(CloseOutcome::NeedsDecision {
                buffer_id,
                title: buffer.display_title(),
                save_as_required: buffer.source.path().is_none(),
            });
        }
        self.remove_buffer(buffer_id)?;
        Ok(CloseOutcome::Closed { buffer_id })
    }

    pub fn close_active_with_decision(&mut self, decision: CloseDecision) -> Result<CloseOutcome> {
        let Some(active) = self.active else {
            return Ok(CloseOutcome::Cancelled { buffer_id: 0 });
        };
        self.close_buffer_with_decision(active, decision)
    }

    pub fn close_buffer_with_decision(
        &mut self,
        buffer_id: BufferId,
        decision: CloseDecision,
    ) -> Result<CloseOutcome> {
        if !self.buffer(buffer_id)?.is_dirty() {
            return self.request_close_buffer(buffer_id);
        }
        match decision {
            CloseDecision::Cancel => Ok(CloseOutcome::Cancelled { buffer_id }),
            CloseDecision::Discard => {
                self.remove_buffer(buffer_id)?;
                Ok(CloseOutcome::Closed { buffer_id })
            }
            CloseDecision::Save => match self.save_buffer_checked(buffer_id)? {
                SaveOutcome::Saved { .. } => {
                    self.remove_buffer(buffer_id)?;
                    Ok(CloseOutcome::Closed { buffer_id })
                }
                SaveOutcome::SaveAsRequired { .. } => {
                    Ok(CloseOutcome::SaveAsRequired { buffer_id })
                }
                SaveOutcome::Conflict(conflict) => Ok(CloseOutcome::Conflict(conflict)),
                SaveOutcome::Reloaded { .. } | SaveOutcome::Cancelled => {
                    Ok(CloseOutcome::Cancelled { buffer_id })
                }
            },
        }
    }

    pub fn create_file_in_selection(&mut self, name: &str) -> Result<()> {
        let parent = self.selected_parent()?;
        let path = texti_fs::create_file(&parent, name)?;
        self.selected_path = Some(path.clone());
        self.open_file(path)?;
        self.refresh_explorer_cache();
        Ok(())
    }

    pub fn create_folder_in_selection(&mut self, name: &str) -> Result<()> {
        let parent = self.selected_parent()?;
        let path = texti_fs::create_folder(&parent, name)?;
        self.selected_path = Some(path.clone());
        self.refresh_explorer_cache();
        self.last_message = format!("Created folder {}", path.display());
        Ok(())
    }

    pub fn rename_selected(&mut self, new_name: &str) -> Result<()> {
        let source = self
            .selected_path
            .clone()
            .context("No explorer selection")?;
        if self
            .workspace_root
            .as_ref()
            .is_some_and(|root| same_path(root, &source))
        {
            bail!("Refusing to rename workspace root");
        }
        let source_is_dir = std::fs::symlink_metadata(&source)
            .with_context(|| format!("inspecting {}", source.display()))?
            .file_type()
            .is_dir();
        let affected = self
            .buffers
            .iter()
            .filter_map(|buffer| {
                let path = buffer.source.path()?;
                let suffix = if path == &source {
                    PathBuf::new()
                } else if source_is_dir {
                    path.strip_prefix(&source).ok()?.to_path_buf()
                } else {
                    return None;
                };
                Some((buffer.id, suffix))
            })
            .collect::<Vec<_>>();
        let new_path = texti_fs::rename_path(&source, new_name)?;
        for (buffer_id, suffix) in &affected {
            let buffer = self.buffer_mut(*buffer_id)?;
            let remapped = new_path.join(suffix);
            buffer.source = match &buffer.source {
                DocumentSource::Recovery(_) => DocumentSource::Recovery(remapped),
                _ => DocumentSource::File(remapped),
            };
        }
        for (buffer_id, _) in affected {
            self.syntax_cache.get_mut().remove(&buffer_id);
        }
        self.selected_path = Some(new_path.clone());
        self.refresh_explorer_cache();
        self.last_message = format!("Renamed to {}", new_path.display());
        Ok(())
    }

    pub fn trash_selected(&mut self) -> Result<()> {
        self.trash_selected_unchecked()
    }

    pub fn trash_selected_confirmed(&mut self, confirmed: bool) -> Result<()> {
        if self.settings.confirm_trash && !confirmed {
            bail!("Trash confirmation required");
        }
        self.trash_selected_unchecked()
    }

    fn trash_selected_unchecked(&mut self) -> Result<()> {
        let source = self
            .selected_path
            .clone()
            .context("No explorer selection")?;
        if self
            .workspace_root
            .as_ref()
            .is_some_and(|root| same_path(root, &source))
        {
            bail!("Refusing to trash workspace root");
        }
        let source_is_dir = std::fs::symlink_metadata(&source)
            .with_context(|| format!("inspecting {}", source.display()))?
            .file_type()
            .is_dir();
        let affected_ids = self
            .buffers
            .iter()
            .filter(|buffer| {
                buffer.source.path().is_some_and(|path| {
                    path == &source || (source_is_dir && path.strip_prefix(&source).is_ok())
                })
            })
            .map(|buffer| buffer.id)
            .collect::<Vec<_>>();
        if let Some(buffer) = self
            .buffers
            .iter()
            .find(|buffer| affected_ids.contains(&buffer.id) && buffer.is_dirty())
        {
            bail!(
                "{} has unsaved changes; close or discard it before moving it to trash",
                buffer.display_title()
            );
        }

        let trashed = texti_fs::move_to_trash(&source)?;
        self.buffers
            .retain(|buffer| !affected_ids.contains(&buffer.id));
        for buffer_id in &affected_ids {
            self.view_states.remove(buffer_id);
            self.syntax_cache.get_mut().remove(buffer_id);
            let _ = self.settings_store.remove_recovery(*buffer_id);
        }
        self.autosave_records
            .retain(|record| !affected_ids.contains(&record.buffer_id));
        if self.active.is_some_and(|id| affected_ids.contains(&id)) {
            self.active = self.buffers.last().map(|buffer| buffer.id);
        }
        if self.buffers.is_empty() {
            self.new_untitled();
        }
        self.selected_path = self.workspace_root.clone();
        self.refresh_explorer_cache();
        self.last_message = format!("Moved to trash: {}", trashed.display());
        Ok(())
    }

    pub fn toggle_hidden_files(&mut self) -> Result<()> {
        self.settings.show_hidden_files = !self.settings.show_hidden_files;
        self.settings_store.save(&self.settings)?;
        self.refresh_explorer_cache();
        self.last_message = if self.settings.show_hidden_files {
            "Hidden files visible".to_string()
        } else {
            "Hidden files hidden".to_string()
        };
        Ok(())
    }

    pub fn set_font_size(&mut self, font_size: u32) -> Result<()> {
        self.settings.font_size = font_size.clamp(12, 22);
        self.save_settings(format!("Font size: {}", self.settings.font_size))
    }

    pub fn set_tab_size(&mut self, tab_size: u8) -> Result<()> {
        if !matches!(tab_size, 2 | 4 | 8) {
            bail!("Tab size must be 2, 4, or 8");
        }
        self.settings.tab_size = tab_size;
        self.save_settings(format!("Tab size: {tab_size}"))
    }

    pub fn set_insert_spaces(&mut self, insert_spaces: bool) -> Result<()> {
        self.settings.insert_spaces = insert_spaces;
        self.save_settings(if insert_spaces {
            "Indentation: spaces"
        } else {
            "Indentation: tabs"
        })
    }

    pub fn set_command_palette_visibility(&mut self, id: &str, visible: bool) -> Result<()> {
        if visible {
            self.settings.command_palette.hidden_commands.remove(id);
        } else {
            self.settings
                .command_palette
                .hidden_commands
                .insert(id.to_string());
        }
        self.save_settings(format!(
            "Command palette entry {}",
            if visible { "shown" } else { "hidden" }
        ))
    }

    pub fn set_command_shortcut_override(
        &mut self,
        id: &str,
        shortcut: Option<String>,
    ) -> Result<()> {
        self.settings
            .command_palette
            .shortcut_overrides
            .insert(id.to_string(), shortcut);
        self.save_settings("Command shortcut updated")
    }

    pub fn reset_command_customizations(&mut self) -> Result<()> {
        self.settings.command_palette.hidden_commands.clear();
        self.settings.command_palette.shortcut_overrides.clear();
        self.save_settings("Command Palette defaults restored")
    }

    pub fn toggle_line_numbers(&mut self) -> Result<()> {
        self.settings.show_line_numbers = !self.settings.show_line_numbers;
        self.save_settings("Line numbers updated")
    }

    pub fn toggle_word_wrap(&mut self) -> Result<()> {
        self.settings.word_wrap = !self.settings.word_wrap;
        self.save_settings("Word wrap updated")
    }

    pub fn toggle_minimap(&mut self) -> Result<()> {
        self.settings.show_minimap = !self.settings.show_minimap;
        self.save_settings("Minimap updated")
    }

    pub fn toggle_whitespace(&mut self) -> Result<()> {
        self.settings.show_whitespace = !self.settings.show_whitespace;
        self.save_settings("Whitespace display updated")
    }

    pub fn toggle_status_hints(&mut self) -> Result<()> {
        self.settings.status_hints = !self.settings.status_hints;
        self.save_settings("Status hints updated")
    }

    pub fn toggle_syntax_highlighting(&mut self) -> Result<()> {
        self.settings.syntax_highlighting = true;
        self.settings.default_syntax_mode = SyntaxMode::AutoDetect;
        self.save_settings("Syntax highlighting is always automatic")
    }

    pub fn toggle_recovery_autosave(&mut self) -> Result<()> {
        self.settings.recovery_autosave = !self.settings.recovery_autosave;
        self.save_settings("Recovery autosave updated")?;
        if !self.settings.recovery_autosave {
            self.settings_store.cleanup_recovery_except(&[])?;
            self.autosave_records.clear();
        }
        Ok(())
    }

    pub fn toggle_confirm_trash(&mut self) -> Result<()> {
        self.settings.confirm_trash = !self.settings.confirm_trash;
        self.save_settings("Trash confirmation updated")
    }

    pub fn set_syntax_mode(&mut self, mode: SyntaxMode) -> Result<()> {
        let _ = mode;
        self.settings.default_syntax_mode = SyntaxMode::AutoDetect;
        self.settings.syntax_highlighting = true;
        self.save_settings("Syntax mode: Auto Detect")
    }

    pub fn search_active(&mut self, query: &str) -> Result<Vec<String>> {
        let matches = self.search_active_structured(query)?;
        self.last_message = format!("{} matches for '{}'", matches.len(), query);
        Ok(matches
            .into_iter()
            .map(|m| format!("{}:{} {}", m.line_number, m.column_number, m.preview))
            .collect())
    }

    pub fn search_active_structured(&mut self, query: &str) -> Result<Vec<SearchResult>> {
        let matches = self
            .active_buffer()?
            .search(query, 50)
            .into_iter()
            .map(search_result)
            .collect::<Vec<_>>();
        self.last_message = format!("{} matches for '{}'", matches.len(), query);
        Ok(matches)
    }

    pub fn replace_active_next(
        &mut self,
        query: &str,
        replacement: &str,
    ) -> Result<Option<SearchResult>> {
        let found = self
            .active_buffer_mut()?
            .replace_first(query, replacement)?
            .map(search_result);
        self.last_message = if let Some(found) = &found {
            format!(
                "Replaced next match at line {}, column {}",
                found.line_number, found.column_number
            )
        } else {
            format!("No matches for '{query}'")
        };
        Ok(found)
    }

    pub fn replace_active_all(&mut self, query: &str, replacement: &str) -> Result<usize> {
        let count = self.active_buffer_mut()?.replace_all(query, replacement)?;
        self.last_message = format!("Replaced {count} matches");
        Ok(count)
    }

    pub fn go_to_line_active(&mut self, line_number: usize) -> Result<Option<usize>> {
        let offset = self.active_buffer()?.line_start_byte(line_number);
        self.last_message = if offset.is_some() {
            format!("Line {line_number}")
        } else {
            format!("Line {line_number} is outside this document")
        };
        Ok(offset)
    }

    pub fn workspace_matches(&self, query: &str, limit: usize) -> Vec<ExplorerRow> {
        let Some(root) = &self.workspace_root else {
            return Vec::new();
        };
        let needle = query.trim().to_lowercase();
        self.explorer_rows_cache
            .iter()
            .filter(|row| {
                if needle.is_empty() {
                    return true;
                }
                let relative = row
                    .path
                    .strip_prefix(root)
                    .map(Path::display)
                    .map(|display| display.to_string())
                    .unwrap_or_else(|_| row.name.clone());
                relative.to_lowercase().contains(&needle)
            })
            .take(limit)
            .cloned()
            .collect()
    }

    pub fn recent_files(&self) -> Vec<RecentFile> {
        self.settings
            .recent_files
            .iter()
            .filter(|path| path.exists())
            .map(|path| RecentFile {
                path: path.clone(),
                display: path.display().to_string(),
            })
            .collect()
    }

    pub fn autosave_active(&mut self) -> Result<Option<AutosaveRecord>> {
        if !self.settings.recovery_autosave {
            return Ok(None);
        }
        let id = self.active_buffer()?.id;
        self.autosave_buffer(id)
    }

    pub fn autosave_dirty_buffers(&mut self) -> Result<Vec<AutosaveRecord>> {
        if !self.settings.recovery_autosave {
            return Ok(Vec::new());
        }
        let dirty_ids = self
            .buffers
            .iter()
            .filter(|buffer| buffer.is_dirty())
            .map(|buffer| buffer.id)
            .collect::<Vec<_>>();
        let mut records = Vec::with_capacity(dirty_ids.len());
        for id in dirty_ids {
            if let Some(record) = self.autosave_buffer(id)? {
                records.push(record);
            }
        }
        Ok(records)
    }

    pub fn set_buffer_view_state(
        &mut self,
        buffer_id: BufferId,
        mut view_state: ViewState,
    ) -> Result<()> {
        self.buffer(buffer_id)?;
        if !view_state.viewport_x.is_finite() {
            view_state.viewport_x = 0.0;
        }
        if !view_state.viewport_y.is_finite() {
            view_state.viewport_y = 0.0;
        }
        self.view_states.insert(buffer_id, view_state);
        Ok(())
    }

    pub fn buffer_view_state(&self, buffer_id: BufferId) -> Result<ViewState> {
        self.buffer(buffer_id)?;
        Ok(self
            .view_states
            .get(&buffer_id)
            .cloned()
            .unwrap_or_default())
    }

    pub fn persist_session_with_view_states(
        &mut self,
        view_states: &[(BufferId, ViewState)],
    ) -> Result<()> {
        for (buffer_id, view_state) in view_states {
            if self.buffers.iter().any(|buffer| buffer.id == *buffer_id) {
                self.set_buffer_view_state(*buffer_id, view_state.clone())?;
            }
        }
        self.persist_session()
    }

    pub fn persist_session(&mut self) -> Result<()> {
        let records = self.autosave_dirty_buffers()?;
        let recovery_paths = records
            .iter()
            .map(|record| (record.buffer_id, record.recovery_path.clone()))
            .collect::<HashMap<_, _>>();
        let documents = self
            .buffers
            .iter()
            .map(|buffer| SessionDocument {
                buffer_id: buffer.id,
                source: buffer.source.clone(),
                encoding: buffer.encoding,
                line_ending: buffer.line_ending,
                revision: buffer.revision,
                last_saved_revision: buffer.last_saved_revision,
                dirty: buffer.is_dirty(),
                recovery_path: recovery_paths.get(&buffer.id).cloned().or_else(|| {
                    (self.settings.recovery_autosave && buffer.is_dirty())
                        .then(|| self.settings_store.paths().recovery_file(buffer.id))
                        .filter(|path| path.exists())
                }),
                fingerprint: fingerprint(buffer.last_known_modified, buffer.last_known_len),
                view_state: self
                    .view_states
                    .get(&buffer.id)
                    .cloned()
                    .unwrap_or_default(),
            })
            .collect();
        let manifest = SessionManifest {
            active_buffer_id: self.active,
            workspace_root: self.workspace_root.clone(),
            selected_path: self.selected_path.clone(),
            documents,
            ..SessionManifest::default()
        };
        self.settings_store.save_session(&manifest)?;
        let retained = manifest
            .documents
            .iter()
            .filter_map(|document| document.recovery_path.clone())
            .collect::<Vec<_>>();
        self.settings_store.cleanup_recovery_except(&retained)?;
        Ok(())
    }

    fn autosave_buffer(&mut self, buffer_id: BufferId) -> Result<Option<AutosaveRecord>> {
        let buffer = self.buffer(buffer_id)?;
        if !buffer.is_dirty() {
            return Ok(None);
        }
        if let Some(record) = self
            .autosave_records
            .iter()
            .find(|record| {
                record.buffer_id == buffer_id
                    && record.revision == buffer.revision
                    && record.recovery_path.is_file()
            })
            .cloned()
        {
            return Ok(Some(record));
        }
        let record = self.settings_store.write_recovery(
            buffer.id,
            &buffer.source,
            buffer.revision,
            &buffer.text(),
        )?;
        self.autosave_records
            .retain(|existing| existing.buffer_id != buffer_id);
        self.autosave_records.push(record.clone());
        Ok(Some(record))
    }

    pub fn check_active_external_change(&mut self) -> Result<bool> {
        let id = self.active_buffer()?.id;
        let Some(conflict) = self.external_conflict_for_buffer(id)? else {
            return Ok(false);
        };
        self.buffer_mut(id)?.save_state = SaveState::ExternalChangeDetected;
        self.last_message = conflict.message;
        Ok(true)
    }

    pub fn check_external_changes(&mut self) -> Result<Vec<FileConflict>> {
        let buffer_ids = self
            .buffers
            .iter()
            .map(|buffer| buffer.id)
            .collect::<Vec<_>>();
        let mut conflicts = Vec::new();
        let mut reloaded = 0usize;
        for buffer_id in buffer_ids {
            let Some(conflict) = self.external_conflict_for_buffer(buffer_id)? else {
                continue;
            };
            if self.buffer(buffer_id)?.is_dirty() {
                self.buffer_mut(buffer_id)?.save_state = SaveState::ExternalChangeDetected;
                conflicts.push(conflict);
            } else if conflict.path.is_file() {
                match self.reload_buffer_from_disk(buffer_id) {
                    Ok(_) => reloaded += 1,
                    Err(_) => {
                        self.buffer_mut(buffer_id)?.save_state = SaveState::ExternalChangeDetected;
                        conflicts.push(conflict);
                    }
                }
            } else {
                self.buffer_mut(buffer_id)?.save_state = SaveState::ExternalChangeDetected;
                conflicts.push(conflict);
            }
        }
        self.last_message = if conflicts.is_empty() {
            match reloaded {
                0 => "Files are current".to_string(),
                1 => "Reloaded one externally changed file".to_string(),
                count => format!("Reloaded {count} externally changed files"),
            }
        } else {
            format!("{} file conflicts require a decision", conflicts.len())
        };
        Ok(conflicts)
    }

    pub fn snapshot(&self) -> AppSnapshot {
        let active_buffer = self
            .active
            .and_then(|id| self.buffers.iter().find(|buffer| buffer.id == id));
        let active_buffer_id = active_buffer.map(|buffer| buffer.id);
        let active_revision = active_buffer
            .map(|buffer| buffer.revision)
            .unwrap_or_default();
        let active_line_ending = active_buffer
            .map(|buffer| buffer.line_ending)
            .unwrap_or_default();
        let editor_text = active_buffer.map(Buffer::text).unwrap_or_default();
        let tabs = self
            .buffers
            .iter()
            .map(|buffer| TabInfo {
                id: buffer.id,
                title: buffer.display_title(),
                path: buffer.source.path().cloned(),
                dirty: buffer.is_dirty(),
                active: Some(buffer.id) == self.active,
                readonly: buffer.is_readonly(),
            })
            .collect::<Vec<_>>();
        let explorer_rows = self.explorer_rows_cache.clone();
        let (status, syntax_spans) = active_buffer
            .map(|buffer| {
                let path = buffer.source.path().map(PathBuf::as_path);
                let language = SyntaxService::detect(path, &editor_text);
                let spans = self.syntax_spans(buffer, &editor_text);
                let highlight_note = if language.is_plain_text() {
                    "plain text".to_string()
                } else if spans.is_empty() {
                    "plain render".to_string()
                } else {
                    "highlighted".to_string()
                };
                let mut status_parts = vec![
                    language.label().to_string(),
                    highlight_note,
                    buffer.encoding.label().to_string(),
                    format!("Ln {}, Col {}", 1, 1),
                    buffer.save_state.label(),
                ];
                if !matches!(buffer.large_file_mode, texti_model::LargeFileMode::Normal) {
                    status_parts.push(format!("Large file: {}", buffer.large_file_mode.label()));
                }
                (
                    StatusInfo {
                        text: status_parts.join(" · "),
                        encoding: buffer.encoding,
                        language: Some(language.label().to_string()),
                        large_file_mode: buffer.large_file_mode,
                        save_state: buffer.save_state.clone(),
                        cursor_line: 1,
                        cursor_col: 1,
                    },
                    spans,
                )
            })
            .unwrap_or_else(|| {
                (
                    StatusInfo {
                        text: "No document".to_string(),
                        encoding: TextEncoding::Utf8,
                        language: None,
                        large_file_mode: texti_model::LargeFileMode::Normal,
                        save_state: SaveState::Clean,
                        cursor_line: 1,
                        cursor_col: 1,
                    },
                    Vec::new(),
                )
            });
        let window_title = active_buffer
            .map(|buffer| {
                let dirty = if buffer.is_dirty() { " *" } else { "" };
                format!("Texti - {}{}", buffer.display_title(), dirty)
            })
            .unwrap_or_else(|| "Texti".to_string());
        AppSnapshot {
            active_buffer_id,
            active_revision,
            active_line_ending,
            editor_text,
            tabs,
            explorer_rows,
            recent_files: self.recent_files(),
            status,
            window_title,
            workspace_root: self.workspace_root.clone(),
            selected_path: self.selected_path.clone(),
            syntax_spans,
            settings: self.settings.clone(),
            active_view_state: active_buffer_id
                .and_then(|id| self.view_states.get(&id).cloned())
                .unwrap_or_default(),
            message: self.last_message.clone(),
        }
    }

    fn syntax_spans(&self, buffer: &Buffer, text: &str) -> Vec<SyntaxSpan> {
        if !self.settings.syntax_highlighting
            || buffer.is_readonly()
            || !matches!(
                buffer.large_file_mode,
                LargeFileMode::Normal | LargeFileMode::Opportunistic
            )
        {
            self.syntax_cache.borrow_mut().remove(&buffer.id);
            return Vec::new();
        }

        let theme = HighlightTheme::Dark;
        let key = SyntaxCacheKey {
            revision: buffer.revision,
            theme,
            path: buffer.source.path().cloned(),
            large_file_mode: buffer.large_file_mode,
        };
        if let Some(entry) = self.syntax_cache.borrow().get(&buffer.id)
            && entry.key == key
        {
            return entry.spans.clone();
        }

        let max_bytes = match buffer.large_file_mode {
            LargeFileMode::Normal => text.len(),
            LargeFileMode::Opportunistic => 512 * 1024,
            LargeFileMode::Degraded | LargeFileMode::ReadOnlyPreview => 0,
        };
        let spans = SyntaxService::highlight(
            buffer.source.path().map(PathBuf::as_path),
            text,
            max_bytes,
            theme,
        );
        self.syntax_cache.borrow_mut().insert(
            buffer.id,
            SyntaxCacheEntry {
                key,
                spans: spans.clone(),
            },
        );
        spans
    }

    fn restore_session_manifest(&mut self, manifest: SessionManifest) -> Result<()> {
        self.workspace_root = manifest
            .workspace_root
            .as_deref()
            .filter(|path| path.is_dir())
            .and_then(|path| std::fs::canonicalize(path).ok());
        self.selected_path = manifest
            .selected_path
            .filter(|path| path.exists())
            .map(|path| std::fs::canonicalize(&path).unwrap_or(path));

        let requested_active = manifest.active_buffer_id;
        let mut skipped = 0usize;
        let mut retained_recovery = Vec::new();
        for document in manifest.documents {
            if document.buffer_id == 0
                || self
                    .buffers
                    .iter()
                    .any(|buffer| buffer.id == document.buffer_id)
            {
                skipped += 1;
                continue;
            }
            let Some(buffer) = self.restore_session_document(&document)? else {
                skipped += 1;
                continue;
            };
            if let DocumentSource::File(path) = &buffer.source
                && self.buffers.iter().any(|open| {
                    matches!(&open.source, DocumentSource::File(open_path) if same_path(open_path, path))
                })
            {
                skipped += 1;
                continue;
            }
            if let Some(path) = document.recovery_path.filter(|_| document.dirty) {
                retained_recovery.push(path.clone());
                self.autosave_records.push(AutosaveRecord {
                    id: uuid::Uuid::new_v4(),
                    buffer_id: buffer.id,
                    source: buffer.source.clone(),
                    recovery_path: path,
                    revision: buffer.revision,
                    saved_at: Utc::now(),
                });
            }
            self.view_states
                .insert(buffer.id, normalized_view_state(document.view_state));
            self.next_buffer_id = self.next_buffer_id.max(buffer.id.saturating_add(1));
            self.buffers.push(buffer);
        }
        self.active = requested_active
            .filter(|id| self.buffers.iter().any(|buffer| buffer.id == *id))
            .or_else(|| self.buffers.first().map(|buffer| buffer.id));
        let _ = self
            .settings_store
            .cleanup_recovery_except(&retained_recovery);
        if !self.buffers.is_empty() {
            self.last_message = if skipped == 0 {
                format!("Restored {} documents", self.buffers.len())
            } else {
                format!(
                    "Restored {} documents; skipped {skipped} unavailable entries",
                    self.buffers.len()
                )
            };
        }
        Ok(())
    }

    fn restore_session_document(&self, document: &SessionDocument) -> Result<Option<Buffer>> {
        if document.dirty
            && let Some(recovery_path) = &document.recovery_path
            && recovery_path.is_file()
        {
            let text = self.settings_store.read_recovery(recovery_path)?;
            let source = canonical_document_source(&document.source);
            let mut buffer = Buffer::from_text(
                document.buffer_id,
                source,
                text,
                document.encoding,
                document
                    .fingerprint
                    .modified_at
                    .map(std::time::SystemTime::from),
                document.fingerprint.len,
            );
            buffer.line_ending = document.line_ending;
            buffer.revision = document.revision.max(1);
            buffer.last_saved_revision = document
                .last_saved_revision
                .min(buffer.revision.saturating_sub(1));
            buffer.save_state = SaveState::Dirty;
            return Ok(Some(buffer));
        }

        match &document.source {
            DocumentSource::File(path) | DocumentSource::Recovery(path) => {
                let path = match std::fs::canonicalize(path) {
                    Ok(path) => path,
                    Err(_) => return Ok(None),
                };
                let snapshot = match texti_fs::read_file(&path) {
                    Ok(snapshot) => snapshot,
                    Err(_) => return Ok(None),
                };
                let source = match document.source {
                    DocumentSource::Recovery(_) => DocumentSource::Recovery(path),
                    _ => DocumentSource::File(path),
                };
                let opened = texti_editor::Buffer::from_bytes(
                    document.buffer_id,
                    source,
                    &snapshot.bytes,
                    snapshot.modified,
                    Some(snapshot.len),
                    snapshot.extent,
                )?;
                Ok(Some(opened.buffer))
            }
            DocumentSource::Untitled { .. } => {
                let mut buffer = Buffer::from_text(
                    document.buffer_id,
                    document.source.clone(),
                    "",
                    document.encoding,
                    None,
                    None,
                );
                buffer.line_ending = document.line_ending;
                Ok(Some(buffer))
            }
        }
    }

    pub fn save_buffer_checked(&mut self, buffer_id: BufferId) -> Result<SaveOutcome> {
        let buffer = self.buffer(buffer_id)?;
        if buffer.is_readonly() {
            bail!("Save As is disabled for readonly preview buffers");
        }
        let Some(path) = buffer.source.path().cloned() else {
            return Ok(SaveOutcome::SaveAsRequired { buffer_id });
        };
        if let Some(conflict) = self.external_conflict_for_buffer(buffer_id)? {
            self.buffer_mut(buffer_id)?.save_state = SaveState::ExternalChangeDetected;
            self.last_message = conflict.message.clone();
            return Ok(SaveOutcome::Conflict(conflict));
        }
        self.save_buffer_as_unchecked(buffer_id, path)
    }

    fn save_buffer_as_unchecked(
        &mut self,
        buffer_id: BufferId,
        path: PathBuf,
    ) -> Result<SaveOutcome> {
        if self.buffer(buffer_id)?.is_readonly() {
            bail!("Save As is disabled for readonly preview buffers");
        }
        if self.buffers.iter().any(|buffer| {
            buffer.id != buffer_id
                && buffer
                    .source
                    .path()
                    .is_some_and(|open_path| same_path(open_path, &path))
        }) {
            bail!("{} is already open in another tab", path.display());
        }
        let bytes = self.buffer(buffer_id)?.content_bytes();
        self.buffer_mut(buffer_id)?.save_state = SaveState::Saving;
        match texti_fs::atomic_save(&path, &bytes) {
            Ok(written) => {
                let path = std::fs::canonicalize(&path).unwrap_or(path);
                self.buffer_mut(buffer_id)?.mark_saved(
                    Some(DocumentSource::File(path.clone())),
                    written.modified,
                    Some(written.len),
                );
                self.settings_store.remove_recovery(buffer_id)?;
                self.autosave_records
                    .retain(|record| record.buffer_id != buffer_id);
                if self.active == Some(buffer_id) {
                    self.selected_path = Some(path.clone());
                    self.refresh_explorer_cache();
                }
                self.remember_recent_file(&path)?;
                self.last_message = format!("Saved {}", path.display());
                Ok(SaveOutcome::Saved { path })
            }
            Err(error) => {
                self.buffer_mut(buffer_id)?.save_state = SaveState::SaveFailed(error.to_string());
                Err(error).with_context(|| format!("saving {}", path.display()))
            }
        }
    }

    fn reload_buffer_from_disk(&mut self, buffer_id: BufferId) -> Result<PathBuf> {
        let source = self.buffer(buffer_id)?.source.clone();
        let next_revision = self.buffer(buffer_id)?.revision.saturating_add(1);
        let Some(path) = source.path().cloned() else {
            bail!("Untitled documents cannot reload from disk");
        };
        let path = std::fs::canonicalize(&path).unwrap_or(path);
        let snapshot = texti_fs::read_file(&path)?;
        let mut opened = texti_editor::Buffer::from_bytes(
            buffer_id,
            DocumentSource::File(path.clone()),
            &snapshot.bytes,
            snapshot.modified,
            Some(snapshot.len),
            snapshot.extent,
        )?;
        opened.buffer.revision = next_revision;
        opened.buffer.last_saved_revision = next_revision;
        *self.buffer_mut(buffer_id)? = opened.buffer;
        self.syntax_cache.get_mut().remove(&buffer_id);
        self.settings_store.remove_recovery(buffer_id)?;
        self.autosave_records
            .retain(|record| record.buffer_id != buffer_id);
        self.last_message = format!("Reloaded {}", path.display());
        Ok(path)
    }

    fn external_conflict_for_buffer(&self, buffer_id: BufferId) -> Result<Option<FileConflict>> {
        let buffer = self.buffer(buffer_id)?;
        let DocumentSource::File(path) = &buffer.source else {
            return Ok(None);
        };
        if matches!(buffer.save_state, SaveState::ExternalChangeDetected) {
            return Ok(Some(file_conflict(buffer_id, path)));
        }
        let metadata = match std::fs::metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Some(FileConflict {
                    buffer_id,
                    path: path.clone(),
                    message: format!("{} was removed outside Texti", path.display()),
                }));
            }
            Err(error) => {
                return Ok(Some(FileConflict {
                    buffer_id,
                    path: path.clone(),
                    message: format!("Cannot verify {} before saving: {error}", path.display()),
                }));
            }
        };
        let modified = metadata.modified().ok();
        let changed = (buffer.last_known_modified.is_some()
            && modified.is_some()
            && buffer.last_known_modified != modified)
            || (buffer.last_known_len.is_some() && buffer.last_known_len != Some(metadata.len()));
        Ok(changed.then(|| file_conflict(buffer_id, path)))
    }

    fn remove_buffer(&mut self, buffer_id: BufferId) -> Result<()> {
        let index = self
            .buffers
            .iter()
            .position(|buffer| buffer.id == buffer_id)
            .context("Tab no longer exists")?;
        self.settings_store.remove_recovery(buffer_id)?;
        self.autosave_records
            .retain(|record| record.buffer_id != buffer_id);
        self.view_states.remove(&buffer_id);
        self.syntax_cache.get_mut().remove(&buffer_id);
        self.buffers.remove(index);
        if self.active == Some(buffer_id) {
            self.active = self
                .buffers
                .get(index)
                .or_else(|| {
                    index
                        .checked_sub(1)
                        .and_then(|index| self.buffers.get(index))
                })
                .map(|buffer| buffer.id);
        }
        if self.buffers.is_empty() {
            self.new_untitled();
        }
        self.last_message = "Closed tab".to_string();
        Ok(())
    }

    fn buffer(&self, buffer_id: BufferId) -> Result<&Buffer> {
        self.buffers
            .iter()
            .find(|buffer| buffer.id == buffer_id)
            .context("Tab no longer exists")
    }

    fn buffer_mut(&mut self, buffer_id: BufferId) -> Result<&mut Buffer> {
        self.buffers
            .iter_mut()
            .find(|buffer| buffer.id == buffer_id)
            .context("Tab no longer exists")
    }

    fn active_buffer(&self) -> Result<&Buffer> {
        let id = self.active.context("No active buffer")?;
        self.buffers
            .iter()
            .find(|buffer| buffer.id == id)
            .context("Active buffer missing")
    }

    fn active_buffer_mut(&mut self) -> Result<&mut Buffer> {
        let id = self.active.context("No active buffer")?;
        self.buffers
            .iter_mut()
            .find(|buffer| buffer.id == id)
            .context("Active buffer missing")
    }

    fn selected_parent(&self) -> Result<PathBuf> {
        let selected = self
            .selected_path
            .as_ref()
            .or(self.workspace_root.as_ref())
            .context("Open a folder before creating files")?;
        let metadata = std::fs::symlink_metadata(selected)
            .with_context(|| format!("inspecting {}", selected.display()))?;
        if metadata.is_dir() {
            Ok(selected.clone())
        } else {
            selected
                .parent()
                .map(Path::to_path_buf)
                .context("Selected file has no parent")
        }
    }

    fn allocate_id(&mut self) -> BufferId {
        let id = self.next_buffer_id;
        self.next_buffer_id += 1;
        id
    }

    fn blank_untitled_buffer_id(&self) -> Option<BufferId> {
        (self.buffers.len() == 1 && self.is_blank_untitled_at(0)).then_some(self.buffers[0].id)
    }

    fn replaceable_active_tab_index(&self) -> Option<usize> {
        let active = self.active?;
        let index = self.buffers.iter().position(|buffer| buffer.id == active)?;
        self.is_blank_untitled_at(index).then_some(index)
    }

    fn is_blank_untitled_at(&self, index: usize) -> bool {
        let Some(buffer) = self.buffers.get(index) else {
            return false;
        };
        matches!(buffer.source, DocumentSource::Untitled { .. })
            && !buffer.is_dirty()
            && buffer.text().is_empty()
    }

    fn remember_recent_file(&mut self, path: &Path) -> Result<()> {
        let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        self.settings
            .recent_files
            .retain(|existing| !same_path(existing, &path));
        self.settings.recent_files.insert(0, path);
        self.settings.recent_files.truncate(20);
        self.settings_store.save(&self.settings)?;
        Ok(())
    }

    fn save_settings(&mut self, message: impl Into<String>) -> Result<()> {
        self.settings_store.save(&self.settings)?;
        self.last_message = message.into();
        Ok(())
    }

    fn refresh_explorer_cache(&mut self) {
        self.explorer_rows_cache = self
            .workspace_root
            .as_ref()
            .and_then(|root| {
                texti_fs::list_tree(
                    root,
                    self.selected_path.as_deref(),
                    self.settings.show_hidden_files,
                    2_000,
                )
                .ok()
            })
            .unwrap_or_default();
    }
}

fn fingerprint(modified: Option<std::time::SystemTime>, len: Option<u64>) -> FileFingerprint {
    FileFingerprint {
        modified_at: modified.map(DateTime::<Utc>::from),
        len,
    }
}

fn normalized_view_state(mut view_state: ViewState) -> ViewState {
    if !view_state.viewport_x.is_finite() {
        view_state.viewport_x = 0.0;
    }
    if !view_state.viewport_y.is_finite() {
        view_state.viewport_y = 0.0;
    }
    view_state
}

fn canonical_document_source(source: &DocumentSource) -> DocumentSource {
    match source {
        DocumentSource::File(path) => {
            DocumentSource::File(std::fs::canonicalize(path).unwrap_or_else(|_| path.clone()))
        }
        _ => source.clone(),
    }
}

fn file_conflict(buffer_id: BufferId, path: &Path) -> FileConflict {
    FileConflict {
        buffer_id,
        path: path.to_path_buf(),
        message: format!(
            "{} changed outside Texti; reload, overwrite, or save elsewhere",
            path.display()
        ),
    }
}

fn same_path(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

fn search_result(m: texti_editor::SearchMatch) -> SearchResult {
    SearchResult {
        line_number: m.line_number,
        column_number: m.column_number,
        line_byte_start: m.byte_start,
        line_byte_end: m.byte_end,
        absolute_byte_start: m.absolute_byte_start,
        absolute_byte_end: m.absolute_byte_end,
        preview: m.preview,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use texti_settings::SettingsPaths;

    fn state_for(dir: &Path) -> AppState {
        let store = SettingsStore::new(SettingsPaths::from_root(&dir.join("settings")));
        AppState::new(store).unwrap()
    }

    #[test]
    fn creates_and_opens_file_in_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = state_for(dir.path());
        state.open_folder(dir.path()).unwrap();
        state.create_file_in_selection("note.txt").unwrap();
        state.update_active_text("hello".to_string()).unwrap();
        state.save_active().unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("note.txt")).unwrap(),
            "hello"
        );
    }

    #[test]
    fn detects_external_change() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        std::fs::write(&path, "a").unwrap();
        let mut state = state_for(dir.path());
        state.open_file(&path).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        std::fs::write(&path, "b").unwrap();
        assert!(state.check_active_external_change().unwrap());
    }

    #[test]
    fn autosaves_dirty_buffer() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = state_for(dir.path());
        state.update_active_text("draft".to_string()).unwrap();
        let record = state.autosave_active().unwrap().unwrap();
        assert!(record.recovery_path.exists());
    }

    #[test]
    fn renames_open_buffer_source() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        std::fs::write(&path, "a").unwrap();
        let mut state = state_for(dir.path());
        state.open_folder(dir.path()).unwrap();
        state.select_or_open(&path).unwrap();
        state.rename_selected("b.txt").unwrap();
        assert!(dir.path().join("b.txt").exists());
        assert_eq!(state.snapshot().tabs[0].title, "b.txt");
    }

    #[test]
    fn renaming_directory_remaps_open_descendant_buffers() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        let nested = source.join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        let old_file = nested.join("main.rs");
        std::fs::write(&old_file, "fn main() {}\n").unwrap();
        let mut state = state_for(dir.path());
        state.open_folder(dir.path()).unwrap();
        state.select_or_open(&old_file).unwrap();
        state
            .update_active_text("fn main() { println!(\"renamed\"); }\n".to_string())
            .unwrap();
        state.select_or_open(&source).unwrap();

        state.rename_selected("renamed").unwrap();

        let new_file = dir.path().join("renamed/nested/main.rs");
        let snapshot = state.snapshot();
        assert_eq!(snapshot.tabs[0].path.as_deref(), Some(new_file.as_path()));
        assert!(snapshot.tabs[0].dirty);
        state.save_active().unwrap();
        assert!(
            std::fs::read_to_string(new_file)
                .unwrap()
                .contains("renamed")
        );
    }

    #[test]
    fn trash_refuses_to_drop_dirty_open_buffer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("draft.txt");
        std::fs::write(&path, "saved").unwrap();
        let mut state = state_for(dir.path());
        state.open_folder(dir.path()).unwrap();
        state.select_or_open(&path).unwrap();
        state
            .update_active_text("unsaved work".to_string())
            .unwrap();

        let error = state.trash_selected_confirmed(true).unwrap_err();

        assert!(error.to_string().contains("unsaved changes"));
        assert!(path.exists());
        let snapshot = state.snapshot();
        assert_eq!(snapshot.editor_text, "unsaved work");
        assert!(snapshot.tabs[0].dirty);
    }

    #[test]
    fn open_file_reuses_active_blank_untitled_tab() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("note.txt");
        std::fs::write(&path, "note").unwrap();
        let mut state = state_for(dir.path());

        state.open_file(&path).unwrap();

        let snapshot = state.snapshot();
        assert_eq!(snapshot.tabs.len(), 1);
        assert_eq!(snapshot.editor_text, "note");
        assert_eq!(snapshot.tabs[0].path.as_deref(), Some(path.as_path()));
    }

    #[test]
    fn crlf_line_ending_survives_recovery_and_controls_new_lines() {
        let dir = tempfile::tempdir().unwrap();
        let settings_root = dir.path().join("settings");
        let path = dir.path().join("windows.txt");
        std::fs::write(&path, "first\r\nsecond\r\n").unwrap();
        let store = SettingsStore::new(SettingsPaths::from_root(&settings_root));
        let mut state = AppState::new(store).unwrap();
        state.open_file(&path).unwrap();
        assert_eq!(state.snapshot().active_line_ending, LineEnding::CrLf);

        state.update_active_text("single line".to_string()).unwrap();
        state.persist_session().unwrap();
        drop(state);

        let store = SettingsStore::new(SettingsPaths::from_root(&settings_root));
        let mut restored = AppState::new(store).unwrap();
        let snapshot = restored.snapshot();
        assert_eq!(snapshot.editor_text, "single line");
        assert_eq!(snapshot.active_line_ending, LineEnding::CrLf);

        let end = snapshot.editor_text.chars().count();
        restored
            .replace_active_range_chars(end, end, snapshot.active_line_ending.as_str())
            .unwrap();
        restored
            .replace_active_range_chars(end + 2, end + 2, "next")
            .unwrap();
        restored.save_active().unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"single line\r\nnext");
    }

    #[test]
    fn legacy_session_without_line_ending_defaults_to_lf() {
        let dir = tempfile::tempdir().unwrap();
        let settings_root = dir.path().join("settings");
        let paths = SettingsPaths::from_root(&settings_root);
        std::fs::create_dir_all(&paths.data_dir).unwrap();
        std::fs::write(
            paths.session_file(),
            r#"{
                "version": 1,
                "active_buffer_id": 7,
                "documents": [{
                    "buffer_id": 7,
                    "source": {"Untitled": {"name": "Legacy"}}
                }]
            }"#,
        )
        .unwrap();

        let state = AppState::new(SettingsStore::new(paths)).unwrap();
        let snapshot = state.snapshot();
        assert_eq!(snapshot.active_buffer_id, Some(7));
        assert_eq!(snapshot.active_line_ending, LineEnding::Lf);
    }

    #[test]
    fn open_file_from_dirty_untitled_creates_new_tab() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("note.txt");
        std::fs::write(&path, "note").unwrap();
        let mut state = state_for(dir.path());
        state.update_active_text("draft".to_string()).unwrap();

        state.open_file(&path).unwrap();

        let snapshot = state.snapshot();
        assert_eq!(snapshot.tabs.len(), 2);
        assert_eq!(snapshot.editor_text, "note");
        assert!(
            snapshot
                .tabs
                .iter()
                .any(|tab| tab.dirty && tab.path.is_none())
        );
        assert_eq!(
            snapshot
                .tabs
                .iter()
                .find(|tab| tab.active)
                .and_then(|tab| tab.path.as_deref()),
            Some(path.as_path())
        );
    }

    #[test]
    fn structured_search_reports_offsets() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = state_for(dir.path());
        state
            .update_active_text("alpha\nbeta alpha".to_string())
            .unwrap();
        let matches = state.search_active_structured("alpha").unwrap();
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[1].line_number, 2);
        assert_eq!(matches[1].column_number, 6);
        assert_eq!(matches[1].absolute_byte_start, 11);
    }

    #[test]
    fn replace_next_and_all_update_active_buffer() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = state_for(dir.path());
        state
            .update_active_text("one two one two".to_string())
            .unwrap();
        assert!(state.replace_active_next("one", "1").unwrap().is_some());
        assert_eq!(state.snapshot().editor_text, "1 two one two");
        assert_eq!(state.replace_active_all("two", "2").unwrap(), 2);
        assert_eq!(state.snapshot().editor_text, "1 2 one 2");
    }

    #[test]
    fn isolated_range_edit_keeps_its_own_undo_step() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = state_for(dir.path());
        state.replace_active_range_chars(0, 0, "a").unwrap();
        state
            .replace_active_range_chars_isolated(1, 1, "b")
            .unwrap();
        state.replace_active_range_chars(2, 2, "c").unwrap();

        state.undo_active().unwrap();
        assert_eq!(state.snapshot().editor_text, "ab");
        state.undo_active().unwrap();
        assert_eq!(state.snapshot().editor_text, "a");
    }

    #[test]
    fn go_to_line_returns_byte_offset() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = state_for(dir.path());
        state.update_active_text("aa\nbbb\nc".to_string()).unwrap();
        assert_eq!(state.go_to_line_active(2).unwrap(), Some(3));
        assert_eq!(state.go_to_line_active(9).unwrap(), None);
    }

    #[test]
    fn recent_files_track_opened_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("note.txt");
        std::fs::write(&path, "note").unwrap();
        let mut state = state_for(dir.path());
        state.open_file(&path).unwrap();
        let recent = state.recent_files();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].path, std::fs::canonicalize(path).unwrap());
    }

    #[test]
    fn startup_file_argument_opens_file_without_blank_untitled() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clicked.rs");
        std::fs::write(&path, "fn main() {}").unwrap();
        let mut state = state_for(dir.path());

        state.open_startup_paths(vec![path.clone()]).unwrap();

        let snapshot = state.snapshot();
        assert_eq!(snapshot.editor_text, "fn main() {}");
        assert_eq!(snapshot.tabs.len(), 1);
        assert_eq!(snapshot.tabs[0].path.as_deref(), Some(path.as_path()));
        assert_eq!(snapshot.status.language.as_deref(), Some("Rust"));
    }

    #[test]
    fn workspace_matches_filter_paths_and_hidden_setting() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "").unwrap();
        std::fs::write(dir.path().join(".secret"), "").unwrap();
        let mut state = state_for(dir.path());
        state.open_folder(dir.path()).unwrap();
        let cargo = state.workspace_matches("cargo", 20);
        assert_eq!(cargo.len(), 1);
        assert_eq!(cargo[0].name, "Cargo.toml");
        assert!(state.workspace_matches("secret", 20).is_empty());
        state.toggle_hidden_files().unwrap();
        assert_eq!(state.workspace_matches("secret", 20).len(), 1);
    }

    #[test]
    fn explorer_cache_refreshes_after_workspace_mutations() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".secret"), "hidden").unwrap();
        let mut state = state_for(dir.path());
        state.open_folder(dir.path()).unwrap();
        assert!(
            !state
                .snapshot()
                .explorer_rows
                .iter()
                .any(|row| row.name == ".secret")
        );

        state.toggle_hidden_files().unwrap();
        assert!(
            state
                .snapshot()
                .explorer_rows
                .iter()
                .any(|row| row.name == ".secret")
        );

        state.create_folder_in_selection("new-folder").unwrap();
        assert!(
            state
                .workspace_matches("new-folder", 10)
                .iter()
                .any(|row| row.name == "new-folder")
        );
    }

    #[test]
    fn syntax_cache_tracks_revision_in_dark_mode() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = state_for(dir.path());
        state
            .update_active_text("fn main() { println!(\"hello\"); }".to_string())
            .unwrap();
        let dark = state.snapshot();
        let buffer_id = dark.active_buffer_id.unwrap();
        assert!(!dark.syntax_spans.is_empty());
        {
            let cache = state.syntax_cache.borrow();
            let entry = cache.get(&buffer_id).unwrap();
            assert_eq!(entry.key.revision, dark.active_revision);
            assert_eq!(entry.key.theme, HighlightTheme::Dark);
        }

        let repeated = state.snapshot();
        assert_eq!(repeated.syntax_spans, dark.syntax_spans);

        state
            .replace_active_range_chars(
                dark.editor_text.chars().count(),
                dark.editor_text.chars().count(),
                "\n",
            )
            .unwrap();
        let edited = state.snapshot();
        assert!(edited.active_revision > dark.active_revision);
        assert_eq!(
            state
                .syntax_cache
                .borrow()
                .get(&buffer_id)
                .unwrap()
                .key
                .revision,
            edited.active_revision
        );
    }

    #[test]
    fn confirm_trash_gate_can_require_confirmation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old.txt");
        std::fs::write(&path, "old").unwrap();
        let mut state = state_for(dir.path());
        state.open_folder(dir.path()).unwrap();
        state.select_or_open(&path).unwrap();
        assert!(state.trash_selected_confirmed(false).is_err());
        state.trash_selected_confirmed(true).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn syntax_mode_setting_is_compatibility_only() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = state_for(dir.path());
        state
            .update_active_text("fn main() {}".to_string())
            .unwrap();
        state.set_syntax_mode(SyntaxMode::PlainText).unwrap();
        assert_eq!(state.snapshot().status.language.as_deref(), Some("Rust"));
        state.set_syntax_mode(SyntaxMode::Rust).unwrap();
        assert_eq!(
            state.snapshot().settings.default_syntax_mode,
            SyntaxMode::AutoDetect
        );
        assert_eq!(state.snapshot().status.language.as_deref(), Some("Rust"));
    }

    #[test]
    fn opening_same_canonical_file_activates_existing_tab() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("note.txt");
        std::fs::write(&path, "note").unwrap();
        let mut state = state_for(dir.path());

        let first = state.open_file(&path).unwrap();
        let second = state
            .open_file(dir.path().join(".").join("note.txt"))
            .unwrap();

        assert_eq!(first, second);
        assert_eq!(state.snapshot().tabs.len(), 1);
    }

    #[test]
    fn restores_dirty_session_and_per_buffer_view_state() {
        let dir = tempfile::tempdir().unwrap();
        let settings_root = dir.path().join("settings");
        let store = SettingsStore::new(SettingsPaths::from_root(&settings_root));
        let mut state = AppState::new(store).unwrap();
        let buffer_id = state.snapshot().active_buffer_id.unwrap();
        state.update_active_text("recover me".to_string()).unwrap();
        let view_state = ViewState {
            cursor_char: 7,
            anchor_char: 2,
            preferred_column: Some(4),
            viewport_x: 12.0,
            viewport_y: 36.0,
        };
        state
            .set_buffer_view_state(buffer_id, view_state.clone())
            .unwrap();
        state.persist_session().unwrap();
        drop(state);

        let store = SettingsStore::new(SettingsPaths::from_root(&settings_root));
        let restored = AppState::new(store).unwrap();
        let snapshot = restored.snapshot();
        assert_eq!(snapshot.editor_text, "recover me");
        assert!(snapshot.tabs[0].dirty);
        assert_eq!(snapshot.active_view_state, view_state);
        assert!(snapshot.message.starts_with("Restored 1 document"));
    }

    #[test]
    fn editing_waits_for_explicit_recovery_flush() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = state_for(dir.path());
        state.update_active_text("draft".to_string()).unwrap();
        let recovery_dir = dir.path().join("settings/data/recovery");
        assert!(!recovery_dir.exists());

        let records = state.autosave_dirty_buffers().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(
            std::fs::read_to_string(&records[0].recovery_path).unwrap(),
            "draft"
        );
    }

    #[test]
    fn dirty_close_requires_an_explicit_decision() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = state_for(dir.path());
        let buffer_id = state.snapshot().active_buffer_id.unwrap();
        state.update_active_text("draft".to_string()).unwrap();

        assert!(matches!(
            state.request_close_buffer(buffer_id).unwrap(),
            CloseOutcome::NeedsDecision {
                save_as_required: true,
                ..
            }
        ));
        assert_eq!(
            state
                .close_buffer_with_decision(buffer_id, CloseDecision::Cancel)
                .unwrap(),
            CloseOutcome::Cancelled { buffer_id }
        );
        assert_eq!(state.snapshot().editor_text, "draft");
        assert_eq!(
            state
                .close_buffer_with_decision(buffer_id, CloseDecision::Discard)
                .unwrap(),
            CloseOutcome::Closed { buffer_id }
        );
        assert_eq!(state.snapshot().editor_text, "");
    }

    #[test]
    fn checked_save_blocks_external_overwrite_until_resolved() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("note.txt");
        std::fs::write(&path, "base").unwrap();
        let mut state = state_for(dir.path());
        state.open_file(&path).unwrap();
        state.update_active_text("my draft".to_string()).unwrap();
        std::fs::write(&path, "external content").unwrap();

        assert!(matches!(
            state.save_active_checked().unwrap(),
            SaveOutcome::Conflict(_)
        ));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "external content");
        assert!(matches!(
            state
                .resolve_active_file_conflict(FileConflictDecision::Overwrite)
                .unwrap(),
            SaveOutcome::Saved { .. }
        ));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "my draft");
    }

    #[test]
    fn clean_external_change_reloads_automatically() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("note.txt");
        std::fs::write(&path, "before").unwrap();
        let mut state = state_for(dir.path());
        state.open_file(&path).unwrap();
        std::fs::write(&path, "after, with a different length").unwrap();

        let conflicts = state.check_external_changes().unwrap();
        assert!(conflicts.is_empty());
        assert_eq!(
            state.snapshot().editor_text,
            "after, with a different length"
        );
    }

    #[test]
    fn functional_preferences_validate_and_persist() {
        let dir = tempfile::tempdir().unwrap();
        let settings_root = dir.path().join("settings");
        let store = SettingsStore::new(SettingsPaths::from_root(&settings_root));
        let mut state = AppState::new(store).unwrap();
        state.set_font_size(100).unwrap();
        state.set_tab_size(2).unwrap();
        state.set_insert_spaces(false).unwrap();
        state.toggle_minimap().unwrap();
        state.toggle_whitespace().unwrap();
        state
            .set_command_palette_visibility("view.minimap", false)
            .unwrap();
        state
            .set_command_shortcut_override("file.save", Some("Ctrl+Alt+S".to_string()))
            .unwrap();
        assert!(state.set_tab_size(3).is_err());

        let store = SettingsStore::new(SettingsPaths::from_root(&settings_root));
        let loaded = store.load().unwrap();
        assert_eq!(loaded.theme, texti_model::ThemeMode::Dark);
        assert_eq!(loaded.font_size, 22);
        assert_eq!(loaded.tab_size, 2);
        assert!(!loaded.insert_spaces);
        assert!(loaded.show_minimap);
        assert!(loaded.show_whitespace);
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
    }
}
