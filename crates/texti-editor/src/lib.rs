use encoding_rs::WINDOWS_1252;
use ropey::Rope;
use std::path::Path;
use texti_model::{
    BufferId, DocumentSource, LargeFileMode, LineEnding, ReadExtent, SaveState, TextEncoding,
    ViewportRow, ViewportSnapshot,
};
use thiserror::Error;

const OPPORTUNISTIC_BYTES: usize = 4 * 1024 * 1024;
const DEGRADED_BYTES: usize = 64 * 1024 * 1024;
const READONLY_BYTES: usize = 256 * 1024 * 1024;
const READONLY_LINES: usize = 5_000_000;

#[derive(Debug, Error)]
pub enum EditorError {
    #[error("buffer is read-only: {0}")]
    ReadOnly(String),
    #[error("edit range is invalid")]
    InvalidRange,
    #[error("document is binary")]
    BinaryDocument,
    #[error("text decoding failed: {0}")]
    Decode(String),
}

#[derive(Clone, Debug)]
pub struct Buffer {
    pub id: BufferId,
    pub source: DocumentSource,
    pub encoding: TextEncoding,
    pub line_ending: LineEnding,
    rope: Rope,
    pub revision: u64,
    pub last_saved_revision: u64,
    pub large_file_mode: LargeFileMode,
    pub save_state: SaveState,
    undo: Vec<EditTransaction>,
    redo: Vec<EditTransaction>,
    coalesce_barrier: bool,
    pub readonly_reason: Option<String>,
    pub last_known_modified: Option<std::time::SystemTime>,
    pub last_known_len: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct EditTransaction {
    start_char: usize,
    deleted: String,
    inserted: String,
    kind: EditKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EditKind {
    Typing,
    Delete,
    Other,
}

#[derive(Clone, Debug)]
pub struct OpenedBuffer {
    pub buffer: Buffer,
    pub binary: bool,
    pub warning: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchMatch {
    pub line_number: usize,
    pub column_number: usize,
    pub byte_start: usize,
    pub byte_end: usize,
    pub absolute_byte_start: usize,
    pub absolute_byte_end: usize,
    pub preview: String,
}

#[derive(Clone, Debug)]
struct DecodedText {
    text: String,
    encoding: TextEncoding,
    readonly_reason: Option<String>,
    warning: Option<String>,
}

impl Buffer {
    pub fn new_untitled(id: BufferId) -> Self {
        Self::from_text(
            id,
            DocumentSource::Untitled {
                name: format!("Untitled-{id}"),
            },
            "",
            TextEncoding::Utf8,
            None,
            None,
        )
    }

    pub fn from_text(
        id: BufferId,
        source: DocumentSource,
        text: impl AsRef<str>,
        encoding: TextEncoding,
        modified: Option<std::time::SystemTime>,
        len: Option<u64>,
    ) -> Self {
        let text = text.as_ref();
        let large_file_mode = classify_large_file(text.len(), text.lines().count().max(1));
        Self {
            id,
            source,
            encoding,
            line_ending: detect_line_ending(text),
            rope: Rope::from_str(text),
            revision: 0,
            last_saved_revision: 0,
            large_file_mode,
            save_state: SaveState::Clean,
            undo: Vec::new(),
            redo: Vec::new(),
            coalesce_barrier: true,
            readonly_reason: None,
            last_known_modified: modified,
            last_known_len: len,
        }
    }

    pub fn from_bytes(
        id: BufferId,
        source: DocumentSource,
        bytes: &[u8],
        modified: Option<std::time::SystemTime>,
        len: Option<u64>,
        extent: ReadExtent,
    ) -> Result<OpenedBuffer, EditorError> {
        let total_len = len.unwrap_or(bytes.len() as u64);
        let total_bytes = total_len.min(usize::MAX as u64) as usize;
        if let Some(decoded) = decode_openable_text(bytes)? {
            let mut buffer =
                Self::from_text(id, source, decoded.text, decoded.encoding, modified, len);
            let classified = classify_large_file(total_bytes, buffer.line_count());
            buffer.large_file_mode = classified;

            let mut warning = decoded.warning;
            let mut readonly_reason = decoded.readonly_reason;
            if let ReadExtent::Preview {
                shown_bytes,
                total_bytes,
            } = extent
            {
                buffer.large_file_mode = LargeFileMode::ReadOnlyPreview;
                readonly_reason =
                    Some("file is above safe loading threshold; preview is truncated".to_string());
                warning = Some(format!(
                    "Huge file opened as readonly preview (showing {shown_bytes} of {total_bytes} bytes)."
                ));
            } else if matches!(classified, LargeFileMode::ReadOnlyPreview) {
                readonly_reason = Some("file is above editable threshold".to_string());
            }

            if let Some(reason) = readonly_reason {
                buffer.readonly_reason = Some(reason.clone());
                buffer.save_state = SaveState::ReadOnly(reason);
            }
            if warning.is_none() {
                warning = match buffer.large_file_mode {
                    LargeFileMode::Normal => None,
                    LargeFileMode::Opportunistic => Some(
                        "Large file: syntax and search are opportunistic and cancelable."
                            .to_string(),
                    ),
                    LargeFileMode::Degraded => {
                        Some("Large-file mode: expensive editor decorations are disabled.".to_string())
                    }
                    LargeFileMode::ReadOnlyPreview => Some(
                        "Huge file opened as readonly preview. Use a new document to save copied content."
                            .to_string(),
                    ),
                };
            }
            return Ok(OpenedBuffer {
                buffer,
                binary: false,
                warning,
            });
        }

        let preview = hex_preview(bytes);
        let mut buffer = Self::from_text(id, source, preview, TextEncoding::Binary, modified, len);
        buffer.large_file_mode = LargeFileMode::ReadOnlyPreview;
        buffer.readonly_reason = Some("binary file opened in safe byte preview".to_string());
        buffer.save_state = SaveState::ReadOnly("binary file".to_string());
        Ok(OpenedBuffer {
            buffer,
            binary: true,
            warning: Some("Binary file opened in readonly byte preview.".to_string()),
        })
    }

    pub fn text(&self) -> String {
        self.rope.to_string()
    }

    pub fn line_count(&self) -> usize {
        self.rope.len_lines().max(1)
    }

    pub fn is_dirty(&self) -> bool {
        self.revision != self.last_saved_revision || self.save_state.dirty()
    }

    pub fn is_readonly(&self) -> bool {
        self.readonly_reason.is_some() || matches!(self.save_state, SaveState::ReadOnly(_))
    }

    pub fn set_text(
        &mut self,
        new_text: impl Into<String>,
        description: &str,
    ) -> Result<(), EditorError> {
        self.ensure_editable()?;
        let new_text = new_text.into();
        let deleted = self.text();
        if deleted == new_text {
            return Ok(());
        }
        self.rope = Rope::from_str(&new_text);
        self.record_edit(0, deleted, new_text, EditKind::Other, description);
        Ok(())
    }

    pub fn insert(&mut self, char_idx: usize, text: &str) -> Result<(), EditorError> {
        self.ensure_editable()?;
        if char_idx > self.rope.len_chars() {
            return Err(EditorError::InvalidRange);
        }
        self.rope.insert(char_idx, text);
        let kind = if is_typing_insert(text) {
            EditKind::Typing
        } else {
            EditKind::Other
        };
        self.record_edit(char_idx, String::new(), text.to_string(), kind, "insert");
        Ok(())
    }

    pub fn delete_range(&mut self, start_char: usize, end_char: usize) -> Result<(), EditorError> {
        self.ensure_editable()?;
        if start_char > end_char || end_char > self.rope.len_chars() {
            return Err(EditorError::InvalidRange);
        }
        if start_char == end_char {
            return Ok(());
        }
        let deleted = self.rope.slice(start_char..end_char).to_string();
        self.rope.remove(start_char..end_char);
        let kind = if deleted.chars().count() == 1 {
            EditKind::Delete
        } else {
            EditKind::Other
        };
        self.record_edit(start_char, deleted, String::new(), kind, "delete");
        Ok(())
    }

    pub fn replace_range(
        &mut self,
        start_char: usize,
        end_char: usize,
        text: &str,
    ) -> Result<(), EditorError> {
        self.replace_range_with_kind(start_char, end_char, text, None)
    }

    pub fn replace_range_isolated(
        &mut self,
        start_char: usize,
        end_char: usize,
        text: &str,
    ) -> Result<(), EditorError> {
        self.replace_range_with_kind(start_char, end_char, text, Some(EditKind::Other))
    }

    fn replace_range_with_kind(
        &mut self,
        start_char: usize,
        end_char: usize,
        text: &str,
        requested_kind: Option<EditKind>,
    ) -> Result<(), EditorError> {
        self.ensure_editable()?;
        if start_char > end_char || end_char > self.rope.len_chars() {
            return Err(EditorError::InvalidRange);
        }
        if start_char == end_char && text.is_empty() {
            return Ok(());
        }
        let deleted = self.rope.slice(start_char..end_char).to_string();
        if deleted == text {
            return Ok(());
        }
        self.rope.remove(start_char..end_char);
        self.rope.insert(start_char, text);
        let kind = requested_kind.unwrap_or_else(|| {
            if deleted.is_empty() && is_typing_insert(text) {
                EditKind::Typing
            } else if text.is_empty() && deleted.chars().count() == 1 {
                EditKind::Delete
            } else {
                EditKind::Other
            }
        });
        self.record_edit(start_char, deleted, text.to_string(), kind, "edit");
        Ok(())
    }

    pub fn undo(&mut self) -> bool {
        if let Some(transaction) = self.undo.pop() {
            let inserted_chars = transaction.inserted.chars().count();
            self.rope.remove(
                transaction.start_char..transaction.start_char.saturating_add(inserted_chars),
            );
            self.rope
                .insert(transaction.start_char, &transaction.deleted);
            self.revision = self.revision.saturating_add(1);
            self.refresh_after_content_change();
            self.save_state = SaveState::Dirty;
            self.redo.push(transaction);
            self.coalesce_barrier = true;
            true
        } else {
            false
        }
    }

    pub fn redo(&mut self) -> bool {
        if let Some(transaction) = self.redo.pop() {
            let deleted_chars = transaction.deleted.chars().count();
            self.rope.remove(
                transaction.start_char..transaction.start_char.saturating_add(deleted_chars),
            );
            self.rope
                .insert(transaction.start_char, &transaction.inserted);
            self.revision = self.revision.saturating_add(1);
            self.refresh_after_content_change();
            self.save_state = SaveState::Dirty;
            self.undo.push(transaction);
            self.coalesce_barrier = true;
            true
        } else {
            false
        }
    }

    pub fn mark_saved(
        &mut self,
        source: Option<DocumentSource>,
        modified: Option<std::time::SystemTime>,
        len: Option<u64>,
    ) {
        if let Some(source) = source {
            self.source = source;
        }
        self.last_saved_revision = self.revision;
        self.save_state = SaveState::Clean;
        self.last_known_modified = modified;
        self.last_known_len = len;
        self.coalesce_barrier = true;
    }

    pub fn mark_external_change_if_needed(
        &mut self,
        modified: Option<std::time::SystemTime>,
        len: Option<u64>,
    ) -> bool {
        let changed = (self.last_known_modified.is_some()
            && modified.is_some()
            && self.last_known_modified != modified)
            || (self.last_known_len.is_some() && len.is_some() && self.last_known_len != len);
        if changed {
            self.save_state = SaveState::ExternalChangeDetected;
        }
        changed
    }

    pub fn content_bytes(&self) -> Vec<u8> {
        let text = self.text();
        match self.encoding {
            TextEncoding::Utf8 => text.into_bytes(),
            TextEncoding::Utf8Bom => {
                let mut bytes = vec![0xEF, 0xBB, 0xBF];
                bytes.extend_from_slice(text.as_bytes());
                bytes
            }
            TextEncoding::Utf16Le => {
                let mut bytes = vec![0xFF, 0xFE];
                for unit in text.encode_utf16() {
                    bytes.extend_from_slice(&unit.to_le_bytes());
                }
                bytes
            }
            TextEncoding::Utf16Be => {
                let mut bytes = vec![0xFE, 0xFF];
                for unit in text.encode_utf16() {
                    bytes.extend_from_slice(&unit.to_be_bytes());
                }
                bytes
            }
            TextEncoding::Utf16LePreview => {
                let mut bytes = vec![0xFF, 0xFE];
                for unit in text.encode_utf16() {
                    bytes.extend_from_slice(&unit.to_le_bytes());
                }
                bytes
            }
            TextEncoding::Utf16BePreview => {
                let mut bytes = vec![0xFE, 0xFF];
                for unit in text.encode_utf16() {
                    bytes.extend_from_slice(&unit.to_be_bytes());
                }
                bytes
            }
            TextEncoding::Windows1252Preview => {
                let (bytes, _, _) = WINDOWS_1252.encode(&text);
                bytes.into_owned()
            }
            TextEncoding::Binary => text.into_bytes(),
        }
    }

    pub fn viewport(&self, start_line: usize, max_lines: usize) -> ViewportSnapshot {
        let total_lines = self.line_count();
        let start_line = start_line.min(total_lines.saturating_sub(1));
        let end = (start_line + max_lines).min(total_lines);
        let mut rows = Vec::with_capacity(end.saturating_sub(start_line));
        for line_idx in start_line..end {
            let line = self.rope.line(line_idx).to_string();
            rows.push(ViewportRow {
                line_number: line_idx + 1,
                text: line.trim_end_matches(['\r', '\n']).to_string(),
                spans: Vec::new(),
            });
        }
        ViewportSnapshot {
            buffer_id: self.id,
            revision: self.revision,
            start_line,
            rows,
            total_lines,
        }
    }

    pub fn search(&self, query: &str, limit: usize) -> Vec<SearchMatch> {
        if query.is_empty() {
            return Vec::new();
        }
        let mut matches = Vec::new();
        for line_idx in 0..self.line_count() {
            let line = self.rope.line(line_idx).to_string();
            let line_byte_start = self.rope.line_to_byte(line_idx);
            let mut offset = 0;
            while let Some(pos) = line[offset..].find(query) {
                let byte_start = offset + pos;
                let byte_end = byte_start + query.len();
                matches.push(SearchMatch {
                    line_number: line_idx + 1,
                    column_number: line[..byte_start].chars().count() + 1,
                    byte_start,
                    byte_end,
                    absolute_byte_start: line_byte_start + byte_start,
                    absolute_byte_end: line_byte_start + byte_end,
                    preview: line.trim_end_matches(['\r', '\n']).to_string(),
                });
                if matches.len() >= limit {
                    return matches;
                }
                offset = byte_end;
            }
        }
        matches
    }

    pub fn replace_first(
        &mut self,
        query: &str,
        replacement: &str,
    ) -> Result<Option<SearchMatch>, EditorError> {
        self.ensure_editable()?;
        let Some(found) = self.search(query, 1).into_iter().next() else {
            return Ok(None);
        };
        let start_char = self.rope.byte_to_char(found.absolute_byte_start);
        let end_char = self.rope.byte_to_char(found.absolute_byte_end);
        self.replace_range(start_char, end_char, replacement)?;
        self.coalesce_barrier = true;
        Ok(Some(found))
    }

    pub fn replace_all(&mut self, query: &str, replacement: &str) -> Result<usize, EditorError> {
        self.ensure_editable()?;
        if query.is_empty() {
            return Ok(0);
        }
        let deleted = self.text();
        let count = deleted.matches(query).count();
        if count == 0 {
            return Ok(0);
        }
        let inserted = deleted.replace(query, replacement);
        self.rope = Rope::from_str(&inserted);
        self.record_edit(0, deleted, inserted, EditKind::Other, "replace all");
        Ok(count)
    }

    pub fn line_start_byte(&self, line_number: usize) -> Option<usize> {
        if line_number == 0 || line_number > self.line_count() {
            return None;
        }
        Some(self.rope.line_to_byte(line_number - 1))
    }

    pub fn display_title(&self) -> String {
        self.source.display_name()
    }

    fn ensure_editable(&self) -> Result<(), EditorError> {
        if let Some(reason) = &self.readonly_reason {
            return Err(EditorError::ReadOnly(reason.clone()));
        }
        if let SaveState::ReadOnly(reason) = &self.save_state {
            return Err(EditorError::ReadOnly(reason.clone()));
        }
        Ok(())
    }

    pub fn break_undo_group(&mut self) {
        self.coalesce_barrier = true;
    }

    fn record_edit(
        &mut self,
        start_char: usize,
        deleted: String,
        inserted: String,
        kind: EditKind,
        _description: &str,
    ) {
        self.revision = self.revision.saturating_add(1);
        self.refresh_after_content_change();
        self.save_state = SaveState::Dirty;
        self.redo.clear();
        let transaction = EditTransaction {
            start_char,
            deleted,
            inserted,
            kind,
        };
        let coalesced = !self.coalesce_barrier
            && self
                .undo
                .last_mut()
                .is_some_and(|previous| previous.try_coalesce(&transaction));
        if !coalesced {
            self.undo.push(transaction);
        }
        self.coalesce_barrier = !matches!(kind, EditKind::Typing | EditKind::Delete);
        const MAX_UNDO: usize = 200;
        if self.undo.len() > MAX_UNDO {
            let extra = self.undo.len() - MAX_UNDO;
            self.undo.drain(0..extra);
        }
    }

    fn refresh_after_content_change(&mut self) {
        self.large_file_mode = classify_large_file(self.rope.len_bytes(), self.rope.len_lines());
    }
}

impl EditTransaction {
    fn try_coalesce(&mut self, next: &Self) -> bool {
        match (self.kind, next.kind) {
            (EditKind::Typing, EditKind::Typing)
                if self.deleted.is_empty()
                    && next.deleted.is_empty()
                    && next.start_char
                        == self
                            .start_char
                            .saturating_add(self.inserted.chars().count()) =>
            {
                self.inserted.push_str(&next.inserted);
                true
            }
            (EditKind::Delete, EditKind::Delete)
                if self.inserted.is_empty() && next.inserted.is_empty() =>
            {
                let next_chars = next.deleted.chars().count();
                if next.start_char.saturating_add(next_chars) == self.start_char {
                    self.start_char = next.start_char;
                    self.deleted.insert_str(0, &next.deleted);
                    true
                } else if next.start_char == self.start_char {
                    self.deleted.push_str(&next.deleted);
                    true
                } else {
                    false
                }
            }
            _ => false,
        }
    }
}

fn is_typing_insert(text: &str) -> bool {
    let mut chars = text.chars();
    chars
        .next()
        .is_some_and(|ch| !matches!(ch, '\n' | '\r' | '\t') && !ch.is_control())
        && chars.next().is_none()
}

fn detect_line_ending(text: &str) -> LineEnding {
    text.find('\n')
        .filter(|index| text.as_bytes().get(index.saturating_sub(1)) == Some(&b'\r'))
        .map_or(LineEnding::Lf, |_| LineEnding::CrLf)
}

pub fn classify_large_file(bytes: usize, lines: usize) -> LargeFileMode {
    if bytes > READONLY_BYTES || lines > READONLY_LINES {
        LargeFileMode::ReadOnlyPreview
    } else if bytes >= DEGRADED_BYTES {
        LargeFileMode::Degraded
    } else if bytes >= OPPORTUNISTIC_BYTES {
        LargeFileMode::Opportunistic
    } else {
        LargeFileMode::Normal
    }
}

fn decode_openable_text(bytes: &[u8]) -> Result<Option<DecodedText>, EditorError> {
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return match std::str::from_utf8(&bytes[3..]) {
            Ok(text) => Ok(Some(DecodedText {
                text: text.to_string(),
                encoding: TextEncoding::Utf8Bom,
                readonly_reason: None,
                warning: None,
            })),
            Err(_) => Ok(Some(windows_1252_preview(&bytes[3..]))),
        };
    }
    if bytes.starts_with(&[0xFF, 0xFE]) {
        return Ok(Some(match decode_utf16(&bytes[2..], true) {
            Ok(text) => DecodedText {
                text,
                encoding: TextEncoding::Utf16Le,
                readonly_reason: None,
                warning: None,
            },
            Err(_) => utf16_preview(&bytes[2..], true),
        }));
    }
    if bytes.starts_with(&[0xFE, 0xFF]) {
        return Ok(Some(match decode_utf16(&bytes[2..], false) {
            Ok(text) => DecodedText {
                text,
                encoding: TextEncoding::Utf16Be,
                readonly_reason: None,
                warning: None,
            },
            Err(_) => utf16_preview(&bytes[2..], false),
        }));
    }
    if let Some(decoded) = decode_probable_utf16(bytes) {
        return Ok(Some(decoded));
    }
    if looks_binary(bytes) {
        return Ok(None);
    }
    if let Ok(text) = std::str::from_utf8(bytes) {
        return Ok(Some(DecodedText {
            text: text.to_string(),
            encoding: TextEncoding::Utf8,
            readonly_reason: None,
            warning: None,
        }));
    }
    Ok(Some(windows_1252_preview(bytes)))
}

pub fn decode_text(bytes: &[u8]) -> Result<(String, TextEncoding), EditorError> {
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        let text = std::str::from_utf8(&bytes[3..])
            .map_err(|err| EditorError::Decode(err.to_string()))?
            .to_string();
        return Ok((text, TextEncoding::Utf8Bom));
    }
    if bytes.starts_with(&[0xFF, 0xFE]) {
        return decode_utf16(&bytes[2..], true).map(|text| (text, TextEncoding::Utf16Le));
    }
    if bytes.starts_with(&[0xFE, 0xFF]) {
        return decode_utf16(&bytes[2..], false).map(|text| (text, TextEncoding::Utf16Be));
    }
    let text = std::str::from_utf8(bytes)
        .map_err(|err| EditorError::Decode(err.to_string()))?
        .to_string();
    Ok((text, TextEncoding::Utf8))
}

fn decode_probable_utf16(bytes: &[u8]) -> Option<DecodedText> {
    if bytes.len() < 8 {
        return None;
    }
    let sample_len = (bytes.len().min(4096) / 2) * 2;
    if sample_len < 8 {
        return None;
    }
    let sample = &bytes[..sample_len];
    let units = sample_len / 2;
    let even_nuls = sample.iter().step_by(2).filter(|byte| **byte == 0).count();
    let odd_nuls = sample
        .iter()
        .skip(1)
        .step_by(2)
        .filter(|byte| **byte == 0)
        .count();
    if odd_nuls * 100 / units >= 30 && even_nuls * 100 / units <= 5 {
        return Some(utf16_preview(bytes, true));
    }
    if even_nuls * 100 / units >= 30 && odd_nuls * 100 / units <= 5 {
        return Some(utf16_preview(bytes, false));
    }
    None
}

fn utf16_preview(bytes: &[u8], little_endian: bool) -> DecodedText {
    let mut units = Vec::with_capacity(bytes.len() / 2);
    for chunk in bytes.chunks_exact(2) {
        units.push(if little_endian {
            u16::from_le_bytes([chunk[0], chunk[1]])
        } else {
            u16::from_be_bytes([chunk[0], chunk[1]])
        });
    }
    DecodedText {
        text: String::from_utf16_lossy(&units),
        encoding: if little_endian {
            TextEncoding::Utf16LePreview
        } else {
            TextEncoding::Utf16BePreview
        },
        readonly_reason: Some("UTF-16 file opened as readonly text preview".to_string()),
        warning: Some("UTF-16 file opened as readonly text preview.".to_string()),
    }
}

fn windows_1252_preview(bytes: &[u8]) -> DecodedText {
    let (text, _, _) = WINDOWS_1252.decode(bytes);
    DecodedText {
        text: text.into_owned(),
        encoding: TextEncoding::Windows1252Preview,
        readonly_reason: Some("legacy text encoding opened as readonly preview".to_string()),
        warning: Some("File is not UTF-8; opened as readonly Windows-1252 preview.".to_string()),
    }
}

pub fn looks_binary(bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return false;
    }
    let sample = &bytes[..bytes.len().min(8192)];
    if sample.contains(&0) {
        return true;
    }
    let control_count = sample
        .iter()
        .filter(|byte| {
            let byte = **byte;
            byte < 0x09 || (byte > 0x0D && byte < 0x20)
        })
        .count();
    control_count * 100 / sample.len() > 30
}

