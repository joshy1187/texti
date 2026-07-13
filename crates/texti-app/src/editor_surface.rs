use crate::{EditorUiRow, EditorUiSegment};
use slint::{Color, SharedString};
use std::collections::HashMap;
use texti_model::{BufferId, SyntaxSpan};

pub const LINE_HEIGHT: f32 = 17.0;
pub const CHAR_WIDTH: f32 = 8.4;
pub const TEXT_LEFT: f32 = 12.0;
pub const TEXT_TOP: f32 = 1.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EditorRenderConfig {
    pub word_wrap: bool,
    pub viewport_width: f32,
    pub viewport_height: f32,
    pub scroll_x: f32,
    pub scroll_y: f32,
    pub font_size: f32,
    pub cell_width: f32,
    pub tab_size: usize,
    pub show_whitespace: bool,
    pub overscan_rows: usize,
}

impl Default for EditorRenderConfig {
    fn default() -> Self {
        Self {
            word_wrap: false,
            viewport_width: 800.0,
            viewport_height: f32::INFINITY,
            scroll_x: 0.0,
            scroll_y: 0.0,
            font_size: 14.0,
            cell_width: CHAR_WIDTH,
            tab_size: 4,
            show_whitespace: false,
            overscan_rows: 4,
        }
    }
}

impl EditorRenderConfig {
    pub fn line_height(self) -> f32 {
        (self.font_size.max(8.0) + 3.0).max(12.0)
    }

    fn normalized(self) -> Self {
        let font_size = self.font_size.clamp(8.0, 72.0);
        let fallback_cell_width = (font_size * 0.6).max(4.8);
        Self {
            viewport_width: self.viewport_width.max(TEXT_LEFT + 32.0),
            viewport_height: if self.viewport_height.is_nan() {
                f32::INFINITY
            } else {
                self.viewport_height.max(0.0)
            },
            scroll_x: self.scroll_x.max(0.0),
            scroll_y: self.scroll_y.max(0.0),
            font_size,
            cell_width: if self.cell_width.is_finite() && self.cell_width > 0.0 {
                self.cell_width
            } else {
                fallback_cell_width
            },
            tab_size: self.tab_size.clamp(1, 16),
            overscan_rows: self.overscan_rows.min(100),
            ..self
        }
    }
}