pub fn path_language(path: &Path) -> Option<&'static str> {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
    {
        "rs" => Some("Rust"),
        "toml" => Some("TOML"),
        "json" => Some("JSON"),
        "md" | "markdown" => Some("Markdown"),
        "slint" => Some("Slint"),
        "txt" | "log" => Some("Plain Text"),
        _ => None,
    }
}

fn decode_utf16(bytes: &[u8], little_endian: bool) -> Result<String, EditorError> {
    if !bytes.len().is_multiple_of(2) {
        return Err(EditorError::Decode(
            "UTF-16 input has an odd number of bytes".to_string(),
        ));
    }
    let mut units = Vec::with_capacity(bytes.len() / 2);
    for chunk in bytes.chunks_exact(2) {
        units.push(if little_endian {
            u16::from_le_bytes([chunk[0], chunk[1]])
        } else {
            u16::from_be_bytes([chunk[0], chunk[1]])
        });
    }
    String::from_utf16(&units).map_err(|err| EditorError::Decode(err.to_string()))
}

fn hex_preview(bytes: &[u8]) -> String {
    let mut output = String::new();
    for (row, chunk) in bytes.chunks(16).take(512).enumerate() {
        output.push_str(&format!("{:08x}  ", row * 16));
        for index in 0..16 {
            if let Some(byte) = chunk.get(index) {
                output.push_str(&format!("{byte:02x} "));
            } else {
                output.push_str("   ");
            }
        }
        output.push(' ');
        for byte in chunk {
            let ch = if byte.is_ascii_graphic() || *byte == b' ' {
                *byte as char
            } else {
                '.'
            };
            output.push(ch);
        }
        output.push('\n');
    }
    if bytes.len() > 8192 {
        output.push_str("\n... preview truncated at 8 KiB ...\n");
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_binary_with_nul() {
        assert!(looks_binary(b"abc\0def"));
        assert!(!looks_binary(b"hello\nworld"));
    }

    #[test]
    fn detects_crlf_without_normalizing_mixed_content() {
        let original = "first\r\nsecond\nthird";
        let mut opened = Buffer::from_bytes(
            1,
            DocumentSource::Untitled {
                name: "mixed.txt".to_string(),
            },
            original.as_bytes(),
            None,
            Some(original.len() as u64),
            ReadExtent::Complete,
        )
        .unwrap();

        assert_eq!(opened.buffer.line_ending, LineEnding::CrLf);
        assert_eq!(opened.buffer.text(), original);
        assert_eq!(opened.buffer.content_bytes(), original.as_bytes());

        let end = opened.buffer.text().chars().count();
        let line_ending = opened.buffer.line_ending.as_str();
        opened.buffer.insert(end, line_ending).unwrap();
        assert_eq!(opened.buffer.text(), format!("{original}\r\n"));
    }

    #[test]
    fn new_and_lf_buffers_default_to_lf() {
        assert_eq!(Buffer::new_untitled(1).line_ending, LineEnding::Lf);
        let buffer = Buffer::from_text(
            2,
            DocumentSource::Untitled {
                name: "lf.txt".to_string(),
            },
            "first\nsecond",
            TextEncoding::Utf8,
            None,
            None,
        );
        assert_eq!(buffer.line_ending, LineEnding::Lf);
    }

    #[test]
    fn decodes_utf16_bom() {
        let bytes = [0xFF, 0xFE, b'h', 0, b'i', 0];
        let (text, encoding) = decode_text(&bytes).unwrap();
        assert_eq!(text, "hi");
        assert_eq!(encoding, TextEncoding::Utf16Le);
    }

    #[test]
    fn utf16_bom_file_opens_as_editable_text() {
        let bytes = [0xFF, 0xFE, b'h', 0, b'i', 0];
        let opened = Buffer::from_bytes(
            1,
            DocumentSource::Untitled {
                name: "utf16.txt".to_string(),
            },
            &bytes,
            None,
            Some(bytes.len() as u64),
            ReadExtent::Complete,
        )
        .unwrap();

        assert_eq!(opened.buffer.text(), "hi");
        assert_eq!(opened.buffer.encoding, TextEncoding::Utf16Le);
        assert!(!opened.buffer.is_readonly());
    }

    #[test]
    fn bomless_utf16_opens_as_readonly_preview() {
        let bytes = [b'h', 0, b'i', 0, b'\n', 0, b'x', 0];
        let opened = Buffer::from_bytes(
            1,
            DocumentSource::Untitled {
                name: "utf16.txt".to_string(),
            },
            &bytes,
            None,
            Some(bytes.len() as u64),
            ReadExtent::Complete,
        )
        .unwrap();

        assert_eq!(opened.buffer.text(), "hi\nx");
        assert_eq!(opened.buffer.encoding, TextEncoding::Utf16LePreview);
        assert!(opened.buffer.is_readonly());
    }

    #[test]
    fn invalid_utf8_text_opens_as_legacy_readonly_preview() {
        let bytes = b"caf\xe9";
        let opened = Buffer::from_bytes(
            1,
            DocumentSource::Untitled {
                name: "latin1.txt".to_string(),
            },
            bytes,
            None,
            Some(bytes.len() as u64),
            ReadExtent::Complete,
        )
        .unwrap();

        assert_eq!(opened.buffer.text(), "caf\u{e9}");
        assert_eq!(opened.buffer.encoding, TextEncoding::Windows1252Preview);
        assert!(opened.buffer.is_readonly());
    }

    #[test]
    fn truncated_text_extent_opens_as_readonly_preview() {
        let bytes = b"first lines only\n";
        let opened = Buffer::from_bytes(
            1,
            DocumentSource::Untitled {
                name: "huge.log".to_string(),
            },
            bytes,
            None,
            Some(300 * 1024 * 1024),
            ReadExtent::Preview {
                shown_bytes: bytes.len() as u64,
                total_bytes: 300 * 1024 * 1024,
            },
        )
        .unwrap();

        assert!(opened.buffer.text().contains("first lines only"));
        assert_eq!(
            opened.buffer.large_file_mode,
            LargeFileMode::ReadOnlyPreview
        );
        assert!(opened.buffer.is_readonly());
    }

    #[test]
    fn undo_and_redo_restore_text() {
        let mut buffer = Buffer::new_untitled(1);
        buffer.insert(0, "hello").unwrap();
        buffer.insert(5, " world").unwrap();
        assert_eq!(buffer.text(), "hello world");
        assert!(buffer.undo());
        assert_eq!(buffer.text(), "hello");
        assert!(buffer.redo());
        assert_eq!(buffer.text(), "hello world");
    }

    #[test]
    fn consecutive_typing_coalesces_into_one_undo() {
        let mut buffer = Buffer::new_untitled(1);
        buffer.insert(0, "h").unwrap();
        buffer.insert(1, "é").unwrap();
        buffer.insert(2, "日").unwrap();
        assert_eq!(buffer.text(), "hé日");

        assert!(buffer.undo());
        assert_eq!(buffer.text(), "");
        assert!(buffer.redo());
        assert_eq!(buffer.text(), "hé日");
    }

    #[test]
    fn consecutive_backward_deletes_coalesce_and_remain_unicode_safe() {
        let mut buffer = Buffer::new_untitled(1);
        buffer.set_text("aé日", "fixture").unwrap();
        buffer.break_undo_group();
        buffer.delete_range(2, 3).unwrap();
        buffer.delete_range(1, 2).unwrap();
        assert_eq!(buffer.text(), "a");

        assert!(buffer.undo());
        assert_eq!(buffer.text(), "aé日");
    }

    #[test]
    fn newline_and_paste_are_distinct_undo_steps() {
        let mut buffer = Buffer::new_untitled(1);
        buffer.insert(0, "a").unwrap();
        buffer.insert(1, "\n").unwrap();
        buffer.insert(2, "pasted").unwrap();
        assert_eq!(buffer.text(), "a\npasted");

        assert!(buffer.undo());
        assert_eq!(buffer.text(), "a\n");
        assert!(buffer.undo());
        assert_eq!(buffer.text(), "a");
        assert!(buffer.undo());
        assert_eq!(buffer.text(), "");
    }

    #[test]
    fn isolated_single_character_edit_does_not_join_typing() {
        let mut buffer = Buffer::new_untitled(1);
        buffer.replace_range(0, 0, "a").unwrap();
        buffer.replace_range_isolated(1, 1, "b").unwrap();
        buffer.replace_range(2, 2, "c").unwrap();
        assert_eq!(buffer.text(), "abc");

        assert!(buffer.undo());
        assert_eq!(buffer.text(), "ab");
        assert!(buffer.undo());
        assert_eq!(buffer.text(), "a");
        assert!(buffer.undo());
        assert_eq!(buffer.text(), "");
    }

    #[test]
    fn classifies_large_modes() {
        assert_eq!(classify_large_file(1, 1), LargeFileMode::Normal);
        assert_eq!(
            classify_large_file(4 * 1024 * 1024 - 1, 1),
            LargeFileMode::Normal
        );
        assert_eq!(
            classify_large_file(4 * 1024 * 1024, 1),
            LargeFileMode::Opportunistic
        );
        assert_eq!(
            classify_large_file(64 * 1024 * 1024, 1),
            LargeFileMode::Degraded
        );
        assert_eq!(
            classify_large_file(257 * 1024 * 1024, 1),
            LargeFileMode::ReadOnlyPreview
        );
        assert_eq!(
            classify_large_file(1, 5_000_001),
            LargeFileMode::ReadOnlyPreview
        );
    }

    #[test]
    fn search_returns_line_matches() {
        let mut buffer = Buffer::new_untitled(1);
        buffer.set_text("alpha\nbeta\nalphabet", "test").unwrap();
        let matches = buffer.search("alpha", 10);
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[1].line_number, 3);
        assert_eq!(matches[1].column_number, 1);
        assert_eq!(matches[1].absolute_byte_start, 11);
    }

    #[test]
    fn replace_first_updates_one_match() {
        let mut buffer = Buffer::new_untitled(1);
        buffer.set_text("alpha beta alpha", "test").unwrap();
        let found = buffer.replace_first("alpha", "omega").unwrap().unwrap();
        assert_eq!(found.line_number, 1);
        assert_eq!(buffer.text(), "omega beta alpha");
        assert!(buffer.is_dirty());
    }

    #[test]
    fn replace_all_updates_all_matches() {
        let mut buffer = Buffer::new_untitled(1);
        buffer.set_text("one two one", "test").unwrap();
        let count = buffer.replace_all("one", "1").unwrap();
        assert_eq!(count, 2);
        assert_eq!(buffer.text(), "1 two 1");
    }

    #[test]
    fn replace_char_range_is_one_undo_transaction() {
        let mut buffer = Buffer::new_untitled(1);
        buffer.set_text("hello world", "test").unwrap();
        buffer.replace_range(6, 11, "Texti").unwrap();
        assert_eq!(buffer.text(), "hello Texti");
        assert!(buffer.undo());
        assert_eq!(buffer.text(), "hello world");
    }

    #[test]
    fn line_start_byte_is_one_based() {
        let mut buffer = Buffer::new_untitled(1);
        buffer.set_text("aa\nbbb\nc", "test").unwrap();
        assert_eq!(buffer.line_start_byte(1), Some(0));
        assert_eq!(buffer.line_start_byte(2), Some(3));
        assert_eq!(buffer.line_start_byte(0), None);
    }
}