#[derive(Clone, Debug)]
pub struct RenderedEditor {
    pub rows: Vec<EditorUiRow>,
    pub segments: Vec<EditorUiSegment>,
    pub content_width: f32,
    pub content_height: f32,
    pub caret_x: f32,
    pub caret_y: f32,
    pub caret_height: f32,
    pub caret_visible: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct EditorViewState {
    pub cursor: usize,
    pub anchor: usize,
    pub preferred_column: Option<usize>,
    pub scroll_x: f32,
    pub scroll_y: f32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditorEdit {
    pub start: usize,
    pub end: usize,
    pub replacement: String,
    pub cursor: usize,
    pub anchor: usize,
}

#[derive(Clone, Debug)]
pub struct EditorRuntime {
    active_buffer_id: Option<BufferId>,
    active_revision: u64,
    pub cursor: usize,
    pub anchor: usize,
    preferred_column: Option<usize>,
    scroll_x: f32,
    scroll_y: f32,
    views: HashMap<BufferId, EditorViewState>,
    rows: Vec<VisualRow>,
    byte_offsets: Vec<usize>,
    layout_key: Option<LayoutKey>,
    max_columns: usize,
    metrics: LayoutMetrics,
    caret_visible: bool,
}

impl Default for EditorRuntime {
    fn default() -> Self {
        Self {
            active_buffer_id: None,
            active_revision: 0,
            cursor: 0,
            anchor: 0,
            preferred_column: None,
            scroll_x: 0.0,
            scroll_y: 0.0,
            views: HashMap::new(),
            rows: Vec::new(),
            byte_offsets: vec![0],
            layout_key: None,
            max_columns: 1,
            metrics: LayoutMetrics::default(),
            caret_visible: true,
        }
    }
}

#[allow(dead_code)]
impl EditorRuntime {
    pub fn sync(&mut self, active_buffer_id: Option<BufferId>, active_revision: u64, text: &str) {
        let content_changed =
            self.active_buffer_id != active_buffer_id || self.active_revision != active_revision;
        if self.active_buffer_id != active_buffer_id {
            self.persist_active_view();
            let view = active_buffer_id
                .and_then(|id| self.views.get(&id).cloned())
                .unwrap_or_default();
            self.apply_view(view);
            self.layout_key = None;
        } else if self.active_revision != active_revision {
            self.preferred_column = None;
        }
        let len = text.chars().count();
        self.cursor = self.cursor.min(len);
        self.anchor = self.anchor.min(len);
        self.active_buffer_id = active_buffer_id;
        self.active_revision = active_revision;
        if content_changed {
            self.caret_visible = true;
        }
        self.persist_active_view();
    }

    pub fn active_buffer_id(&self) -> Option<BufferId> {
        self.active_buffer_id
    }

    pub fn view_state(&self, buffer_id: BufferId) -> Option<EditorViewState> {
        if self.active_buffer_id == Some(buffer_id) {
            Some(self.current_view())
        } else {
            self.views.get(&buffer_id).cloned()
        }
    }

    pub fn set_view_state(&mut self, buffer_id: BufferId, view: EditorViewState) {
        self.views.insert(buffer_id, view.clone());
        if self.active_buffer_id == Some(buffer_id) {
            self.apply_view(view);
        }
    }

    pub fn view_states(&self) -> Vec<(BufferId, EditorViewState)> {
        let mut views = self.views.clone();
        if let Some(buffer_id) = self.active_buffer_id {
            views.insert(buffer_id, self.current_view());
        }
        let mut views = views.into_iter().collect::<Vec<_>>();
        views.sort_by_key(|(buffer_id, _)| *buffer_id);
        views
    }

    pub fn replace_view_states(
        &mut self,
        views: impl IntoIterator<Item = (BufferId, EditorViewState)>,
    ) {
        self.views = views.into_iter().collect();
        if let Some(buffer_id) = self.active_buffer_id
            && let Some(view) = self.views.get(&buffer_id).cloned()
        {
            self.apply_view(view);
        }
    }

    pub fn remove_view_state(&mut self, buffer_id: BufferId) {
        self.views.remove(&buffer_id);
    }

    pub fn set_viewport(&mut self, scroll_x: f32, scroll_y: f32) {
        self.scroll_x = scroll_x.max(0.0);
        self.scroll_y = scroll_y.max(0.0);
        self.persist_active_view();
    }

    pub fn viewport(&self) -> (f32, f32) {
        (self.scroll_x, self.scroll_y)
    }

    pub fn set_selection(&mut self, anchor: usize, cursor: usize, text: &str) {
        let len = text.chars().count();
        self.anchor = anchor.min(len);
        self.cursor = cursor.min(len);
        self.preferred_column = None;
        self.finish_interaction();
    }

    pub fn apply_edit(&mut self, edit: &EditorEdit) {
        self.cursor = edit.cursor;
        self.anchor = edit.anchor;
        self.preferred_column = None;
        self.finish_interaction();
    }

    pub fn select_all(&mut self, text: &str) {
        self.anchor = 0;
        self.cursor = text.chars().count();
        self.preferred_column = None;
        self.finish_interaction();
    }

    pub fn clear_selection(&mut self) {
        self.anchor = self.cursor;
        self.finish_interaction();
    }

    pub fn selection_range(&self) -> Option<(usize, usize)> {
        let start = self.anchor.min(self.cursor);
        let end = self.anchor.max(self.cursor);
        (start != end).then_some((start, end))
    }

    pub fn selected_text(&self, text: &str) -> String {
        let Some((start, end)) = self.selection_range() else {
            return String::new();
        };
        text.chars().skip(start).take(end - start).collect()
    }

    pub fn select_byte_range(&mut self, text: &str, start_byte: usize, end_byte: usize) {
        let offsets = byte_offsets(text);
        self.anchor = byte_to_char(&offsets, start_byte);
        self.cursor = byte_to_char(&offsets, end_byte);
        self.preferred_column = None;
        self.finish_interaction();
    }

    pub fn hit_test(&self, x: f32, y: f32) -> usize {
        if self.rows.is_empty() {
            return 0;
        }
        let row_index = (y / self.metrics.line_height).floor().max(0.0) as usize;
        let row = self
            .rows
            .get(row_index)
            .or_else(|| self.rows.last())
            .expect("row fallback exists");
        let col = ((x - TEXT_LEFT + self.metrics.char_width / 2.0) / self.metrics.char_width)
            .floor()
            .max(0.0) as usize;
        row.char_at_column(col)
    }

    pub fn pointer_down(&mut self, text: &str, x: f32, y: f32, extend: bool) {
        let hit = self.hit_test(x, y).min(text.chars().count());
        if !extend {
            self.anchor = hit;
        }
        self.cursor = hit;
        self.preferred_column = None;
        self.finish_interaction();
    }

    pub fn pointer_drag(&mut self, text: &str, x: f32, y: f32) {
        self.cursor = self.hit_test(x, y).min(text.chars().count());
        self.preferred_column = None;
        self.finish_interaction();
    }

    pub fn pointer_select_word(&mut self, text: &str, x: f32, y: f32) {
        let hit = self.hit_test(x, y).min(text.chars().count());
        self.select_word_at(text, hit);
    }

    pub fn pointer_select_line(&mut self, text: &str, x: f32, y: f32) {
        let hit = self.hit_test(x, y).min(text.chars().count());
        self.select_line_at(text, hit);
    }

    pub fn select_word_at(&mut self, text: &str, char_index: usize) {
        let (start, end) = word_range_at(text, char_index);
        self.anchor = start;
        self.cursor = end;
        self.preferred_column = None;
        self.finish_interaction();
    }

    pub fn select_line_at(&mut self, text: &str, char_index: usize) {
        let (start, end) = line_range_at(text, char_index);
        self.anchor = start;
        self.cursor = end;
        self.preferred_column = None;
        self.finish_interaction();
    }

    pub fn move_left(&mut self, text: &str, extend: bool) {
        if !extend && let Some((start, _)) = self.selection_range() {
            self.cursor = start;
            self.anchor = start;
            self.preferred_column = None;
            self.finish_interaction();
            return;
        }
        self.cursor = previous_cursor_position(text, self.cursor);
        self.cursor = self.cursor.min(text.chars().count());
        self.complete_nonvertical_move(extend);
    }

    pub fn move_right(&mut self, text: &str, extend: bool) {
        if !extend && let Some((_, end)) = self.selection_range() {
            self.cursor = end;
            self.anchor = end;
            self.preferred_column = None;
            self.finish_interaction();
            return;
        }
        self.cursor = next_cursor_position(text, self.cursor);
        self.complete_nonvertical_move(extend);
    }

    pub fn move_word_left(&mut self, text: &str, extend: bool) {
        if !extend && let Some((start, _)) = self.selection_range() {
            self.cursor = start;
        } else {
            self.cursor = word_boundary_left(text, self.cursor);
        }
        self.complete_nonvertical_move(extend);
    }

    pub fn move_word_right(&mut self, text: &str, extend: bool) {
        if !extend && let Some((_, end)) = self.selection_range() {
            self.cursor = end;
        } else {
            self.cursor = word_boundary_right(text, self.cursor);
        }
        self.complete_nonvertical_move(extend);
    }

    pub fn move_document_start(&mut self, extend: bool) {
        self.cursor = 0;
        self.complete_nonvertical_move(extend);
    }

    pub fn move_document_end(&mut self, text: &str, extend: bool) {
        self.cursor = text.chars().count();
        self.complete_nonvertical_move(extend);
    }

    pub fn move_home(&mut self, extend: bool) {
        if let Some(row) = self.row_for_cursor() {
            self.cursor = row.start_char;
        } else {
            self.cursor = 0;
        }
        self.complete_nonvertical_move(extend);
    }

    pub fn move_end(&mut self, extend: bool) {
        if let Some(row) = self.row_for_cursor() {
            self.cursor = row.end_char;
        }
        self.complete_nonvertical_move(extend);
    }

    pub fn move_vertical(&mut self, text: &str, rows_delta: isize, extend: bool) {
        if self.rows.is_empty() {
            return;
        }
        let current_index = self.row_index_for_cursor().unwrap_or(0);
        let current = &self.rows[current_index];
        let col = self
            .preferred_column
            .unwrap_or_else(|| current.column_at_char(self.cursor));
        self.preferred_column = Some(col);
        let target_index = current_index
            .saturating_add_signed(rows_delta)
            .min(self.rows.len().saturating_sub(1));
        let target = &self.rows[target_index];
        self.cursor = target.char_at_column(col).min(text.chars().count());
        if !extend {
            self.anchor = self.cursor;
        }
        self.finish_interaction();
    }

    pub fn delete_word_backward_range(&self, text: &str) -> Option<(usize, usize)> {
        if let Some(selection) = self.selection_range() {
            return Some(selection);
        }
        let start = word_boundary_left(text, self.cursor);
        (start < self.cursor).then_some((start, self.cursor))
    }

    pub fn delete_word_forward_range(&self, text: &str) -> Option<(usize, usize)> {
        if let Some(selection) = self.selection_range() {
            return Some(selection);
        }
        let end = word_boundary_right(text, self.cursor);
        (self.cursor < end).then_some((self.cursor, end))
    }

    pub fn delete_backward_range(&self, text: &str) -> Option<(usize, usize)> {
        if let Some(selection) = self.selection_range() {
            return Some(selection);
        }
        let start = previous_cursor_position(text, self.cursor);
        (start < self.cursor).then_some((start, self.cursor))
    }

    pub fn delete_forward_range(&self, text: &str) -> Option<(usize, usize)> {
        if let Some(selection) = self.selection_range() {
            return Some(selection);
        }
        let end = next_cursor_position(text, self.cursor);
        (self.cursor < end).then_some((self.cursor, end))
    }

    pub fn indent_edit(&self, text: &str, indent: &str) -> Option<EditorEdit> {
        if indent.is_empty() {
            return None;
        }
        let Some((selection_start, selection_end)) = self.selection_range() else {
            let inserted_chars = indent.chars().count();
            return Some(EditorEdit {
                start: self.cursor,
                end: self.cursor,
                replacement: indent.to_string(),
                cursor: self.cursor + inserted_chars,
                anchor: self.cursor + inserted_chars,
            });
        };
        let chars = text.chars().collect::<Vec<_>>();
        let (start, end) = selected_line_span(&chars, selection_start, selection_end);
        let line_starts = line_starts_in_span(&chars, start, end);
        let mut replacement = String::new();
        for (index, ch) in chars.iter().enumerate().take(end).skip(start) {
            if line_starts.binary_search(&index).is_ok() {
                replacement.push_str(indent);
            }
            replacement.push(*ch);
        }
        let indent_chars = indent.chars().count();
        let map = |position: usize| {
            position
                + line_starts
                    .iter()
                    .filter(|line_start| **line_start <= position)
                    .count()
                    * indent_chars
        };
        Some(EditorEdit {
            start,
            end,
            replacement,
            cursor: map(self.cursor),
            anchor: map(self.anchor),
        })
    }

    pub fn outdent_edit(&self, text: &str, tab_size: usize) -> Option<EditorEdit> {
        let chars = text.chars().collect::<Vec<_>>();
        let (selection_start, selection_end) =
            self.selection_range().unwrap_or((self.cursor, self.cursor));
        let (start, end) = selected_line_span(&chars, selection_start, selection_end);
        let mut removals = Vec::new();
        for line_start in line_starts_in_span(&chars, start, end) {
            let removal_end = if chars.get(line_start) == Some(&'\t') {
                line_start + 1
            } else {
                let mut cursor = line_start;
                while cursor < end
                    && cursor < line_start + tab_size.max(1)
                    && chars.get(cursor) == Some(&' ')
                {
                    cursor += 1;
                }
                cursor
            };
            if removal_end > line_start {
                removals.push((line_start, removal_end));
            }
        }
        if removals.is_empty() {
            return None;
        }
        let mut replacement = String::new();
        let mut removal_index = 0usize;
        let mut cursor = start;
        while cursor < end {
            if let Some((remove_start, remove_end)) = removals.get(removal_index).copied()
                && cursor == remove_start
            {
                cursor = remove_end;
                removal_index += 1;
                continue;
            }
            replacement.push(chars[cursor]);
            cursor += 1;
        }
        let map = |position: usize| map_after_removals(position, &removals);
        Some(EditorEdit {
            start,
            end,
            replacement,
            cursor: map(self.cursor),
            anchor: map(self.anchor),
        })
    }

    pub fn auto_indent_edit(&self, text: &str, indent: &str, newline: &str) -> EditorEdit {
        let chars = text.chars().collect::<Vec<_>>();
        let (start, end) = self.selection_range().unwrap_or((self.cursor, self.cursor));
        let line_start = find_line_start(&chars, start);
        let base_indent = chars[line_start..start]
            .iter()
            .take_while(|ch| matches!(**ch, ' ' | '\t'))
            .collect::<String>();
        let opening = chars[line_start..start]
            .iter()
            .rev()
            .find(|ch| !ch.is_whitespace())
            .copied();
        let line_end = find_line_end(&chars, end);
        let closing = chars[end..line_end]
            .iter()
            .find(|ch| !ch.is_whitespace())
            .copied();
        let increase = matches!(opening, Some('{' | '[' | '('));
        let paired = matches!(
            (opening, closing),
            (Some('{'), Some('}')) | (Some('['), Some(']')) | (Some('('), Some(')'))
        );
        let mut replacement = format!("{newline}{base_indent}");
        if increase {
            replacement.push_str(indent);
        }
        let cursor = start + replacement.chars().count();
        if paired {
            replacement.push_str(newline);
            replacement.push_str(&base_indent);
        }
        EditorEdit {
            start,
            end,
            replacement,
            cursor,
            anchor: cursor,
        }
    }

    pub fn cursor_line_col(&self) -> (usize, usize) {
        if let Some(row) = self.row_for_cursor() {
            (
                row.line_number,
                self.cursor.saturating_sub(row.line_start_char) + 1,
            )
        } else {
            (1, 1)
        }
    }

    fn row_for_cursor(&self) -> Option<&VisualRow> {
        self.row_index_for_cursor()
            .and_then(|index| self.rows.get(index))
    }

    fn row_index_for_cursor(&self) -> Option<usize> {
        self.rows
            .iter()
            .enumerate()
            .rev()
            .find(|(_, row)| self.cursor >= row.start_char && self.cursor <= row.end_char)
            .map(|(index, _)| index)
            .or_else(|| (!self.rows.is_empty()).then_some(self.rows.len() - 1))
    }

    fn complete_nonvertical_move(&mut self, extend: bool) {
        if !extend {
            self.anchor = self.cursor;
        }
        self.preferred_column = None;
        self.finish_interaction();
    }

    fn finish_interaction(&mut self) {
        self.caret_visible = true;
        self.persist_active_view();
    }

    pub fn show_caret(&mut self) {
        self.caret_visible = true;
    }

    pub fn hide_caret(&mut self) {
        self.caret_visible = false;
    }

    pub fn toggle_caret(&mut self) -> bool {
        self.caret_visible = !self.caret_visible;
        self.caret_visible
    }

    pub fn caret_geometry(&self) -> Option<(f32, f32, f32)> {
        let row = self.row_for_cursor()?;
        Some((
            TEXT_LEFT + row.column_at_char(self.cursor) as f32 * self.metrics.char_width,
            row.y,
            self.metrics.line_height,
        ))
    }

    pub fn viewport_to_reveal_caret(
        &self,
        viewport_width: f32,
        viewport_height: f32,
        padding: f32,
    ) -> (f32, f32) {
        let Some((caret_x, caret_y, caret_height)) = self.caret_geometry() else {
            return self.viewport();
        };
        let padding = padding.max(0.0);
        let mut x = self.scroll_x;
        let mut y = self.scroll_y;
        if caret_x < x + padding {
            x = (caret_x - padding).max(0.0);
        } else if caret_x > x + viewport_width.max(0.0) - padding {
            x = (caret_x - viewport_width.max(0.0) + padding).max(0.0);
        }
        if caret_y < y + padding {
            y = (caret_y - padding).max(0.0);
        } else if caret_y + caret_height > y + viewport_height.max(0.0) - padding {
            y = (caret_y + caret_height - viewport_height.max(0.0) + padding).max(0.0);
        }
        (x, y)
    }

    fn current_view(&self) -> EditorViewState {
        EditorViewState {
            cursor: self.cursor,
            anchor: self.anchor,
            preferred_column: self.preferred_column,
            scroll_x: self.scroll_x,
            scroll_y: self.scroll_y,
        }
    }

    fn apply_view(&mut self, view: EditorViewState) {
        self.cursor = view.cursor;
        self.anchor = view.anchor;
        self.preferred_column = view.preferred_column;
        self.scroll_x = view.scroll_x.max(0.0);
        self.scroll_y = view.scroll_y.max(0.0);
    }

    fn persist_active_view(&mut self) {
        if let Some(buffer_id) = self.active_buffer_id {
            self.views.insert(buffer_id, self.current_view());
        }
    }

    fn ensure_layout(
        &mut self,
        active_buffer_id: Option<BufferId>,
        active_revision: u64,
        text: &str,
        config: EditorRenderConfig,
    ) {
        let key = LayoutKey::new(active_buffer_id, active_revision, config);
        if self.layout_key.as_ref() == Some(&key) {
            return;
        }
        self.metrics = LayoutMetrics {
            line_height: config.line_height(),
            char_width: config.cell_width,
        };
        self.rows = visual_rows(
            text,
            config.word_wrap,
            config.viewport_width,
            self.metrics,
            config.tab_size,
        );
        self.byte_offsets = byte_offsets(text);
        self.max_columns = self
            .rows
            .iter()
            .map(VisualRow::display_columns)
            .max()
            .unwrap_or(1)
            .max(1);
        self.layout_key = Some(key);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LayoutKey {
    active_buffer_id: Option<BufferId>,
    active_revision: u64,
    word_wrap: bool,
    viewport_width_bits: u32,
    font_size_bits: u32,
    cell_width_bits: u32,
    tab_size: usize,
}

impl LayoutKey {
    fn new(
        active_buffer_id: Option<BufferId>,
        active_revision: u64,
        config: EditorRenderConfig,
    ) -> Self {
        Self {
            active_buffer_id,
            active_revision,
            word_wrap: config.word_wrap,
            viewport_width_bits: if config.word_wrap {
                config.viewport_width.to_bits()
            } else {
                0
            },
            font_size_bits: config.font_size.to_bits(),
            cell_width_bits: config.cell_width.to_bits(),
            tab_size: config.tab_size,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct LayoutMetrics {
    line_height: f32,
    char_width: f32,
}

impl Default for LayoutMetrics {
    fn default() -> Self {
        Self {
            line_height: LINE_HEIGHT,
            char_width: CHAR_WIDTH,
        }
    }
}

#[allow(dead_code)]
pub fn render_editor(
    runtime: &mut EditorRuntime,
    active_buffer_id: Option<BufferId>,
    active_revision: u64,
    text: &str,
    spans: &[SyntaxSpan],
    word_wrap: bool,
    viewport_width: f32,
) -> RenderedEditor {
    render_editor_with_config(
        runtime,
        active_buffer_id,
        active_revision,
        text,
        spans,
        EditorRenderConfig {
            word_wrap,
            viewport_width,
            ..EditorRenderConfig::default()
        },
    )
}

pub fn render_editor_with_config(
    runtime: &mut EditorRuntime,
    active_buffer_id: Option<BufferId>,
    active_revision: u64,
    text: &str,
    spans: &[SyntaxSpan],
    config: EditorRenderConfig,
) -> RenderedEditor {
    let mut config = config.normalized();
    let switched_buffer = runtime.active_buffer_id() != active_buffer_id;
    let has_saved_view = active_buffer_id
        .and_then(|buffer_id| runtime.view_state(buffer_id))
        .is_some();
    runtime.sync(active_buffer_id, active_revision, text);
    if switched_buffer && has_saved_view {
        (config.scroll_x, config.scroll_y) = runtime.viewport();
    } else {
        runtime.set_viewport(config.scroll_x, config.scroll_y);
    }
    runtime.ensure_layout(active_buffer_id, active_revision, text, config);

    let (first_row, end_row) = visible_row_range(runtime.rows.len(), config);
    let mut rows = Vec::with_capacity(end_row.saturating_sub(first_row));
    let mut segments = Vec::new();
    let selection = runtime.selection_range();
    let mut caret_x = TEXT_LEFT;
    let mut caret_y = 0.0;
    if let Some(row) = runtime.row_for_cursor() {
        caret_x =
            TEXT_LEFT + row.column_at_char(runtime.cursor) as f32 * runtime.metrics.char_width;
        caret_y = row.y;
    }

    for row in &runtime.rows[first_row..end_row] {
        let selected = selection_overlap(selection, row.start_char, row.end_char);
        rows.push(EditorUiRow {
            line_number: row.line_number as i32,
            y: row.y,
            selected_x: selected
                .map(|(start, _)| {
                    TEXT_LEFT + row.column_at_char(start) as f32 * runtime.metrics.char_width
                })
                .unwrap_or(0.0),
            selected_width: selected
                .map(|(start, end)| {
                    (row.column_at_char(end) - row.column_at_char(start)) as f32
                        * runtime.metrics.char_width
                })
                .unwrap_or(0.0),
        });

        push_row_segments(
            &mut segments,
            text,
            &runtime.byte_offsets,
            spans,
            row,
            runtime.metrics,
            config,
        );
    }

    let content_width = TEXT_LEFT + runtime.max_columns as f32 * runtime.metrics.char_width + 32.0;
    let content_height = runtime.rows.len().max(1) as f32 * runtime.metrics.line_height + 4.0;

    RenderedEditor {
        rows,
        segments,
        content_width,
        content_height,
        caret_x,
        caret_y,
        caret_height: runtime.metrics.line_height,
        caret_visible: runtime.caret_visible,
    }
}

fn visible_row_range(total_rows: usize, config: EditorRenderConfig) -> (usize, usize) {
    if total_rows == 0 {
        return (0, 0);
    }
    if !config.viewport_height.is_finite() || config.viewport_height <= 0.0 {
        return (0, total_rows);
    }
    let line_height = config.line_height();
    let first_visible = (config.scroll_y / line_height).floor() as usize;
    let visible_end = ((config.scroll_y + config.viewport_height) / line_height).ceil() as usize;
    (
        first_visible
            .saturating_sub(config.overscan_rows)
            .min(total_rows),
        visible_end
            .saturating_add(config.overscan_rows)
            .min(total_rows),
    )
}

fn push_row_segments(
    output: &mut Vec<EditorUiSegment>,
    text: &str,
    offsets: &[usize],
    spans: &[SyntaxSpan],
    row: &VisualRow,
    metrics: LayoutMetrics,
    config: EditorRenderConfig,
) {
    let row_start_byte = offsets[row.start_char];
    let row_end_byte = offsets[row.end_char];
    let mut cursor_char = row.start_char;
    let segment_context = SegmentRenderContext {
        text,
        offsets,
        row,
        metrics,
        show_whitespace: config.show_whitespace,
    };

    let first_span = spans.partition_point(|span| span.end_byte <= row_start_byte);
    for span in &spans[first_span..] {
        if span.start_byte >= row_end_byte {
            break;
        }
        let span_start_char = byte_to_char(offsets, span.start_byte.max(row_start_byte));
        let span_end_char = byte_to_char(offsets, span.end_byte.min(row_end_byte));
        if cursor_char < span_start_char {
            push_segment(
                output,
                &segment_context,
                cursor_char,
                span_start_char,
                SegmentStyle::plain(),
            );
        }
        let styled_start = span_start_char.max(cursor_char);
        if styled_start < span_end_char {
            push_segment(
                output,
                &segment_context,
                styled_start,
                span_end_char,
                SegmentStyle {
                    color: color_from_hex(span.foreground.as_deref()).unwrap_or_else(token_color),
                    bold: span.bold,
                    italic: span.italic,
                },
            );
        }
        cursor_char = cursor_char.max(span_end_char);
    }

    if cursor_char < row.end_char {
        push_segment(
            output,
            &segment_context,
            cursor_char,
            row.end_char,
            SegmentStyle::plain(),
        );
    }
}

#[derive(Clone, Copy)]
struct SegmentStyle {
    color: Color,
    bold: bool,
    italic: bool,
}

impl SegmentStyle {
    fn plain() -> Self {
        Self {
            color: text_color(),
            bold: false,
            italic: false,
        }
    }
}

struct SegmentRenderContext<'a> {
    text: &'a str,
    offsets: &'a [usize],
    row: &'a VisualRow,
    metrics: LayoutMetrics,
    show_whitespace: bool,
}

fn push_segment(
    output: &mut Vec<EditorUiSegment>,
    context: &SegmentRenderContext<'_>,
    start_char: usize,
    end_char: usize,
    style: SegmentStyle,
) {
    if start_char >= end_char {
        return;
    }
    let value = display_text(
        &context.text[context.offsets[start_char]..context.offsets[end_char]],
        context.row,
        start_char,
        context.show_whitespace,
    );
    if value.is_empty() {
        return;
    }
    output.push(EditorUiSegment {
        x: TEXT_LEFT + context.row.column_at_char(start_char) as f32 * context.metrics.char_width,
        y: context.row.y + TEXT_TOP,
        text: SharedString::from(value),
        color: style.color,
        bold: style.bold,
        italic: style.italic,
    });
}

#[derive(Clone, Debug)]
struct VisualRow {
    line_number: usize,
    line_start_char: usize,
    start_char: usize,
    end_char: usize,
    y: f32,
    columns: Vec<usize>,
}

impl VisualRow {
    fn display_columns(&self) -> usize {
        self.columns.last().copied().unwrap_or(0)
    }

    fn column_at_char(&self, char_index: usize) -> usize {
        let relative = char_index
            .saturating_sub(self.start_char)
            .min(self.columns.len().saturating_sub(1));
        self.columns.get(relative).copied().unwrap_or(0)
    }

    fn char_at_column(&self, column: usize) -> usize {
        let relative = match self.columns.binary_search(&column) {
            Ok(index) => index,
            Err(index) => {
                if index == 0 {
                    0
                } else if index >= self.columns.len() {
                    self.columns.len().saturating_sub(1)
                } else {
                    let before = self.columns[index - 1];
                    let after = self.columns[index];
                    if column - before < after - column {
                        index - 1
                    } else {
                        index
                    }
                }
            }
        };
        self.start_char + relative
    }
}

fn visual_rows(
    text: &str,
    word_wrap: bool,
    viewport_width: f32,
    metrics: LayoutMetrics,
    tab_size: usize,
) -> Vec<VisualRow> {
    let wrap_cols = if word_wrap {
        ((viewport_width - TEXT_LEFT - 24.0) / metrics.char_width)
            .floor()
            .max(8.0) as usize
    } else {
        usize::MAX
    };
    let layout = VisualLayout {
        wrap_cols,
        tab_size,
        line_height: metrics.line_height,
    };

    let mut rows = Vec::new();
    let mut line_number = 1usize;
    let mut char_cursor = 0usize;
    let mut y = 0.0f32;

    if text.is_empty() {
        rows.push(VisualRow {
            line_number,
            line_start_char: 0,
            start_char: 0,
            end_char: 0,
            y,
            columns: vec![0],
        });
        return rows;
    }

    for raw_line in text.split_inclusive('\n') {
        let line_chars = raw_line.chars().collect::<Vec<_>>();
        let visible_len = line_chars
            .iter()
            .rposition(|ch| *ch != '\n' && *ch != '\r')
            .map(|idx| idx + 1)
            .unwrap_or(0);
        push_visual_line(
            &mut rows,
            line_number,
            char_cursor,
            &line_chars[..visible_len],
            layout,
            &mut y,
        );
        char_cursor += line_chars.len();
        line_number += 1;
    }

    if text.ends_with('\n') {
        rows.push(VisualRow {
            line_number,
            line_start_char: char_cursor,
            start_char: char_cursor,
            end_char: char_cursor,
            y,
            columns: vec![0],
        });
    }

    rows
}

#[derive(Clone, Copy)]
struct VisualLayout {
    wrap_cols: usize,
    tab_size: usize,
    line_height: f32,
}

fn push_visual_line(
    rows: &mut Vec<VisualRow>,
    line_number: usize,
    line_start_char: usize,
    visible_chars: &[char],
    layout: VisualLayout,
    y: &mut f32,
) {
    if visible_chars.is_empty() {
        rows.push(VisualRow {
            line_number,
            line_start_char,
            start_char: line_start_char,
            end_char: line_start_char,
            y: *y,
            columns: vec![0],
        });
        *y += layout.line_height;
        return;
    }

    let mut offset = 0usize;
    while offset < visible_chars.len() {
        let row_start = offset;
        let mut columns = vec![0usize];
        let mut column = 0usize;
        while offset < visible_chars.len() {
            let width = if visible_chars[offset] == '\t' {
                layout.tab_size - column % layout.tab_size
            } else {
                1
            };
            if column > 0 && column.saturating_add(width) > layout.wrap_cols {
                break;
            }
            column = column.saturating_add(width);
            columns.push(column);
            offset += 1;
        }
        rows.push(VisualRow {
            line_number,
            line_start_char,
            start_char: line_start_char + row_start,
            end_char: line_start_char + offset,
            y: *y,
            columns,
        });
        *y += layout.line_height;
    }
}

fn display_text(text: &str, row: &VisualRow, start_char: usize, show_whitespace: bool) -> String {
    let mut output = String::new();
    for (offset, ch) in text.chars().enumerate() {
        let relative = start_char.saturating_sub(row.start_char) + offset;
        match ch {
            '\t' => {
                let width = row.columns[relative + 1].saturating_sub(row.columns[relative]);
                if show_whitespace {
                    output.push('\u{2192}');
                    output.extend(std::iter::repeat_n(' ', width.saturating_sub(1)));
                } else {
                    output.extend(std::iter::repeat_n(' ', width));
                }
            }
            ' ' if show_whitespace => output.push('\u{00b7}'),
            _ => output.push(ch),
        }
    }
    output
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CharacterClass {
    Word,
    Whitespace,
    Punctuation,
}

fn character_class(ch: char) -> CharacterClass {
    if ch.is_whitespace() {
        CharacterClass::Whitespace
    } else if ch.is_alphanumeric() || ch == '_' {
        CharacterClass::Word
    } else {
        CharacterClass::Punctuation
    }
}

fn previous_cursor_position(text: &str, char_index: usize) -> usize {
    let cursor = char_index.min(text.chars().count());
    if cursor >= 2 {
        let mut previous = text.chars().skip(cursor - 2);
        if previous.next() == Some('\r') && previous.next() == Some('\n') {
            return cursor - 2;
        }
    }
    cursor.saturating_sub(1)
}

fn next_cursor_position(text: &str, char_index: usize) -> usize {
    let len = text.chars().count();
    let cursor = char_index.min(len);
    let mut following = text.chars().skip(cursor);
    if following.next() == Some('\r') && following.next() == Some('\n') {
        (cursor + 2).min(len)
    } else {
        (cursor + 1).min(len)
    }
}

pub fn word_boundary_left(text: &str, char_index: usize) -> usize {
    let chars = text.chars().collect::<Vec<_>>();
    let mut cursor = char_index.min(chars.len());
    while cursor > 0 && character_class(chars[cursor - 1]) == CharacterClass::Whitespace {
        cursor -= 1;
    }
    let Some(class) = cursor
        .checked_sub(1)
        .map(|index| character_class(chars[index]))
    else {
        return cursor;
    };
    while cursor > 0 && character_class(chars[cursor - 1]) == class {
        cursor -= 1;
    }
    cursor
}

pub fn word_boundary_right(text: &str, char_index: usize) -> usize {
    let chars = text.chars().collect::<Vec<_>>();
    let mut cursor = char_index.min(chars.len());
    if cursor >= chars.len() {
        return cursor;
    }
    let class = character_class(chars[cursor]);
    while cursor < chars.len() && character_class(chars[cursor]) == class {
        cursor += 1;
    }
    if class != CharacterClass::Whitespace {
        while cursor < chars.len() && character_class(chars[cursor]) == CharacterClass::Whitespace {
            cursor += 1;
        }
    }
    cursor
}

#[allow(dead_code)]
pub fn word_range_at(text: &str, char_index: usize) -> (usize, usize) {
    let chars = text.chars().collect::<Vec<_>>();
    if chars.is_empty() {
        return (0, 0);
    }
    let index = char_index.min(chars.len().saturating_sub(1));
    let class = character_class(chars[index]);
    let mut start = index;
    let mut end = index + 1;
    while start > 0 && character_class(chars[start - 1]) == class {
        start -= 1;
    }
    while end < chars.len() && character_class(chars[end]) == class {
        end += 1;
    }
    (start, end)
}

#[allow(dead_code)]
pub fn line_range_at(text: &str, char_index: usize) -> (usize, usize) {
    let chars = text.chars().collect::<Vec<_>>();
    let index = char_index.min(chars.len());
    (
        find_line_start(&chars, index),
        find_line_end_including_break(&chars, index),
    )
}

fn find_line_start(chars: &[char], char_index: usize) -> usize {
    let mut cursor = char_index.min(chars.len());
    while cursor > 0 && chars[cursor - 1] != '\n' {
        cursor -= 1;
    }
    cursor
}

fn find_line_end(chars: &[char], char_index: usize) -> usize {
    let mut cursor = char_index.min(chars.len());
    while cursor < chars.len() && chars[cursor] != '\n' {
        cursor += 1;
    }
    cursor
}

fn find_line_end_including_break(chars: &[char], char_index: usize) -> usize {
    let cursor = find_line_end(chars, char_index);
    if cursor < chars.len() {
        cursor + 1
    } else {
        cursor
    }
}

fn selected_line_span(
    chars: &[char],
    selection_start: usize,
    selection_end: usize,
) -> (usize, usize) {
    let selection_start = selection_start.min(chars.len());
    let selection_end = selection_end.min(chars.len()).max(selection_start);
    let start = find_line_start(chars, selection_start);
    let probe = if selection_end > selection_start
        && chars.get(selection_end.saturating_sub(1)) == Some(&'\n')
    {
        selection_end - 1
    } else {
        selection_end
    };
    (start, find_line_end_including_break(chars, probe))
}

fn line_starts_in_span(chars: &[char], start: usize, end: usize) -> Vec<usize> {
    let mut starts = vec![start.min(chars.len())];
    for (index, ch) in chars
        .iter()
        .enumerate()
        .take(end.min(chars.len()))
        .skip(start.min(chars.len()))
    {
        if *ch == '\n' && index + 1 < end {
            starts.push(index + 1);
        }
    }
    starts
}

fn map_after_removals(position: usize, removals: &[(usize, usize)]) -> usize {
    let mut removed_before = 0usize;
    for &(start, end) in removals {
        if position <= start {
            break;
        }
        if position < end {
            return start.saturating_sub(removed_before);
        }
        removed_before += end.saturating_sub(start);
    }
    position.saturating_sub(removed_before)
}

fn selection_overlap(
    selection: Option<(usize, usize)>,
    row_start: usize,
    row_end: usize,
) -> Option<(usize, usize)> {
    let (start, end) = selection?;
    let start = start.max(row_start);
    let end = end.min(row_end);
    (start < end).then_some((start, end))
}

pub fn byte_offsets(text: &str) -> Vec<usize> {
    let mut offsets = Vec::with_capacity(text.chars().count() + 1);
    for (byte, _) in text.char_indices() {
        offsets.push(byte);
    }
    offsets.push(text.len());
    offsets
}

pub fn byte_to_char(offsets: &[usize], byte: usize) -> usize {
    match offsets.binary_search(&byte) {
        Ok(index) => index,
        Err(index) => index.saturating_sub(1),
    }
}

fn text_color() -> Color {
    Color::from_rgb_u8(243, 240, 247)
}

fn token_color() -> Color {
    Color::from_rgb_u8(201, 167, 255)
}

fn color_from_hex(value: Option<&str>) -> Option<Color> {
    let value = value?.strip_prefix('#')?;
    if value.len() != 6 {
        return None;
    }
    let red = u8::from_str_radix(&value[0..2], 16).ok()?;
    let green = u8::from_str_radix(&value[2..4], 16).ok()?;
    let blue = u8::from_str_radix(&value[4..6], 16).ok()?;
    Some(Color::from_rgb_u8(red, green, blue))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_to_char_is_utf8_safe() {
        let offsets = byte_offsets("aé日");
        assert_eq!(byte_to_char(&offsets, 0), 0);
        assert_eq!(byte_to_char(&offsets, 2), 1);
        assert_eq!(byte_to_char(&offsets, 5), 2);
    }

    #[test]
    fn selection_text_handles_utf8() {
        let mut runtime = EditorRuntime {
            anchor: 1,
            cursor: 3,
            ..EditorRuntime::default()
        };
        assert_eq!(runtime.selected_text("aé日z"), "é日");
        runtime.clear_selection();
        assert_eq!(runtime.selected_text("aé日z"), "");
    }

    #[test]
    fn word_wrap_splits_visual_rows() {
        let rows = visual_rows(
            "abcdefghijkl",
            true,
            TEXT_LEFT + CHAR_WIDTH * 8.0 + 24.0,
            LayoutMetrics::default(),
            4,
        );
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].start_char, 0);
        assert_eq!(rows[0].end_char, 8);
        assert_eq!(rows[1].start_char, 8);
    }

    #[test]
    fn render_splits_highlight_segments() {
        let mut runtime = EditorRuntime::default();
        let spans = vec![SyntaxSpan {
            start_byte: 3,
            end_byte: 5,
            class_name: "token".to_string(),
            foreground: Some("#A55CFF".to_string()),
            bold: false,
            italic: false,
        }];
        let rendered = render_editor(&mut runtime, Some(1), 0, "abcdef", &spans, false, 400.0);
        assert!(rendered.segments.len() >= 3);
        assert_eq!(rendered.rows.len(), 1);
    }

    #[test]
    fn hit_test_maps_to_row_and_column() {
        let mut runtime = EditorRuntime::default();
        let _ = render_editor(&mut runtime, Some(1), 0, "abc\ndef", &[], false, 400.0);
        assert_eq!(runtime.hit_test(TEXT_LEFT + CHAR_WIDTH * 2.0, 0.0), 2);
        assert_eq!(runtime.hit_test(TEXT_LEFT, LINE_HEIGHT + 1.0), 4);
    }

    #[test]
    fn measured_cell_width_drives_caret_selection_tabs_and_hit_testing() {
        let cell_width = 7.25;
        let config = EditorRenderConfig {
            cell_width,
            tab_size: 4,
            viewport_width: 400.0,
            ..EditorRenderConfig::default()
        };
        let mut runtime = EditorRuntime::default();
        let text = "a\tb";
        let _ = render_editor_with_config(&mut runtime, Some(1), 0, text, &[], config);
        runtime.set_selection(1, 2, text);
        let rendered = render_editor_with_config(&mut runtime, Some(1), 0, text, &[], config);

        assert_close(rendered.caret_x, TEXT_LEFT + cell_width * 4.0);
        assert_close(rendered.rows[0].selected_x, TEXT_LEFT + cell_width);
        assert_close(rendered.rows[0].selected_width, cell_width * 3.0);
        assert_close(rendered.content_width, TEXT_LEFT + cell_width * 5.0 + 32.0);
        assert_eq!(runtime.hit_test(TEXT_LEFT + cell_width * 4.0, 0.0), 2);
    }

    #[test]
    fn invalid_cell_width_uses_font_size_fallback() {
        let config = EditorRenderConfig {
            cell_width: f32::NAN,
            font_size: 20.0,
            viewport_width: 400.0,
            ..EditorRenderConfig::default()
        };
        let mut runtime = EditorRuntime::default();
        let _ = render_editor_with_config(&mut runtime, Some(1), 0, "ab", &[], config);
        runtime.set_selection(1, 1, "ab");
        let rendered = render_editor_with_config(&mut runtime, Some(1), 0, "ab", &[], config);

        assert_close(rendered.caret_x, TEXT_LEFT + 12.0);
    }

    #[test]
    fn non_wrapped_layout_key_ignores_viewport_width() {
        let config = EditorRenderConfig {
            cell_width: 7.25,
            viewport_width: 400.0,
            ..EditorRenderConfig::default()
        }
        .normalized();
        let wider = EditorRenderConfig {
            viewport_width: 900.0,
            ..config
        };
        assert_eq!(
            LayoutKey::new(Some(1), 2, config),
            LayoutKey::new(Some(1), 2, wider)
        );

        let wrapped = EditorRenderConfig {
            word_wrap: true,
            ..config
        };
        let wrapped_wider = EditorRenderConfig {
            word_wrap: true,
            ..wider
        };
        assert_ne!(
            LayoutKey::new(Some(1), 2, wrapped),
            LayoutKey::new(Some(1), 2, wrapped_wider)
        );

        let different_cell = EditorRenderConfig {
            cell_width: 8.0,
            ..config
        };
        assert_ne!(
            LayoutKey::new(Some(1), 2, config),
            LayoutKey::new(Some(1), 2, different_cell)
        );
    }

    #[test]
    fn buffer_views_survive_tab_switches() {
        let mut runtime = EditorRuntime::default();
        let _ = render_editor(&mut runtime, Some(1), 0, "first", &[], false, 400.0);
        runtime.set_selection(2, 4, "first");
        runtime.set_viewport(12.0, 34.0);

        let _ = render_editor(&mut runtime, Some(2), 0, "second", &[], false, 400.0);
        runtime.set_selection(1, 1, "second");
        let _ = render_editor(&mut runtime, Some(1), 0, "first", &[], false, 400.0);

        assert_eq!((runtime.anchor, runtime.cursor), (2, 4));
        assert_eq!(runtime.viewport(), (12.0, 34.0));
    }

    #[test]
    fn vertical_navigation_keeps_preferred_column() {
        let mut runtime = EditorRuntime::default();
        let text = "abcdef\nx\nabcdef";
        let _ = render_editor(&mut runtime, Some(1), 0, text, &[], false, 400.0);
        runtime.set_selection(5, 5, text);
        runtime.move_vertical(text, 1, false);
        assert_eq!(runtime.cursor, 8);
        runtime.move_vertical(text, 1, false);
        assert_eq!(runtime.cursor, 14);
    }

    #[test]
    fn word_and_line_selection_are_unicode_safe() {
        assert_eq!(word_range_at("go café!", 5), (3, 7));
        assert_eq!(word_boundary_left("go café!", 7), 3);
        assert_eq!(word_boundary_right("go café!", 0), 3);
        assert_eq!(line_range_at("one\ntwo\nthree", 5), (4, 8));
    }

    #[test]
    fn crlf_is_one_navigation_and_deletion_unit() {
        let text = "a\r\nb";
        let mut runtime = EditorRuntime {
            cursor: 1,
            anchor: 1,
            ..EditorRuntime::default()
        };
        runtime.move_right(text, false);
        assert_eq!(runtime.cursor, 3);
        assert_eq!(runtime.delete_backward_range(text), Some((1, 3)));
        runtime.move_left(text, false);
        assert_eq!(runtime.cursor, 1);
        assert_eq!(runtime.delete_forward_range(text), Some((1, 3)));
    }

    #[test]
    fn indent_and_outdent_selected_lines_preserve_selection_direction() {
        let text = "one\n  two\nthree";
        let runtime = EditorRuntime {
            anchor: 9,
            cursor: 0,
            ..EditorRuntime::default()
        };
        let indent = runtime.indent_edit(text, "  ").unwrap();
        assert_eq!(apply_edit_to_text(text, &indent), "  one\n    two\nthree");
        assert!(indent.anchor > indent.cursor);

        let indented = apply_edit_to_text(text, &indent);
        let runtime = EditorRuntime {
            anchor: indent.anchor,
            cursor: indent.cursor,
            ..EditorRuntime::default()
        };
        let outdent = runtime.outdent_edit(&indented, 2).unwrap();
        assert_eq!(apply_edit_to_text(&indented, &outdent), text);
    }

    #[test]
    fn auto_indent_places_caret_between_paired_braces() {
        let runtime = EditorRuntime {
            cursor: 4,
            anchor: 4,
            ..EditorRuntime::default()
        };
        let edit = runtime.auto_indent_edit("fn { }", "    ", "\n");
        assert_eq!(apply_edit_to_text("fn { }", &edit), "fn {\n    \n }");
        assert_eq!(edit.cursor, 9);

        let runtime = EditorRuntime {
            cursor: 4,
            anchor: 4,
            ..EditorRuntime::default()
        };
        let edit = runtime.auto_indent_edit("x\r\n{}", "    ", "\r\n");
        assert_eq!(apply_edit_to_text("x\r\n{}", &edit), "x\r\n{\r\n    \r\n}");
    }

    #[test]
    fn tabs_use_tab_stops_and_whitespace_is_display_only() {
        let mut runtime = EditorRuntime::default();
        let rendered = render_editor_with_config(
            &mut runtime,
            Some(1),
            0,
            "a\tb c",
            &[],
            EditorRenderConfig {
                tab_size: 4,
                show_whitespace: true,
                viewport_width: 400.0,
                ..EditorRenderConfig::default()
            },
        );
        let rendered_text = rendered
            .segments
            .iter()
            .map(|segment| segment.text.to_string())
            .collect::<String>();
        assert_eq!(rendered_text, "a\u{2192}  b\u{00b7}c");
        assert_eq!(runtime.hit_test(TEXT_LEFT + CHAR_WIDTH * 4.0, 0.0), 2);
    }

    #[test]
    fn viewport_rendering_keeps_only_visible_rows_plus_overscan() {
        let mut runtime = EditorRuntime::default();
        let text = (0..100).map(|line| format!("{line}\n")).collect::<String>();
        let rendered = render_editor_with_config(
            &mut runtime,
            Some(1),
            0,
            &text,
            &[],
            EditorRenderConfig {
                viewport_height: LINE_HEIGHT * 5.0,
                scroll_y: LINE_HEIGHT * 50.0,
                overscan_rows: 2,
                ..EditorRenderConfig::default()
            },
        );
        assert!(rendered.rows.len() <= 10);
        assert!(rendered.rows.first().unwrap().line_number >= 49);
        assert!(rendered.content_height > LINE_HEIGHT * 100.0);
    }

    fn apply_edit_to_text(text: &str, edit: &EditorEdit) -> String {
        let chars = text.chars().collect::<Vec<_>>();
        let mut output = chars[..edit.start].iter().collect::<String>();
        output.push_str(&edit.replacement);
        output.extend(chars[edit.end..].iter());
        output
    }

    fn assert_close(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() < 0.001,
            "expected {expected}, got {actual}"
        );
    }
}
