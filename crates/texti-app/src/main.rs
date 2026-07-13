use anyhow::{Result, bail};
use autoscroll::{AutoScrollConfig, auto_scroll_delta_per_tick};
use commands::{
    COMMANDS, Shortcut, command, is_palette_visible, resolved_shortcut, resolved_shortcut_label,
    shortcut_conflict,
};
use editor_surface::{
    EditorEdit, EditorRenderConfig, EditorRuntime, EditorViewState, render_editor_with_config,
};
use slint::winit_030::winit::event::{ElementState, MouseButton, WindowEvent};
use slint::winit_030::winit::keyboard::{Key, ModifiersState, NamedKey};
use slint::winit_030::{EventResult, WinitWindowAccessor};
use slint::{
    CloseRequestResponse, Color, ComponentHandle, ModelRc, SharedString, Timer, TimerMode, VecModel,
};
use std::cell::RefCell;
use std::ffi::{OsStr, OsString};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};
use texti_core::{AppSnapshot, AppState};
use texti_model::{
    BufferId, CloseDecision, CloseOutcome, ExplorerRow, FileConflictDecision, RecentFile,
    SaveOutcome, SaveState, SearchResult, SyntaxMode, TabInfo, ViewState,
};
use texti_settings::{SettingsStore, TextiSettings};

mod autoscroll;
mod commands;
mod editor_surface;

slint::include_modules!();

const HEADER_HEIGHT: f64 = 44.0;
const STATUS_HINT_HEIGHT: f64 = 24.0;
const LINE_GUTTER_WIDTH: f64 = 58.0;
const MINIMAP_WIDTH: f64 = 72.0;
const AUTOSCROLL_TICK_MS: u64 = 16;
const RECOVERY_DEBOUNCE_MS: u64 = 500;
const MULTI_CLICK_TIMEOUT_MS: u64 = 500;
const MULTI_CLICK_DISTANCE_PX: f32 = 5.0;
const WRAPPED_RESIZE_DEBOUNCE_MS: u64 = 50;

#[derive(Default)]
struct PointerClickTracker {
    last: Option<Instant>,
    x: f32,
    y: f32,
    count: u8,
}

impl PointerClickTracker {
    fn record(&mut self, x: f32, y: f32, extend: bool) -> u8 {
        let now = Instant::now();
        let close_in_time = self.last.is_some_and(|last| {
            now.saturating_duration_since(last) <= Duration::from_millis(MULTI_CLICK_TIMEOUT_MS)
        });
        let close_in_space = (x - self.x).abs() <= MULTI_CLICK_DISTANCE_PX
            && (y - self.y).abs() <= MULTI_CLICK_DISTANCE_PX;

        self.count = if !extend && close_in_time && close_in_space {
            self.count.saturating_add(1).min(3)
        } else {
            1
        };
        self.last = Some(now);
        self.x = x;
        self.y = y;
        self.count
    }

    fn reset(&mut self) {
        self.last = None;
        self.count = 0;
    }
}

enum CliAction {
    Run { paths: Vec<PathBuf> },
    Help,
    Version,
}

fn parse_cli_args(args: impl IntoIterator<Item = OsString>) -> Result<CliAction> {
    let mut paths = Vec::new();
    let mut positional_only = false;
    for arg in args {
        if positional_only {
            paths.push(PathBuf::from(arg));
            continue;
        }
        if arg == OsStr::new("--") {
            positional_only = true;
        } else if arg == OsStr::new("--help") || arg == OsStr::new("-h") {
            return Ok(CliAction::Help);
        } else if arg == OsStr::new("--version") || arg == OsStr::new("-V") {
            return Ok(CliAction::Version);
        } else if arg.to_string_lossy().starts_with('-') {
            bail!("unknown option: {}", arg.to_string_lossy());
        } else {
            paths.push(PathBuf::from(arg));
        }
    }
    Ok(CliAction::Run { paths })
}

fn print_help() {
    println!(
        "Texti {version}\n\nUSAGE:\n    texti [OPTIONS] [FILE_OR_FOLDER ...]\n\nOPTIONS:\n    -h, --help       Print help\n    -V, --version    Print version\n    --               Treat remaining arguments as paths",
        version = env!("CARGO_PKG_VERSION")
    );
}

#[derive(Default)]
struct FindRuntime {
    buffer_id: Option<BufferId>,
    revision: u64,
    query: String,
    matches: Vec<SearchResult>,
    current: Option<usize>,
}

#[derive(Clone)]
struct SessionSaver {
    timer: Rc<Timer>,
    state: Rc<RefCell<AppState>>,
    editor: Rc<RefCell<EditorRuntime>>,
}

impl SessionSaver {
    fn new(state: Rc<RefCell<AppState>>, editor: Rc<RefCell<EditorRuntime>>) -> Self {
        Self {
            timer: Rc::new(Timer::default()),
            state,
            editor,
        }
    }

    fn schedule(&self) {
        let state = self.state.clone();
        let editor = self.editor.clone();
        self.timer.start(
            TimerMode::SingleShot,
            Duration::from_millis(RECOVERY_DEBOUNCE_MS),
            move || {
                let views = model_view_states(&editor.borrow());
                let result = state.borrow_mut().persist_session_with_view_states(&views);
                if let Err(error) = result {
                    state
                        .borrow_mut()
                        .set_message(format!("Recovery save failed: {error:#}"));
                }
            },
        );
    }

    fn flush(&self) -> Result<()> {
        self.timer.stop();
        let views = model_view_states(&self.editor.borrow());
        self.state
            .borrow_mut()
            .persist_session_with_view_states(&views)
    }
}

fn main() -> Result<()> {
    let startup_paths = match parse_cli_args(std::env::args_os().skip(1))? {
        CliAction::Help => {
            print_help();
            return Ok(());
        }
        CliAction::Version => {
            println!("Texti {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        CliAction::Run { paths } => paths,
    };
    if let Err(error) = slint::BackendSelector::new()
        .backend_name("winit".into())
        .select()
    {
        bail!("Texti requires the Winit backend: {error}");
    }
    let ui = AppWindow::new()?;
    let settings_store = SettingsStore::discover()?;
    let state = Rc::new(RefCell::new(AppState::new(settings_store)?));
    let editor = Rc::new(RefCell::new(EditorRuntime::default()));
    let find = Rc::new(RefCell::new(FindRuntime::default()));
    if !startup_paths.is_empty() {
        let startup_result = state.borrow_mut().open_startup_paths(startup_paths);
        if let Err(error) = startup_result {
            state
                .borrow_mut()
                .set_message(format!("Error opening startup file: {error:#}"));
        }
    }
    restore_editor_views(&state.borrow(), &mut editor.borrow_mut());
    ui.set_app_version(env!("CARGO_PKG_VERSION").into());
    let session_saver = SessionSaver::new(state.clone(), editor.clone());
    refresh_ui(&ui, &state.borrow(), &editor);
    refresh_command_rows(&ui, state.borrow().settings(), "");
    refresh_command_settings_rows(&ui, state.borrow().settings(), "", "");
    wire_callbacks(
        &ui,
        state.clone(),
        editor.clone(),
        find,
        session_saver.clone(),
    );
    wire_autoscroll(&ui, state.clone(), editor.clone(), session_saver.clone());
    ui.run()?;
    session_saver.flush()?;
    Ok(())
}

fn wire_callbacks(
    ui: &AppWindow,
    state: Rc<RefCell<AppState>>,
    editor: Rc<RefCell<EditorRuntime>>,
    find: Rc<RefCell<FindRuntime>>,
    session_saver: SessionSaver,
) {
    let weak = ui.as_weak();
    let pending_quit = Rc::new(RefCell::new(false));
    let pending_conflict_close = Rc::new(RefCell::new(None::<BufferId>));
    let pointer_clicks = Rc::new(RefCell::new(PointerClickTracker::default()));
    ui.on_request_minimize({
        let weak = weak.clone();
        move || {
            if let Some(ui) = weak.upgrade() {
                ui.window().set_minimized(true);
            }
        }
    });

    ui.on_request_toggle_maximize({
        let weak = weak.clone();
        move || {
            if let Some(ui) = weak.upgrade() {
                let maximized = ui.window().is_maximized();
                ui.window().set_maximized(!maximized);
            }
        }
    });

    ui.on_request_begin_window_drag({
        let weak = weak.clone();
        move || {
            if let Some(ui) = weak.upgrade() {
                let _ = start_native_window_drag(ui.window());
            }
        }
    });

    ui.on_request_close({
        let weak = weak.clone();
        let state = state.clone();
        let session_saver = session_saver.clone();
        let pending_quit = pending_quit.clone();
        move || {
            if let Some(ui) = weak.upgrade() {
                begin_window_close(&ui, &state, &session_saver, &pending_quit);
            }
        }
    });

    ui.on_request_toggle_focus_mode({
        let weak = weak.clone();
        move || {
            if let Some(ui) = weak.upgrade() {
                let fullscreen = ui.window().is_fullscreen();
                ui.window().set_fullscreen(!fullscreen);
            }
        }
    });

    ui.window().on_close_requested({
        let weak = weak.clone();
        let state = state.clone();
        let session_saver = session_saver.clone();
        let pending_quit = pending_quit.clone();
        move || {
            let Some(ui) = weak.upgrade() else {
                return CloseRequestResponse::HideWindow;
            };
            if can_close_window_now(&state, &session_saver) {
                CloseRequestResponse::HideWindow
            } else {
                *pending_quit.borrow_mut() = true;
                show_first_dirty_dialog(&ui, &state);
                CloseRequestResponse::KeepWindowShown
            }
        }
    });

    ui.on_request_new({
        let weak = weak.clone();
        let state = state.clone();
        let editor = editor.clone();
        let session_saver = session_saver.clone();
        move || {
            run_action(&weak, &state, &editor, |state| {
                state.new_untitled();
                Ok(())
            });
            session_saver.schedule();
        }
    });

    ui.on_request_open_file({
        let weak = weak.clone();
        let state = state.clone();
        let editor = editor.clone();
        let session_saver = session_saver.clone();
        move || {
            let Some(paths) = rfd::FileDialog::new()
                .set_title("Open file in Texti")
                .pick_files()
            else {
                return;
            };
            run_action(&weak, &state, &editor, |state| {
                for path in paths {
                    state.open_file(path)?;
                }
                Ok(())
            });
            session_saver.schedule();
        }
    });

    ui.on_request_open_folder({
        let weak = weak.clone();
        let state = state.clone();
        let editor = editor.clone();
        let session_saver = session_saver.clone();
        move || {
            let Some(path) = rfd::FileDialog::new()
                .set_title("Open folder in Texti")
                .pick_folder()
            else {
                return;
            };
            run_action(&weak, &state, &editor, |state| state.open_folder(path));
            if let Some(ui) = weak.upgrade() {
                refresh_workspace_query(&ui, &state.borrow(), "");
                ui.invoke_show_workspace();
            }
            session_saver.schedule();
        }
    });

    ui.on_request_save({
        let weak = weak.clone();
        let state = state.clone();
        let editor = editor.clone();
        let session_saver = session_saver.clone();
        move || {
            let id = state.borrow().snapshot().active_buffer_id;
            if let (Some(ui), Some(id)) = (weak.upgrade(), id) {
                save_buffer_or_prompt(&ui, &state, &editor, id);
                session_saver.schedule();
            }
        }
    });

    ui.on_request_save_as({
        let weak = weak.clone();
        let state = state.clone();
        let editor = editor.clone();
        let session_saver = session_saver.clone();
        move || {
            let Some(path) = rfd::FileDialog::new().set_title("Save file as").save_file() else {
                return;
            };
            run_action(&weak, &state, &editor, |state| state.save_active_as(path));
            session_saver.schedule();
        }
    });

    ui.on_request_reload({
        let weak = weak.clone();
        let state = state.clone();
        let editor = editor.clone();
        move || {
            run_action(&weak, &state, &editor, AppState::reload_active_from_disk);
        }
    });

    ui.on_request_undo({
        let weak = weak.clone();
        let state = state.clone();
        let editor = editor.clone();
        let session_saver = session_saver.clone();
        move || {
            run_action(&weak, &state, &editor, AppState::undo_active);
            session_saver.schedule();
        }
    });

    ui.on_request_redo({
        let weak = weak.clone();
        let state = state.clone();
        let editor = editor.clone();
        let session_saver = session_saver.clone();
        move || {
            run_action(&weak, &state, &editor, AppState::redo_active);
            session_saver.schedule();
        }
    });

    ui.on_request_copy({
        let state = state.clone();
        let editor = editor.clone();
        move || {
            copy_selection(&state, &editor);
        }
    });

    ui.on_request_cut({
        let weak = weak.clone();
        let state = state.clone();
        let editor = editor.clone();
        let session_saver = session_saver.clone();
        move || {
            cut_selection(&weak, &state, &editor, &session_saver);
        }
    });

    ui.on_request_paste({
        let weak = weak.clone();
        let state = state.clone();
        let editor = editor.clone();
        let session_saver = session_saver.clone();
        move || {
            paste_clipboard(&weak, &state, &editor, &session_saver);
        }
    });

    ui.on_request_select_all({
        let weak = weak.clone();
        let state = state.clone();
        let editor = editor.clone();
        move || {
            let text = state.borrow().snapshot().editor_text;
            editor.borrow_mut().select_all(&text);
            if let Some(ui) = weak.upgrade() {
                refresh_ui(&ui, &state.borrow(), &editor);
            }
        }
    });

    ui.on_request_close_tab({
        let weak = weak.clone();
        let state = state.clone();
        let editor = editor.clone();
        let session_saver = session_saver.clone();
        move || {
            let id = state.borrow().snapshot().active_buffer_id;
            if let (Some(ui), Some(id)) = (weak.upgrade(), id) {
                request_close_buffer(&ui, &state, &editor, id, &session_saver);
            }
        }
    });

    ui.on_request_close_tab_by_id({
        let weak = weak.clone();
        let state = state.clone();
        let editor = editor.clone();
        let session_saver = session_saver.clone();
        move |id| {
            if let Some(ui) = weak.upgrade() {
                request_close_buffer(&ui, &state, &editor, id as BufferId, &session_saver);
            }
        }
    });

    ui.on_request_next_tab({
        let weak = weak.clone();
        let state = state.clone();
        let editor = editor.clone();
        move || cycle_tab(&weak, &state, &editor, 1)
    });

    ui.on_request_previous_tab({
        let weak = weak.clone();
        let state = state.clone();
        let editor = editor.clone();
        move || cycle_tab(&weak, &state, &editor, -1)
    });

    ui.on_request_toggle_hidden({
        let weak = weak.clone();
        let state = state.clone();
        let editor = editor.clone();
        move || {
            run_action(&weak, &state, &editor, AppState::toggle_hidden_files);
        }
    });

    ui.on_request_toggle_line_numbers({
        let weak = weak.clone();
        let state = state.clone();
        let editor = editor.clone();
        move || {
            run_action(&weak, &state, &editor, AppState::toggle_line_numbers);
        }
    });

    ui.on_request_toggle_word_wrap({
        let weak = weak.clone();
        let state = state.clone();
        let editor = editor.clone();
        move || {
            run_action(&weak, &state, &editor, AppState::toggle_word_wrap);
        }
    });

    ui.on_request_toggle_status_hints({
        let weak = weak.clone();
        let state = state.clone();
        let editor = editor.clone();
        move || {
            run_action(&weak, &state, &editor, AppState::toggle_status_hints);
        }
    });

    ui.on_request_toggle_syntax_highlighting({
        let weak = weak.clone();
        let state = state.clone();
        let editor = editor.clone();
        move || {
            run_action(&weak, &state, &editor, AppState::toggle_syntax_highlighting);
        }
    });

    ui.on_request_toggle_recovery_autosave({
        let weak = weak.clone();
        let state = state.clone();
        let editor = editor.clone();
        move || {
            run_action(&weak, &state, &editor, AppState::toggle_recovery_autosave);
        }
    });

    ui.on_request_toggle_confirm_trash({
        let weak = weak.clone();
        let state = state.clone();
        let editor = editor.clone();
        move || {
            run_action(&weak, &state, &editor, AppState::toggle_confirm_trash);
        }
    });

    ui.on_request_set_font_size({
        let weak = weak.clone();
        let state = state.clone();
        let editor = editor.clone();
        move |size| {
            run_action(&weak, &state, &editor, |state| {
                state.set_font_size(size.max(0) as u32)
            });
        }
    });

    ui.on_request_set_tab_size({
        let weak = weak.clone();
        let state = state.clone();
        let editor = editor.clone();
        move |size| {
            run_action(&weak, &state, &editor, |state| {
                state.set_tab_size(size.max(0) as u8)
            });
        }
    });

    ui.on_request_set_insert_spaces({
        let weak = weak.clone();
        let state = state.clone();
        let editor = editor.clone();
        move |insert_spaces| {
            run_action(&weak, &state, &editor, |state| {
                state.set_insert_spaces(insert_spaces)
            });
        }
    });

    ui.on_request_set_show_minimap({
        let weak = weak.clone();
        let state = state.clone();
        let editor = editor.clone();
        move |show| {
            run_action(&weak, &state, &editor, |state| {
                if state.settings().show_minimap != show {
                    state.toggle_minimap()?;
                }
                Ok(())
            });
        }
    });

    ui.on_request_set_show_whitespace({
        let weak = weak.clone();
        let state = state.clone();
        let editor = editor.clone();
        move |show| {
            run_action(&weak, &state, &editor, |state| {
                if state.settings().show_whitespace != show {
                    state.toggle_whitespace()?;
                }
                Ok(())
            });
        }
    });

    ui.on_request_set_syntax_mode({
        let weak = weak.clone();
        let state = state.clone();
        let editor = editor.clone();
        move |mode| {
            let mode = syntax_mode_from_label(&mode);
            run_action(&weak, &state, &editor, |state| state.set_syntax_mode(mode));
        }
    });

    ui.on_request_create_file({
        let weak = weak.clone();
        let state = state.clone();
        let editor = editor.clone();
        move |name| {
            let name = name.to_string();
            run_action(&weak, &state, &editor, |state| {
                state.create_file_in_selection(name.trim())
            });
        }
    });

    ui.on_request_create_folder({
        let weak = weak.clone();
        let state = state.clone();
        let editor = editor.clone();
        move |name| {
            let name = name.to_string();
            run_action(&weak, &state, &editor, |state| {
                state.create_folder_in_selection(name.trim())
            });
        }
    });

    ui.on_request_rename({
        let weak = weak.clone();
        let state = state.clone();
        let editor = editor.clone();
        move |name| {
            let name = name.to_string();
            run_action(&weak, &state, &editor, |state| {
                state.rename_selected(name.trim())
            });
        }
    });

    ui.on_request_trash_confirmed({
        let weak = weak.clone();
        let state = state.clone();
        let editor = editor.clone();
        move |confirmed| {
            run_action(&weak, &state, &editor, |state| {
                state.trash_selected_confirmed(confirmed)
            });
        }
    });

    ui.on_explorer_activated({
        let weak = weak.clone();
        let state = state.clone();
        let editor = editor.clone();
        move |path| {
            let path = PathBuf::from(path.to_string());
            run_action(&weak, &state, &editor, |state| state.select_or_open(path));
        }
    });

    ui.on_tab_activated({
        let weak = weak.clone();
        let state = state.clone();
        let editor = editor.clone();
        move |id| {
            activate_tab(&weak, &state, &editor, id as BufferId);
        }
    });

    ui.on_recent_activated({
        let weak = weak.clone();
        let state = state.clone();
        let editor = editor.clone();
        let session_saver = session_saver.clone();
        move |path| {
            let path = PathBuf::from(path.to_string());
            run_action(&weak, &state, &editor, |state| {
                state.open_file(path)?;
                Ok(())
            });
            session_saver.schedule();
        }
    });

    ui.on_text_changed({
        let weak = weak.clone();
        let state = state.clone();
        let editor = editor.clone();
        let session_saver = session_saver.clone();
        move |text| {
            let text = text.to_string();
            run_action(&weak, &state, &editor, |state| {
                state.update_active_text(text)
            });
            session_saver.schedule();
        }
    });

    ui.on_editor_pointer_down({
        let weak = weak.clone();
        let state = state.clone();
        let editor = editor.clone();
        let pointer_clicks = pointer_clicks.clone();
        move |x, y, extend| {
            let text = state.borrow().snapshot().editor_text;
            let click_count = pointer_clicks.borrow_mut().record(x, y, extend);
            match click_count {
                2 => editor.borrow_mut().pointer_select_word(&text, x, y),
                3 => {
                    editor.borrow_mut().pointer_select_line(&text, x, y);
                    pointer_clicks.borrow_mut().reset();
                }
                _ => editor.borrow_mut().pointer_down(&text, x, y, extend),
            }
            if let Some(ui) = weak.upgrade() {
                refresh_ui(&ui, &state.borrow(), &editor);
            }
        }
    });

    ui.on_editor_pointer_drag({
        let weak = weak.clone();
        let state = state.clone();
        let editor = editor.clone();
        move |x, y| {
            let text = state.borrow().snapshot().editor_text;
            editor.borrow_mut().pointer_drag(&text, x, y);
            if let Some(ui) = weak.upgrade() {
                refresh_ui(&ui, &state.borrow(), &editor);
            }
        }
    });

    ui.on_search_requested({
        let weak = weak.clone();
        let state = state.clone();
        let editor = editor.clone();
        let find = find.clone();
        move |query| {
            let query = query.to_string();
            run_search(&weak, &state, &editor, &find, query.trim(), 0);
        }
    });

    ui.on_search_next_requested({
        let weak = weak.clone();
        let state = state.clone();
        let editor = editor.clone();
        let find = find.clone();
        move |query| {
            let query = query.to_string();
            run_search(&weak, &state, &editor, &find, query.trim(), 1);
        }
    });

    ui.on_search_previous_requested({
        let weak = weak.clone();
        let state = state.clone();
        let editor = editor.clone();
        let find = find.clone();
        move |query| {
            let query = query.to_string();
            run_search(&weak, &state, &editor, &find, query.trim(), -1);
        }
    });

    ui.on_search_result_activated({
        let weak = weak.clone();
        let state = state.clone();
        let editor = editor.clone();
        let find = find.clone();
        move |index| {
            select_find_result(&weak, &state, &editor, &find, index.max(0) as usize);
        }
    });

    ui.on_replace_next_requested({
        let weak = weak.clone();
        let state = state.clone();
        let editor = editor.clone();
        let session_saver = session_saver.clone();
        move |query, replacement| {
            let query = query.to_string();
            let replacement = replacement.to_string();
            let mut selected = None;
            let mut selected_line = None;
            run_action(&weak, &state, &editor, |state| {
                selected = state
                    .replace_active_next(query.trim(), replacement.as_str())?
                    .map(|m| {
                        selected_line = Some(m.line_number);
                        m.absolute_byte_start as i32
                    });
                Ok(())
            });
            if let (Some(ui), Some(offset)) = (weak.upgrade(), selected) {
                select_byte_range_from_snapshot(&state, &editor, offset as usize, offset as usize);
                refresh_ui(&ui, &state.borrow(), &editor);
                if let Some(line_number) = selected_line {
                    scroll_line_into_view(&ui, line_number);
                }
            }
            session_saver.schedule();
        }
    });

    ui.on_replace_all_requested({
        let weak = weak.clone();
        let state = state.clone();
        let editor = editor.clone();
        let session_saver = session_saver.clone();
        move |query, replacement| {
            let query = query.to_string();
            let replacement = replacement.to_string();
            run_action(&weak, &state, &editor, |state| {
                state.replace_active_all(query.trim(), replacement.as_str())?;
                Ok(())
            });
            session_saver.schedule();
        }
    });

    ui.on_goto_line_requested({
        let weak = weak.clone();
        let state = state.clone();
        let editor = editor.clone();
        move |line| {
            let line = line.to_string();
            let parsed = line.trim().parse::<usize>().unwrap_or(0);
            let mut selected = None;
            run_action(&weak, &state, &editor, |state| {
                selected = state.go_to_line_active(parsed)?.map(|offset| offset as i32);
                Ok(())
            });
            if let (Some(ui), Some(offset)) = (weak.upgrade(), selected) {
                select_byte_range_from_snapshot(&state, &editor, offset as usize, offset as usize);
                refresh_ui(&ui, &state.borrow(), &editor);
                scroll_line_into_view(&ui, parsed);
            }
        }
    });

    ui.on_workspace_query_changed({
        let weak = weak.clone();
        let state = state.clone();
        move |query| {
            if let Some(ui) = weak.upgrade() {
                refresh_workspace_query(&ui, &state.borrow(), &query);
            }
        }
    });

    ui.on_command_query_changed({
        let weak = weak.clone();
        let state = state.clone();
        move |query| {
            if let Some(ui) = weak.upgrade() {
                refresh_command_rows(&ui, state.borrow().settings(), &query);
            }
        }
    });

    ui.on_command_activated({
        let weak = weak.clone();
        move |id| {
            if let Some(ui) = weak.upgrade() {
                execute_command(&ui, &id);
            }
        }
    });

    ui.on_command_settings_query_changed({
        let weak = weak.clone();
        let state = state.clone();
        move |query| {
            if let Some(ui) = weak.upgrade() {
                refresh_command_settings_rows(
                    &ui,
                    state.borrow().settings(),
                    &query,
                    &ui.get_shortcut_capture_id(),
                );
            }
        }
    });

    ui.on_command_visibility_changed({
        let weak = weak.clone();
        let state = state.clone();
        let editor = editor.clone();
        move |id, visible| {
            let id = id.to_string();
            if command(&id).is_none() {
                return;
            }
            run_action(&weak, &state, &editor, |state| {
                state.set_command_palette_visibility(&id, visible)
            });
            if let Some(ui) = weak.upgrade() {
                refresh_command_models(&ui, state.borrow().settings());
            }
        }
    });

    ui.on_shortcut_capture_requested({
        let weak = weak.clone();
        let state = state.clone();
        move |id| {
            let id = id.to_string();
            if command(&id).is_none() {
                return;
            }
            if let Some(ui) = weak.upgrade() {
                ui.set_shortcut_capture_id(id.clone().into());
                ui.set_shortcut_error("".into());
                refresh_command_settings_rows(
                    &ui,
                    state.borrow().settings(),
                    &ui.get_command_settings_query(),
                    &id,
                );
            }
        }
    });

    ui.on_shortcut_clear_requested({
        let weak = weak.clone();
        let state = state.clone();
        let editor = editor.clone();
        move |id| {
            let id = id.to_string();
            if command(&id).is_none() {
                return;
            }
            run_action(&weak, &state, &editor, |state| {
                state.set_command_shortcut_override(&id, None)
            });
            if let Some(ui) = weak.upgrade() {
                ui.set_shortcut_capture_id("".into());
                ui.set_shortcut_error("".into());
                refresh_command_models(&ui, state.borrow().settings());
            }
        }
    });

    ui.on_command_customizations_reset({
        let weak = weak.clone();
        let state = state.clone();
        let editor = editor.clone();
        move || {
            run_action(
                &weak,
                &state,
                &editor,
                AppState::reset_command_customizations,
            );
            if let Some(ui) = weak.upgrade() {
                ui.set_shortcut_capture_id("".into());
                ui.set_shortcut_error("".into());
                refresh_command_models(&ui, state.borrow().settings());
            }
        }
    });

    ui.on_minimap_scroll_requested({
        let weak = weak.clone();
        let state = state.clone();
        let editor = editor.clone();
        move |position| {
            if let Some(ui) = weak.upgrade() {
                let max_y = (ui.get_editor_content_height() - editor_view_height(&ui)).max(0.0);
                let y = max_y * position.clamp(0.0, 1.0);
                ui.invoke_set_editor_viewport_y(y);
                editor
                    .borrow_mut()
                    .set_viewport(ui.get_editor_viewport_x(), y);
                refresh_ui(&ui, &state.borrow(), &editor);
            }
        }
    });

    ui.on_dirty_decision({
        let weak = weak.clone();
        let state = state.clone();
        let editor = editor.clone();
        let session_saver = session_saver.clone();
        let pending_quit = pending_quit.clone();
        let pending_conflict_close = pending_conflict_close.clone();
        move |id, decision| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let id = id as BufferId;
            let decision = match decision.as_str() {
                "save" => CloseDecision::Save,
                "discard" => CloseDecision::Discard,
                _ => CloseDecision::Cancel,
            };
            if decision == CloseDecision::Cancel {
                *pending_quit.borrow_mut() = false;
                return;
            }
            let outcome = state.borrow_mut().close_buffer_with_decision(id, decision);
            match outcome {
                Ok(CloseOutcome::SaveAsRequired { .. }) => {
                    let Some(path) = rfd::FileDialog::new().set_title("Save file as").save_file()
                    else {
                        *pending_quit.borrow_mut() = false;
                        return;
                    };
                    let save_result = state.borrow_mut().save_buffer_as(id, path);
                    if let Err(error) = save_result {
                        state.borrow_mut().set_message(format!("Error: {error:#}"));
                    } else {
                        let _ = state.borrow_mut().request_close_buffer(id);
                        editor.borrow_mut().remove_view_state(id);
                    }
                }
                Ok(CloseOutcome::Conflict(conflict)) => {
                    *pending_conflict_close.borrow_mut() = Some(id);
                    show_conflict_dialog(&ui, id, &conflict.path);
                    return;
                }
                Ok(CloseOutcome::Closed { .. }) => {
                    editor.borrow_mut().remove_view_state(id);
                }
                Ok(CloseOutcome::Cancelled { .. } | CloseOutcome::NeedsDecision { .. }) => {
                    *pending_quit.borrow_mut() = false;
                }
                Err(error) => state.borrow_mut().set_message(format!("Error: {error:#}")),
            }
            finish_close_flow(&ui, &state, &editor, &session_saver, &pending_quit);
        }
    });

    ui.on_conflict_decision({
        let weak = weak.clone();
        let state = state.clone();
        let editor = editor.clone();
        let session_saver = session_saver.clone();
        let pending_quit = pending_quit.clone();
        let pending_conflict_close = pending_conflict_close.clone();
        move |id, decision| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let id = id as BufferId;
            let decision = match decision.as_str() {
                "reload" => FileConflictDecision::Reload,
                "overwrite" => FileConflictDecision::Overwrite,
                "save-as" => {
                    let Some(path) = rfd::FileDialog::new().set_title("Save file as").save_file()
                    else {
                        return;
                    };
                    FileConflictDecision::SaveAs(path)
                }
                _ => FileConflictDecision::Cancel,
            };
            let outcome = state.borrow_mut().resolve_file_conflict(id, decision);
            match outcome {
                Ok(SaveOutcome::Saved { .. } | SaveOutcome::Reloaded { .. }) => {
                    if pending_conflict_close.borrow_mut().take() == Some(id) {
                        let _ = state.borrow_mut().request_close_buffer(id);
                        editor.borrow_mut().remove_view_state(id);
                    }
                }
                Ok(SaveOutcome::Cancelled) => {
                    *pending_conflict_close.borrow_mut() = None;
                    *pending_quit.borrow_mut() = false;
                }
                Ok(SaveOutcome::Conflict(conflict)) => {
                    show_conflict_dialog(&ui, id, &conflict.path);
                    return;
                }
                Ok(SaveOutcome::SaveAsRequired { .. }) => {}
                Err(error) => state.borrow_mut().set_message(format!("Error: {error:#}")),
            }
            finish_close_flow(&ui, &state, &editor, &session_saver, &pending_quit);
        }
    });
}

#[derive(Default)]
struct AutoScrollRuntime {
    active: bool,
    anchor_y: f64,
    cursor_y: f64,
    last_cursor_x: f64,
    last_cursor_y: f64,
    has_cursor: bool,
    modifiers: ModifiersState,
}

#[derive(Default)]
struct ResizeRuntime {
    redraw_pending: bool,
}

fn wire_autoscroll(
    ui: &AppWindow,
    state: Rc<RefCell<AppState>>,
    editor: Rc<RefCell<EditorRuntime>>,
    session_saver: SessionSaver,
) {
    let weak = ui.as_weak();
    let runtime = Rc::new(RefCell::new(AutoScrollRuntime::default()));
    let timer = Rc::new(Timer::default());
    let resize_runtime = Rc::new(RefCell::new(ResizeRuntime::default()));
    let resize_timer = Rc::new(Timer::default());
    let config = AutoScrollConfig::default();

    ui.on_request_cancel_autoscroll({
        let weak = weak.clone();
        let runtime = runtime.clone();
        let timer = timer.clone();
        move || {
            stop_autoscroll(&weak, &runtime, &timer);
        }
    });

    ui.window().on_winit_window_event({
        let weak = weak.clone();
        let runtime = runtime.clone();
        let timer = timer.clone();
        let state = state.clone();
        let editor = editor.clone();
        let session_saver = session_saver.clone();
        let resize_runtime = resize_runtime.clone();
        let resize_timer = resize_timer.clone();
        move |window, event| {
            let Some(ui) = weak.upgrade() else {
                return EventResult::Propagate;
            };

            match event {
                WindowEvent::Resized(_) => {
                    if state.borrow().settings().word_wrap {
                        resize_runtime.borrow_mut().redraw_pending = false;
                        let weak = weak.clone();
                        let state = state.clone();
                        let editor = editor.clone();
                        let resize_runtime = resize_runtime.clone();
                        resize_timer.start(
                            TimerMode::SingleShot,
                            Duration::from_millis(WRAPPED_RESIZE_DEBOUNCE_MS),
                            move || {
                                resize_runtime.borrow_mut().redraw_pending = false;
                                if let Some(ui) = weak.upgrade() {
                                    refresh_editor_after_resize(&ui, &state.borrow(), &editor);
                                }
                            },
                        );
                    } else {
                        resize_timer.stop();
                        resize_runtime.borrow_mut().redraw_pending = true;
                    }
                    window.request_redraw();
                    EventResult::Propagate
                }
                WindowEvent::ScaleFactorChanged { .. } => {
                    resize_timer.stop();
                    resize_runtime.borrow_mut().redraw_pending = true;
                    window.request_redraw();
                    EventResult::Propagate
                }
                WindowEvent::RedrawRequested => {
                    let pending = {
                        let mut runtime = resize_runtime.borrow_mut();
                        let pending = runtime.redraw_pending;
                        runtime.redraw_pending = false;
                        pending
                    };
                    if pending {
                        refresh_editor_after_resize(&ui, &state.borrow(), &editor);
                    }
                    EventResult::Propagate
                }
                WindowEvent::DroppedFile(path) => {
                    open_dropped_path(&weak, &state, &editor, path.clone());
                    session_saver.schedule();
                    EventResult::PreventDefault
                }
                WindowEvent::Focused(true) => {
                    let conflicts = state.borrow_mut().check_external_changes();
                    match conflicts {
                        Ok(conflicts) => {
                            refresh_ui(&ui, &state.borrow(), &editor);
                            if let Some(conflict) = conflicts.first() {
                                show_conflict_dialog(&ui, conflict.buffer_id, &conflict.path);
                            }
                        }
                        Err(error) => state
                            .borrow_mut()
                            .set_message(format!("File check failed: {error:#}")),
                    }
                    EventResult::Propagate
                }
                WindowEvent::ModifiersChanged(modifiers) => {
                    runtime.borrow_mut().modifiers = modifiers.state();
                    EventResult::Propagate
                }
                WindowEvent::CursorMoved { position, .. } => {
                    let position = position.to_logical::<f64>(f64::from(window.scale_factor()));
                    let mut session = runtime.borrow_mut();
                    session.last_cursor_x = position.x;
                    session.last_cursor_y = position.y;
                    session.cursor_y = position.y;
                    session.has_cursor = true;
                    EventResult::Propagate
                }
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    button: MouseButton::Middle,
                    ..
                } => {
                    let active = runtime.borrow().active;
                    if active {
                        stop_autoscroll(&weak, &runtime, &timer);
                        return EventResult::PreventDefault;
                    }

                    let (x, y, has_cursor) = {
                        let session = runtime.borrow();
                        (
                            session.last_cursor_x,
                            session.last_cursor_y,
                            session.has_cursor,
                        )
                    };
                    if has_cursor && can_start_autoscroll_at(&ui, x, y) {
                        start_autoscroll(&ui, &weak, &runtime, &timer, config, x, y);
                        return EventResult::PreventDefault;
                    }

                    EventResult::Propagate
                }
                WindowEvent::MouseInput {
                    state: ElementState::Released,
                    button: MouseButton::Middle,
                    ..
                } => {
                    let active = runtime.borrow().active;
                    if active {
                        EventResult::PreventDefault
                    } else {
                        EventResult::Propagate
                    }
                }
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                    ..
                } => {
                    let active = runtime.borrow().active;
                    if active {
                        stop_autoscroll(&weak, &runtime, &timer);
                    }
                    EventResult::Propagate
                }
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    button: MouseButton::Right,
                    ..
                } => {
                    let active = runtime.borrow().active;
                    if active {
                        stop_autoscroll(&weak, &runtime, &timer);
                    }
                    let (x, y, has_cursor) = {
                        let session = runtime.borrow();
                        (
                            session.last_cursor_x,
                            session.last_cursor_y,
                            session.has_cursor,
                        )
                    };
                    if has_cursor && can_start_autoscroll_at(&ui, x, y) {
                        ui.invoke_show_editor_context_menu(x as f32, y as f32);
                        EventResult::PreventDefault
                    } else {
                        EventResult::Propagate
                    }
                }
                WindowEvent::MouseWheel { .. } => {
                    let active = runtime.borrow().active;
                    if active {
                        stop_autoscroll(&weak, &runtime, &timer);
                    }
                    let weak = weak.clone();
                    let state = state.clone();
                    let editor = editor.clone();
                    Timer::single_shot(Duration::from_millis(0), move || {
                        if let Some(ui) = weak.upgrade() {
                            editor.borrow_mut().set_viewport(
                                ui.get_editor_viewport_x(),
                                ui.get_editor_viewport_y(),
                            );
                            refresh_ui(&ui, &state.borrow(), &editor);
                        }
                    });
                    EventResult::Propagate
                }
                WindowEvent::KeyboardInput { event, .. }
                    if event.state == ElementState::Pressed =>
                {
                    let modifiers = runtime.borrow().modifiers;
                    if !ui.get_shortcut_capture_id().is_empty() {
                        handle_shortcut_capture(&ui, &state, &editor, event, modifiers);
                        return EventResult::PreventDefault;
                    }
                    if matches!(event.logical_key, Key::Named(NamedKey::Escape)) {
                        let active = runtime.borrow().active;
                        if active {
                            stop_autoscroll(&weak, &runtime, &timer);
                        }
                        ui.invoke_dismiss_active_overlay();
                        return EventResult::PreventDefault;
                    }
                    if blocking_modal_visible(&ui) {
                        return EventResult::Propagate;
                    }
                    if matches!(event.logical_key, Key::Named(NamedKey::F1)) {
                        ui.invoke_show_command_palette();
                        return EventResult::PreventDefault;
                    }
                    let matched_command = {
                        let state = state.borrow();
                        command_for_key_event(state.settings(), &event.logical_key, modifiers)
                    };
                    if let Some(command) = matched_command {
                        if text_input_has_priority(&ui) && is_text_edit_command(command.id) {
                            return EventResult::Propagate;
                        }
                        ui.invoke_close_overlays();
                        execute_command(&ui, command.id);
                        return EventResult::PreventDefault;
                    }
                    if overlays_visible(&ui) {
                        return EventResult::Propagate;
                    }
                    if handle_editor_key_event(
                        &weak,
                        &state,
                        &editor,
                        &session_saver,
                        event,
                        modifiers,
                    ) {
                        EventResult::PreventDefault
                    } else {
                        EventResult::Propagate
                    }
                }
                _ => EventResult::Propagate,
            }
        }
    });
}

fn start_autoscroll(
    ui: &AppWindow,
    weak: &slint::Weak<AppWindow>,
    runtime: &Rc<RefCell<AutoScrollRuntime>>,
    timer: &Rc<Timer>,
    config: AutoScrollConfig,
    x: f64,
    y: f64,
) {
    {
        let mut session = runtime.borrow_mut();
        session.active = true;
        session.anchor_y = y;
        session.cursor_y = y;
    }

    ui.set_autoscroll_marker_x(editor_local_x(ui, x) as f32);
    ui.set_autoscroll_marker_y((y - editor_top_offset()).max(0.0) as f32);
    ui.set_autoscroll_active(true);

    let weak = weak.clone();
    let runtime = runtime.clone();
    let timer_for_callback = timer.clone();
    timer.start(
        TimerMode::Repeated,
        Duration::from_millis(AUTOSCROLL_TICK_MS),
        move || {
            let Some(ui) = weak.upgrade() else {
                timer_for_callback.stop();
                return;
            };
            if overlays_visible(&ui) {
                stop_autoscroll(&weak, &runtime, &timer_for_callback);
                return;
            }
            let delta = {
                let session = runtime.borrow();
                auto_scroll_delta_per_tick(
                    session.active,
                    session.anchor_y,
                    session.cursor_y,
                    config,
                )
            };
            if delta != 0.0 {
                ui.invoke_scroll_editor_by(delta);
            }
        },
    );
}

fn stop_autoscroll(
    weak: &slint::Weak<AppWindow>,
    runtime: &Rc<RefCell<AutoScrollRuntime>>,
    timer: &Timer,
) {
    runtime.borrow_mut().active = false;
    timer.stop();
    if let Some(ui) = weak.upgrade() {
        ui.set_autoscroll_active(false);
    }
}

fn can_start_autoscroll_at(ui: &AppWindow, x: f64, y: f64) -> bool {
    if overlays_visible(ui) {
        return false;
    }
    let size = ui.window().size();
    let scale = ui.window().scale_factor();
    let logical = slint::LogicalSize::from_physical(size, scale);
    let status_height = if ui.get_status_hints() {
        STATUS_HINT_HEIGHT
    } else {
        0.0
    };
    x >= 0.0
        && x <= f64::from(logical.width)
        && y >= editor_top_offset()
        && y <= f64::from(logical.height) - status_height
}

fn start_native_window_drag(window: &slint::Window) -> bool {
    match window.with_winit_window(|window| window.drag_window()) {
        Some(Ok(())) => true,
        Some(Err(error)) => {
            eprintln!("Texti: failed to start native window drag: {error}");
            false
        }
        None => {
            eprintln!("Texti: failed to start native window drag: winit window unavailable");
            false
        }
    }
}

fn editor_top_offset() -> f64 {
    HEADER_HEIGHT
}

fn editor_local_x(ui: &AppWindow, window_x: f64) -> f64 {
    let gutter = if ui.get_show_line_numbers() {
        LINE_GUTTER_WIDTH
    } else {
        0.0
    };
    (window_x - gutter).max(0.0)
}

fn overlays_visible(ui: &AppWindow) -> bool {
    ui.get_find_visible()
        || ui.get_replace_visible()
        || ui.get_goto_visible()
        || ui.get_workspace_visible()
        || ui.get_recent_visible()
        || ui.get_settings_visible()
        || ui.get_about_visible()
        || ui.get_prompt_visible()
        || ui.get_trash_confirm_visible()
        || ui.get_app_menu_visible()
        || ui.get_context_menu_visible()
        || ui.get_command_palette_visible()
        || ui.get_dirty_decision_visible()
        || ui.get_conflict_visible()
}

fn blocking_modal_visible(ui: &AppWindow) -> bool {
    ui.get_dirty_decision_visible() || ui.get_conflict_visible()
}

fn text_input_has_priority(ui: &AppWindow) -> bool {
    ui.get_find_visible()
        || ui.get_replace_visible()
        || ui.get_goto_visible()
        || ui.get_workspace_visible()
        || ui.get_prompt_visible()
        || ui.get_command_palette_visible()
        || ui.get_settings_visible()
}

fn is_text_edit_command(id: &str) -> bool {
    matches!(
        id,
        "edit.copy" | "edit.cut" | "edit.paste" | "edit.select-all"
    )
}

fn command_for_key_event(
    settings: &TextiSettings,
    key: &Key,
    modifiers: ModifiersState,
) -> Option<&'static commands::CommandDef> {
    let shortcut = Shortcut::from_key(key, modifiers).ok().flatten()?;
    COMMANDS.iter().find(|command| {
        resolved_shortcut(command, &settings.command_palette).as_ref() == Some(&shortcut)
    })
}

fn handle_shortcut_capture(
    ui: &AppWindow,
    state: &Rc<RefCell<AppState>>,
    editor: &Rc<RefCell<EditorRuntime>>,
    event: &slint::winit_030::winit::event::KeyEvent,
    modifiers: ModifiersState,
) {
    let id = ui.get_shortcut_capture_id().to_string();
    if command(&id).is_none() {
        ui.set_shortcut_capture_id("".into());
        return;
    }
    if matches!(event.logical_key, Key::Named(NamedKey::Escape)) {
        ui.set_shortcut_capture_id("".into());
        ui.set_shortcut_error("".into());
        refresh_command_settings_rows(
            ui,
            state.borrow().settings(),
            &ui.get_command_settings_query(),
            "",
        );
        return;
    }
    let clear = matches!(
        event.logical_key,
        Key::Named(NamedKey::Backspace | NamedKey::Delete)
    ) && !(modifiers.control_key()
        || modifiers.alt_key()
        || modifiers.shift_key()
        || modifiers.super_key());
    if clear {
        let result = state.borrow_mut().set_command_shortcut_override(&id, None);
        if let Err(error) = result {
            state.borrow_mut().set_message(format!("Error: {error:#}"));
        }
        ui.set_shortcut_capture_id("".into());
        ui.set_shortcut_error("".into());
        refresh_ui(ui, &state.borrow(), editor);
        refresh_command_models(ui, state.borrow().settings());
        return;
    }

    let candidate = match Shortcut::from_key(&event.logical_key, modifiers) {
        Ok(Some(shortcut)) => shortcut,
        Ok(None) => return,
        Err(error) => {
            ui.set_shortcut_error(error.into());
            return;
        }
    };
    if let Some(conflict) =
        shortcut_conflict(&id, &candidate, &state.borrow().settings().command_palette)
    {
        ui.set_shortcut_error(
            format!(
                "{} is already assigned to {}.",
                candidate.label(),
                conflict.title
            )
            .into(),
        );
        return;
    }

    let result = state
        .borrow_mut()
        .set_command_shortcut_override(&id, Some(candidate.label()));
    if let Err(error) = result {
        state.borrow_mut().set_message(format!("Error: {error:#}"));
    }
    ui.set_shortcut_capture_id("".into());
    ui.set_shortcut_error("".into());
    refresh_ui(ui, &state.borrow(), editor);
    refresh_command_models(ui, state.borrow().settings());
}

fn handle_editor_key_event(
    weak: &slint::Weak<AppWindow>,
    state: &Rc<RefCell<AppState>>,
    editor: &Rc<RefCell<EditorRuntime>>,
    session_saver: &SessionSaver,
    event: &slint::winit_030::winit::event::KeyEvent,
    modifiers: ModifiersState,
) -> bool {
    let ctrl = modifiers.control_key();
    let shift = modifiers.shift_key();
    if ctrl {
        match &event.logical_key {
            Key::Named(NamedKey::ArrowLeft) => {
                let text = state.borrow().snapshot().editor_text;
                editor.borrow_mut().move_word_left(&text, shift);
                refresh_if_possible(weak, state, editor);
                return true;
            }
            Key::Named(NamedKey::ArrowRight) => {
                let text = state.borrow().snapshot().editor_text;
                editor.borrow_mut().move_word_right(&text, shift);
                refresh_if_possible(weak, state, editor);
                return true;
            }
            Key::Named(NamedKey::Home) => {
                editor.borrow_mut().move_document_start(shift);
                refresh_if_possible(weak, state, editor);
                return true;
            }
            Key::Named(NamedKey::End) => {
                let text = state.borrow().snapshot().editor_text;
                editor.borrow_mut().move_document_end(&text, shift);
                refresh_if_possible(weak, state, editor);
                return true;
            }
            Key::Named(NamedKey::Backspace) => {
                let text = state.borrow().snapshot().editor_text;
                let range = editor.borrow().delete_word_backward_range(&text);
                if let Some((start, end)) = range {
                    replace_range(weak, state, editor, session_saver, start..end, "", false);
                }
                return true;
            }
            Key::Named(NamedKey::Delete) => {
                let text = state.borrow().snapshot().editor_text;
                let range = editor.borrow().delete_word_forward_range(&text);
                if let Some((start, end)) = range {
                    replace_range(weak, state, editor, session_saver, start..end, "", false);
                }
                return true;
            }
            _ => return false,
        }
    }

    match &event.logical_key {
        Key::Named(NamedKey::ArrowLeft) => {
            let text = state.borrow().snapshot().editor_text;
            editor.borrow_mut().move_left(&text, shift);
            refresh_if_possible(weak, state, editor);
            true
        }
        Key::Named(NamedKey::ArrowRight) => {
            let text = state.borrow().snapshot().editor_text;
            editor.borrow_mut().move_right(&text, shift);
            refresh_if_possible(weak, state, editor);
            true
        }
        Key::Named(NamedKey::ArrowUp) => {
            let text = state.borrow().snapshot().editor_text;
            editor.borrow_mut().move_vertical(&text, -1, shift);
            refresh_if_possible(weak, state, editor);
            true
        }
        Key::Named(NamedKey::ArrowDown) => {
            let text = state.borrow().snapshot().editor_text;
            editor.borrow_mut().move_vertical(&text, 1, shift);
            refresh_if_possible(weak, state, editor);
            true
        }
        Key::Named(NamedKey::PageUp) => {
            let text = state.borrow().snapshot().editor_text;
            editor.borrow_mut().move_vertical(&text, -20, shift);
            refresh_if_possible(weak, state, editor);
            true
        }
        Key::Named(NamedKey::PageDown) => {
            let text = state.borrow().snapshot().editor_text;
            editor.borrow_mut().move_vertical(&text, 20, shift);
            refresh_if_possible(weak, state, editor);
            true
        }
        Key::Named(NamedKey::Home) => {
            editor.borrow_mut().move_home(shift);
            refresh_if_possible(weak, state, editor);
            true
        }
        Key::Named(NamedKey::End) => {
            editor.borrow_mut().move_end(shift);
            refresh_if_possible(weak, state, editor);
            true
        }
        Key::Named(NamedKey::Backspace) => {
            delete_backward(weak, state, editor, session_saver);
            true
        }
        Key::Named(NamedKey::Delete) => {
            delete_forward(weak, state, editor, session_saver);
            true
        }
        Key::Named(NamedKey::Enter) => {
            let snapshot = state.borrow().snapshot();
            let indent = indentation_text(&snapshot);
            let edit = editor.borrow().auto_indent_edit(
                &snapshot.editor_text,
                &indent,
                snapshot.active_line_ending.as_str(),
            );
            apply_editor_edit(weak, state, editor, session_saver, edit, true);
            true
        }
        Key::Named(NamedKey::Tab) => {
            let snapshot = state.borrow().snapshot();
            let edit = if shift {
                editor
                    .borrow()
                    .outdent_edit(&snapshot.editor_text, snapshot.settings.tab_size as usize)
            } else {
                editor
                    .borrow()
                    .indent_edit(&snapshot.editor_text, &indentation_text(&snapshot))
            };
            if let Some(edit) = edit {
                apply_editor_edit(weak, state, editor, session_saver, edit, true);
            }
            true
        }
        _ => {
            if modifiers.alt_key() || modifiers.super_key() {
                return false;
            }
            let Some(text) = event.text.as_ref() else {
                return false;
            };
            if text.chars().any(|ch| ch.is_control()) {
                return false;
            }
            replace_selection_or_insert(weak, state, editor, session_saver, text, false);
            true
        }
    }
}

fn copy_selection(state: &Rc<RefCell<AppState>>, editor: &Rc<RefCell<EditorRuntime>>) {
    let text = state.borrow().snapshot().editor_text;
    let selected = editor.borrow().selected_text(&text);
    if selected.is_empty() {
        return;
    }
    if let Ok(mut clipboard) = arboard::Clipboard::new() {
        let _ = clipboard.set_text(selected);
    }
}

fn cut_selection(
    weak: &slint::Weak<AppWindow>,
    state: &Rc<RefCell<AppState>>,
    editor: &Rc<RefCell<EditorRuntime>>,
    session_saver: &SessionSaver,
) {
    copy_selection(state, editor);
    replace_selection(weak, state, editor, session_saver, "", true);
}

fn paste_clipboard(
    weak: &slint::Weak<AppWindow>,
    state: &Rc<RefCell<AppState>>,
    editor: &Rc<RefCell<EditorRuntime>>,
    session_saver: &SessionSaver,
) {
    let Ok(mut clipboard) = arboard::Clipboard::new() else {
        return;
    };
    let Ok(text) = clipboard.get_text() else {
        return;
    };
    replace_selection_or_insert(weak, state, editor, session_saver, &text, true);
}

fn delete_backward(
    weak: &slint::Weak<AppWindow>,
    state: &Rc<RefCell<AppState>>,
    editor: &Rc<RefCell<EditorRuntime>>,
    session_saver: &SessionSaver,
) {
    let text = state.borrow().snapshot().editor_text;
    let isolated = editor.borrow().selection_range().is_some();
    let range = editor.borrow().delete_backward_range(&text);
    if let Some((start, end)) = range {
        replace_range(weak, state, editor, session_saver, start..end, "", isolated);
    }
}

fn delete_forward(
    weak: &slint::Weak<AppWindow>,
    state: &Rc<RefCell<AppState>>,
    editor: &Rc<RefCell<EditorRuntime>>,
    session_saver: &SessionSaver,
) {
    let text = state.borrow().snapshot().editor_text;
    let isolated = editor.borrow().selection_range().is_some();
    let range = editor.borrow().delete_forward_range(&text);
    if let Some((start, end)) = range {
        replace_range(weak, state, editor, session_saver, start..end, "", isolated);
    }
}

fn replace_selection_or_insert(
    weak: &slint::Weak<AppWindow>,
    state: &Rc<RefCell<AppState>>,
    editor: &Rc<RefCell<EditorRuntime>>,
    session_saver: &SessionSaver,
    text: &str,
    isolated: bool,
) {
    let selection = editor.borrow().selection_range();
    let (start, end) = selection.unwrap_or_else(|| {
        let cursor = editor.borrow().cursor;
        (cursor, cursor)
    });
    replace_range(
        weak,
        state,
        editor,
        session_saver,
        start..end,
        text,
        isolated,
    );
}

fn replace_selection(
    weak: &slint::Weak<AppWindow>,
    state: &Rc<RefCell<AppState>>,
    editor: &Rc<RefCell<EditorRuntime>>,
    session_saver: &SessionSaver,
    text: &str,
    isolated: bool,
) {
    let selection = editor.borrow().selection_range();
    let Some((start, end)) = selection else {
        return;
    };
    replace_range(
        weak,
        state,
        editor,
        session_saver,
        start..end,
        text,
        isolated,
    );
}

fn replace_range(
    weak: &slint::Weak<AppWindow>,
    state: &Rc<RefCell<AppState>>,
    editor: &Rc<RefCell<EditorRuntime>>,
    session_saver: &SessionSaver,
    range: Range<usize>,
    text: &str,
    isolated: bool,
) {
    let start = range.start;
    let inserted_chars = text.chars().count();
    let result = {
        let mut state = state.borrow_mut();
        if isolated {
            state.replace_active_range_chars_isolated(start, range.end, text)
        } else {
            state.replace_active_range_chars(start, range.end, text)
        }
    };
    match result {
        Ok(()) => {
            let mut editor = editor.borrow_mut();
            editor.cursor = start + inserted_chars;
            editor.anchor = editor.cursor;
            editor.show_caret();
            session_saver.schedule();
        }
        Err(error) => {
            state.borrow_mut().set_message(format!("Error: {error:#}"));
        }
    }
    refresh_if_possible(weak, state, editor);
}

fn apply_editor_edit(
    weak: &slint::Weak<AppWindow>,
    state: &Rc<RefCell<AppState>>,
    editor: &Rc<RefCell<EditorRuntime>>,
    session_saver: &SessionSaver,
    edit: EditorEdit,
    isolated: bool,
) {
    let result = if isolated {
        state.borrow_mut().replace_active_range_chars_isolated(
            edit.start,
            edit.end,
            &edit.replacement,
        )
    } else {
        state
            .borrow_mut()
            .replace_active_range_chars(edit.start, edit.end, &edit.replacement)
    };
    match result {
        Ok(()) => {
            editor.borrow_mut().apply_edit(&edit);
            session_saver.schedule();
        }
        Err(error) => state.borrow_mut().set_message(format!("Error: {error:#}")),
    }
    refresh_if_possible(weak, state, editor);
}

fn select_byte_range_from_snapshot(
    state: &Rc<RefCell<AppState>>,
    editor: &Rc<RefCell<EditorRuntime>>,
    start_byte: usize,
    end_byte: usize,
) {
    let text = state.borrow().snapshot().editor_text;
    editor
        .borrow_mut()
        .select_byte_range(&text, start_byte, end_byte);
}

fn refresh_if_possible(
    weak: &slint::Weak<AppWindow>,
    state: &Rc<RefCell<AppState>>,
    editor: &Rc<RefCell<EditorRuntime>>,
) {
    if let Some(ui) = weak.upgrade() {
        refresh_ui(&ui, &state.borrow(), editor);
        let (x, y) = editor.borrow().viewport_to_reveal_caret(
            editor_view_width(&ui, ui.get_show_line_numbers(), ui.get_show_minimap()),
            editor_view_height(&ui),
            16.0,
        );
        if (x - ui.get_editor_viewport_x()).abs() > f32::EPSILON
            || (y - ui.get_editor_viewport_y()).abs() > f32::EPSILON
        {
            ui.set_editor_viewport_x(x);
            ui.set_editor_viewport_y(y);
            editor.borrow_mut().set_viewport(x, y);
            refresh_ui(&ui, &state.borrow(), editor);
        }
    }
}

fn run_search(
    weak: &slint::Weak<AppWindow>,
    state: &Rc<RefCell<AppState>>,
    editor: &Rc<RefCell<EditorRuntime>>,
    find: &Rc<RefCell<FindRuntime>>,
    query: &str,
    direction: isize,
) {
    let snapshot = state.borrow().snapshot();
    let Some(buffer_id) = snapshot.active_buffer_id else {
        return;
    };
    if query.is_empty() {
        *find.borrow_mut() = FindRuntime::default();
        if let Some(ui) = weak.upgrade() {
            ui.set_search_rows(model_from_vec(Vec::new()));
        }
        return;
    }

    let needs_refresh = {
        let find = find.borrow();
        find.buffer_id != Some(buffer_id)
            || find.revision != snapshot.active_revision
            || find.query != query
    };
    if needs_refresh {
        let search_result = state.borrow_mut().search_active_structured(query);
        let matches = match search_result {
            Ok(matches) => matches,
            Err(error) => {
                state.borrow_mut().set_message(format!("Error: {error:#}"));
                Vec::new()
            }
        };
        *find.borrow_mut() = FindRuntime {
            buffer_id: Some(buffer_id),
            revision: snapshot.active_revision,
            query: query.to_string(),
            matches,
            current: None,
        };
    }

    {
        let mut find = find.borrow_mut();
        if !find.matches.is_empty() {
            find.current = Some(if needs_refresh || find.current.is_none() {
                if direction < 0 {
                    find.matches.len() - 1
                } else {
                    0
                }
            } else {
                let current = find.current.unwrap_or(0) as isize;
                (current + direction).rem_euclid(find.matches.len() as isize) as usize
            });
        }
    }

    if let Some(ui) = weak.upgrade() {
        let find_ref = find.borrow();
        ui.set_search_rows(model_from_vec(search_rows(&find_ref.matches)));
        if let Some(index) = find_ref.current {
            select_search_match(&ui, state, editor, &find_ref.matches[index]);
        } else {
            refresh_ui(&ui, &state.borrow(), editor);
        }
    }
}

fn select_find_result(
    weak: &slint::Weak<AppWindow>,
    state: &Rc<RefCell<AppState>>,
    editor: &Rc<RefCell<EditorRuntime>>,
    find: &Rc<RefCell<FindRuntime>>,
    index: usize,
) {
    let Some(ui) = weak.upgrade() else {
        return;
    };
    let mut find = find.borrow_mut();
    let Some(result) = find.matches.get(index).cloned() else {
        return;
    };
    find.current = Some(index);
    select_search_match(&ui, state, editor, &result);
}

fn select_search_match(
    ui: &AppWindow,
    state: &Rc<RefCell<AppState>>,
    editor: &Rc<RefCell<EditorRuntime>>,
    result: &SearchResult,
) {
    select_byte_range_from_snapshot(
        state,
        editor,
        result.absolute_byte_start,
        result.absolute_byte_end,
    );
    refresh_ui(ui, &state.borrow(), editor);
    scroll_line_into_view(ui, result.line_number);
    editor
        .borrow_mut()
        .set_viewport(ui.get_editor_viewport_x(), ui.get_editor_viewport_y());
    refresh_ui(ui, &state.borrow(), editor);
}

fn run_action<F>(
    weak: &slint::Weak<AppWindow>,
    state: &Rc<RefCell<AppState>>,
    editor: &Rc<RefCell<EditorRuntime>>,
    action: F,
) where
    F: FnOnce(&mut AppState) -> Result<()>,
{
    let result = {
        let mut state = state.borrow_mut();
        action(&mut state)
    };
    if let Err(error) = result {
        state.borrow_mut().set_message(format!("Error: {error:#}"));
    }
    if let Some(ui) = weak.upgrade() {
        refresh_ui(&ui, &state.borrow(), editor);
    }
}

fn open_dropped_path(
    weak: &slint::Weak<AppWindow>,
    state: &Rc<RefCell<AppState>>,
    editor: &Rc<RefCell<EditorRuntime>>,
    path: PathBuf,
) {
    if path.is_dir() {
        run_action(weak, state, editor, |state| state.open_folder(path));
        if let Some(ui) = weak.upgrade() {
            refresh_workspace_query(&ui, &state.borrow(), "");
            ui.set_workspace_query("".into());
            ui.set_workspace_visible(true);
        }
    } else if path.is_file() {
        run_action(weak, state, editor, |state| {
            state.open_file(path)?;
            Ok(())
        });
    } else {
        state.borrow_mut().set_message(format!(
            "Error: dropped path is not a file or folder: {}",
            path.display()
        ));
        refresh_if_possible(weak, state, editor);
    }
}

fn refresh_ui(ui: &AppWindow, state: &AppState, editor: &Rc<RefCell<EditorRuntime>>) {
    let snapshot = state.snapshot();
    let tabs = tab_rows(&snapshot);
    let explorer_rows = explorer_rows(&snapshot);
    let recent_rows = recent_rows(&snapshot.recent_files);
    let selected_path = snapshot
        .selected_path
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "Open a folder to browse files".to_string());
    let readonly = matches!(snapshot.status.save_state, SaveState::ReadOnly(_));

    ui.set_readonly(readonly);
    ui.set_show_line_numbers(snapshot.settings.show_line_numbers);
    ui.set_word_wrap(snapshot.settings.word_wrap);
    ui.set_show_minimap(snapshot.settings.show_minimap);
    ui.set_show_whitespace(snapshot.settings.show_whitespace);
    ui.set_editor_font_size(snapshot.settings.font_size as i32);
    ui.set_tab_size(snapshot.settings.tab_size as i32);
    ui.set_insert_spaces(snapshot.settings.insert_spaces);
    ui.set_status_hints(snapshot.settings.status_hints);
    ui.set_syntax_highlighting(snapshot.settings.syntax_highlighting);
    ui.set_recovery_autosave(snapshot.settings.recovery_autosave);
    ui.set_show_hidden_files(snapshot.settings.show_hidden_files);
    ui.set_confirm_trash(snapshot.settings.confirm_trash);
    ui.set_syntax_mode_label(snapshot.settings.default_syntax_mode.label().into());

    refresh_editor_surface(ui, &snapshot, editor);
    ensure_active_tab_visible(ui, &snapshot);

    ui.set_tabs(model_from_vec(tabs));
    ui.set_explorer_rows(model_from_vec(explorer_rows));
    ui.set_recent_rows(model_from_vec(recent_rows));
    ui.set_message_text(snapshot.message.into());
    ui.set_window_title(snapshot.window_title.clone().into());
    ui.set_selected_path_text(selected_path.into());
    ui.set_workspace_root_text(
        snapshot
            .workspace_root
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_default()
            .into(),
    );
    let (minimap, total_lines) = if snapshot.settings.show_minimap {
        minimap_rows(&snapshot.editor_text)
    } else {
        (Vec::new(), 1)
    };
    ui.set_minimap_rows(model_from_vec(minimap));
    ui.set_minimap_total_lines(total_lines as i32);
}

fn refresh_editor_after_resize(
    ui: &AppWindow,
    state: &AppState,
    editor: &Rc<RefCell<EditorRuntime>>,
) {
    let snapshot = state.snapshot();
    ui.set_editor_font_size(snapshot.settings.font_size as i32);
    refresh_editor_surface(ui, &snapshot, editor);
}

fn refresh_editor_surface(
    ui: &AppWindow,
    snapshot: &AppSnapshot,
    editor: &Rc<RefCell<EditorRuntime>>,
) {
    let switching_buffers = editor.borrow().active_buffer_id() != snapshot.active_buffer_id;
    if switching_buffers {
        if let Some(buffer_id) = snapshot.active_buffer_id {
            editor.borrow_mut().set_view_state(
                buffer_id,
                editor_view_from_model(&snapshot.active_view_state),
            );
        }
        ui.set_editor_viewport_x(snapshot.active_view_state.viewport_x.max(0.0));
        ui.set_editor_viewport_y(snapshot.active_view_state.viewport_y.max(0.0));
    }
    let viewport_width = editor_view_width(
        ui,
        snapshot.settings.show_line_numbers,
        snapshot.settings.show_minimap,
    );
    let viewport_height = editor_view_height_for(ui, snapshot.settings.status_hints);
    let config = EditorRenderConfig {
        word_wrap: snapshot.settings.word_wrap,
        viewport_width,
        viewport_height,
        scroll_x: ui.get_editor_viewport_x(),
        scroll_y: ui.get_editor_viewport_y(),
        font_size: snapshot.settings.font_size as f32,
        cell_width: ui.get_editor_cell_width(),
        tab_size: snapshot.settings.tab_size as usize,
        show_whitespace: snapshot.settings.show_whitespace,
        overscan_rows: 8,
    };
    let rendered = render_editor_with_config(
        &mut editor.borrow_mut(),
        snapshot.active_buffer_id,
        snapshot.active_revision,
        &snapshot.editor_text,
        &snapshot.syntax_spans,
        config,
    );
    let (cursor_line, cursor_col) = editor.borrow().cursor_line_col();
    let status_text = status_with_cursor(&snapshot.status.text, cursor_line, cursor_col);
    let (scroll_x, scroll_y) = editor.borrow().viewport();

    ui.set_editor_viewport_x(scroll_x);
    ui.set_editor_viewport_y(scroll_y);
    ui.set_editor_rows(model_from_vec(rendered.rows));
    ui.set_editor_segments(model_from_vec(rendered.segments));
    ui.set_editor_content_width(rendered.content_width);
    ui.set_editor_content_height(rendered.content_height);
    ui.set_editor_caret_x(rendered.caret_x);
    ui.set_editor_caret_y(rendered.caret_y);
    ui.set_editor_caret_height(rendered.caret_height);
    ui.set_editor_caret_visible(rendered.caret_visible);
    ui.set_status_text(status_text.into());
}

fn editor_view_width(ui: &AppWindow, show_line_numbers: bool, show_minimap: bool) -> f32 {
    let size = ui.window().size();
    let scale = ui.window().scale_factor();
    let logical = slint::LogicalSize::from_physical(size, scale);
    let gutter = if show_line_numbers {
        LINE_GUTTER_WIDTH as f32
    } else {
        0.0
    };
    let minimap = if show_minimap {
        MINIMAP_WIDTH as f32
    } else {
        0.0
    };
    (logical.width - gutter - minimap - 24.0).max(120.0)
}

fn editor_view_height(ui: &AppWindow) -> f32 {
    editor_view_height_for(ui, ui.get_status_hints())
}

fn editor_view_height_for(ui: &AppWindow, status_hints: bool) -> f32 {
    let size = ui.window().size();
    let scale = ui.window().scale_factor();
    let logical = slint::LogicalSize::from_physical(size, scale);
    let status = if status_hints {
        STATUS_HINT_HEIGHT as f32
    } else {
        0.0
    };
    (logical.height - HEADER_HEIGHT as f32 - status).max(80.0)
}

fn status_with_cursor(status: &str, line: usize, column: usize) -> String {
    let parts = status
        .split(" · ")
        .map(|part| {
            if part.starts_with("Ln ") {
                format!("Ln {line}, Col {column}")
            } else {
                part.to_string()
            }
        })
        .collect::<Vec<_>>();
    parts.join(" · ")
}

fn model_from_vec<T: Clone + 'static>(items: Vec<T>) -> ModelRc<T> {
    ModelRc::new(Rc::new(VecModel::from(items)))
}

fn explorer_rows(snapshot: &AppSnapshot) -> Vec<ExplorerUiRow> {
    snapshot
        .explorer_rows
        .iter()
        .map(|row| explorer_row(row, snapshot.workspace_root.as_deref()))
        .collect::<Vec<_>>()
}

fn explorer_row(row: &ExplorerRow, root: Option<&Path>) -> ExplorerUiRow {
    let display_path = root
        .and_then(|root| row.path.strip_prefix(root).ok())
        .map(|path| path.display().to_string())
        .filter(|path| !path.is_empty())
        .unwrap_or_else(|| row.name.clone());
    ExplorerUiRow {
        path: SharedString::from(row.path.display().to_string()),
        name: SharedString::from(row.name.clone()),
        display_path: SharedString::from(display_path),
        depth: i32::from(row.depth),
        is_dir: row.is_dir(),
        selected: row.selected,
        symlink: row.is_symlink(),
    }
}

fn tab_rows(snapshot: &AppSnapshot) -> Vec<TabUiRow> {
    snapshot.tabs.iter().map(tab_row).collect()
}

fn tab_row(tab: &TabInfo) -> TabUiRow {
    TabUiRow {
        id: tab.id as i32,
        title: SharedString::from(tab.title.clone()),
        dirty: tab.dirty,
        active: tab.active,
        readonly: tab.readonly,
    }
}

fn recent_rows(recent: &[RecentFile]) -> Vec<RecentUiRow> {
    recent
        .iter()
        .map(|row| RecentUiRow {
            path: SharedString::from(row.path.display().to_string()),
            display: SharedString::from(row.display.clone()),
        })
        .collect()
}

fn search_rows(matches: &[SearchResult]) -> Vec<SearchUiRow> {
    matches
        .iter()
        .enumerate()
        .map(|(index, m)| SearchUiRow {
            index: index as i32,
            line: m.line_number as i32,
            column: m.column_number as i32,
            preview: SharedString::from(m.preview.clone()),
        })
        .collect()
}

fn refresh_workspace_query(ui: &AppWindow, state: &AppState, query: &str) {
    let snapshot = state.snapshot();
    let rows = state
        .workspace_matches(query, 300)
        .iter()
        .map(|row| explorer_row(row, snapshot.workspace_root.as_deref()))
        .collect::<Vec<_>>();
    ui.set_explorer_rows(model_from_vec(rows));
}

fn scroll_line_into_view(ui: &AppWindow, line_number: usize) {
    if line_number == 0 {
        return;
    }
    let line_height = (ui.get_editor_font_size().max(8) + 3) as f32;
    let target_y = ((line_number - 1) as f32 * line_height) - (line_height * 2.0);
    ui.invoke_set_editor_viewport_y(target_y.max(0.0));
}

fn indentation_text(snapshot: &AppSnapshot) -> String {
    if snapshot.settings.insert_spaces {
        " ".repeat(snapshot.settings.tab_size as usize)
    } else {
        "\t".to_string()
    }
}

fn editor_view_from_model(view: &ViewState) -> EditorViewState {
    EditorViewState {
        cursor: view.cursor_char,
        anchor: view.anchor_char,
        preferred_column: view.preferred_column,
        scroll_x: view.viewport_x,
        scroll_y: view.viewport_y,
    }
}

fn model_view_from_editor(view: EditorViewState) -> ViewState {
    ViewState {
        cursor_char: view.cursor,
        anchor_char: view.anchor,
        preferred_column: view.preferred_column,
        viewport_x: view.scroll_x,
        viewport_y: view.scroll_y,
    }
}

fn model_view_states(editor: &EditorRuntime) -> Vec<(BufferId, ViewState)> {
    editor
        .view_states()
        .into_iter()
        .map(|(id, view)| (id, model_view_from_editor(view)))
        .collect()
}

fn restore_editor_views(state: &AppState, editor: &mut EditorRuntime) {
    let snapshot = state.snapshot();
    editor.replace_view_states(snapshot.tabs.iter().filter_map(|tab| {
        state
            .buffer_view_state(tab.id)
            .ok()
            .map(|view| (tab.id, editor_view_from_model(&view)))
    }));
}

fn activate_tab(
    weak: &slint::Weak<AppWindow>,
    state: &Rc<RefCell<AppState>>,
    editor: &Rc<RefCell<EditorRuntime>>,
    id: BufferId,
) {
    let active = state.borrow().snapshot().active_buffer_id;
    if let (Some(ui), Some(active)) = (weak.upgrade(), active) {
        editor
            .borrow_mut()
            .set_viewport(ui.get_editor_viewport_x(), ui.get_editor_viewport_y());
        let view = editor.borrow().view_state(active);
        if let Some(view) = view {
            let _ = state
                .borrow_mut()
                .set_buffer_view_state(active, model_view_from_editor(view));
        }
    }
    run_action(weak, state, editor, |state| state.activate_tab(id));
}

fn cycle_tab(
    weak: &slint::Weak<AppWindow>,
    state: &Rc<RefCell<AppState>>,
    editor: &Rc<RefCell<EditorRuntime>>,
    delta: isize,
) {
    let snapshot = state.borrow().snapshot();
    if snapshot.tabs.len() < 2 {
        return;
    }
    let current = snapshot.tabs.iter().position(|tab| tab.active).unwrap_or(0);
    let next = (current as isize + delta).rem_euclid(snapshot.tabs.len() as isize) as usize;
    activate_tab(weak, state, editor, snapshot.tabs[next].id);
}

fn request_close_buffer(
    ui: &AppWindow,
    state: &Rc<RefCell<AppState>>,
    editor: &Rc<RefCell<EditorRuntime>>,
    id: BufferId,
    session_saver: &SessionSaver,
) {
    let outcome = state.borrow_mut().request_close_buffer(id);
    match outcome {
        Ok(CloseOutcome::Closed { .. }) => {
            editor.borrow_mut().remove_view_state(id);
            session_saver.schedule();
            refresh_ui(ui, &state.borrow(), editor);
        }
        Ok(CloseOutcome::NeedsDecision { title, .. }) => {
            ui.set_dirty_decision_tab_id(id as i32);
            ui.set_dirty_decision_title(title.into());
            ui.set_dirty_decision_visible(true);
        }
        Ok(_) => {}
        Err(error) => {
            state.borrow_mut().set_message(format!("Error: {error:#}"));
            refresh_ui(ui, &state.borrow(), editor);
        }
    }
}

fn save_buffer_or_prompt(
    ui: &AppWindow,
    state: &Rc<RefCell<AppState>>,
    editor: &Rc<RefCell<EditorRuntime>>,
    id: BufferId,
) {
    let outcome = state.borrow_mut().save_buffer_checked(id);
    match outcome {
        Ok(SaveOutcome::Saved { .. }) => refresh_ui(ui, &state.borrow(), editor),
        Ok(SaveOutcome::SaveAsRequired { .. }) => {
            if let Some(path) = rfd::FileDialog::new().set_title("Save file as").save_file() {
                let save_result = state.borrow_mut().save_buffer_as(id, path);
                if let Err(error) = save_result {
                    state.borrow_mut().set_message(format!("Error: {error:#}"));
                }
                refresh_ui(ui, &state.borrow(), editor);
            }
        }
        Ok(SaveOutcome::Conflict(conflict)) => show_conflict_dialog(ui, id, &conflict.path),
        Ok(_) => {}
        Err(error) => state.borrow_mut().set_message(format!("Error: {error:#}")),
    }
}

fn show_conflict_dialog(ui: &AppWindow, id: BufferId, path: &Path) {
    ui.set_conflict_tab_id(id as i32);
    ui.set_conflict_path(path.display().to_string().into());
    ui.set_conflict_visible(true);
}

fn can_close_window_now(state: &Rc<RefCell<AppState>>, saver: &SessionSaver) -> bool {
    let snapshot = state.borrow().snapshot();
    if snapshot.settings.recovery_autosave || !snapshot.tabs.iter().any(|tab| tab.dirty) {
        match saver.flush() {
            Ok(()) => true,
            Err(error) => {
                state
                    .borrow_mut()
                    .set_message(format!("Cannot close: recovery save failed: {error:#}"));
                false
            }
        }
    } else {
        false
    }
}

fn begin_window_close(
    ui: &AppWindow,
    state: &Rc<RefCell<AppState>>,
    saver: &SessionSaver,
    pending_quit: &Rc<RefCell<bool>>,
) {
    if can_close_window_now(state, saver) {
        let _ = ui.window().hide();
    } else {
        *pending_quit.borrow_mut() = true;
        show_first_dirty_dialog(ui, state);
    }
}

fn show_first_dirty_dialog(ui: &AppWindow, state: &Rc<RefCell<AppState>>) {
    if let Some(tab) = state
        .borrow()
        .snapshot()
        .tabs
        .into_iter()
        .find(|tab| tab.dirty)
    {
        ui.set_dirty_decision_tab_id(tab.id as i32);
        ui.set_dirty_decision_title(tab.title.into());
        ui.set_dirty_decision_visible(true);
    }
}

fn finish_close_flow(
    ui: &AppWindow,
    state: &Rc<RefCell<AppState>>,
    editor: &Rc<RefCell<EditorRuntime>>,
    saver: &SessionSaver,
    pending_quit: &Rc<RefCell<bool>>,
) {
    refresh_ui(ui, &state.borrow(), editor);
    saver.schedule();
    let quitting = *pending_quit.borrow();
    if quitting {
        let has_dirty_tabs = state.borrow().snapshot().tabs.iter().any(|tab| tab.dirty);
        if has_dirty_tabs {
            show_first_dirty_dialog(ui, state);
        } else {
            *pending_quit.borrow_mut() = false;
            match saver.flush() {
                Ok(()) => {
                    let _ = ui.window().hide();
                }
                Err(error) => state
                    .borrow_mut()
                    .set_message(format!("Cannot close: recovery save failed: {error:#}")),
            }
        }
    }
}

fn refresh_command_models(ui: &AppWindow, settings: &TextiSettings) {
    refresh_command_rows(ui, settings, &ui.get_command_query());
    refresh_command_settings_rows(
        ui,
        settings,
        &ui.get_command_settings_query(),
        &ui.get_shortcut_capture_id(),
    );
}

fn refresh_command_rows(ui: &AppWindow, settings: &TextiSettings, query: &str) {
    let needle = query.trim().to_ascii_lowercase();
    let filtered = COMMANDS
        .iter()
        .copied()
        .filter(|command| is_palette_visible(command, &settings.command_palette))
        .filter(|command| {
            needle.is_empty()
                || command.title.to_ascii_lowercase().contains(&needle)
                || command.group.to_ascii_lowercase().contains(&needle)
        })
        .collect::<Vec<_>>();
    let selected = filtered.first().map(|command| command.id).unwrap_or("");
    ui.set_command_selected_id(selected.into());
    ui.set_command_rows(model_from_vec(
        filtered
            .into_iter()
            .enumerate()
            .map(|(index, command)| CommandUiRow {
                id: command.id.into(),
                title: command.title.into(),
                group: command.group.into(),
                shortcut: resolved_shortcut_label(&command, &settings.command_palette).into(),
                selected: index == 0,
            })
            .collect(),
    ));
}

fn refresh_command_settings_rows(
    ui: &AppWindow,
    settings: &TextiSettings,
    query: &str,
    capture_id: &str,
) {
    let needle = query.trim().to_ascii_lowercase();
    let rows = COMMANDS
        .iter()
        .filter(|command| {
            needle.is_empty()
                || command.title.to_ascii_lowercase().contains(&needle)
                || command.group.to_ascii_lowercase().contains(&needle)
                || command.id.to_ascii_lowercase().contains(&needle)
        })
        .map(|command| CommandSettingsUiRow {
            id: command.id.into(),
            title: command.title.into(),
            group: command.group.into(),
            shortcut: resolved_shortcut_label(command, &settings.command_palette).into(),
            visible: is_palette_visible(command, &settings.command_palette),
            capturing: capture_id == command.id,
        })
        .collect();
    ui.set_command_settings_rows(model_from_vec(rows));
}

fn execute_command(ui: &AppWindow, id: &str) {
    match id {
        "file.new" => ui.invoke_request_new(),
        "file.open" => ui.invoke_request_open_file(),
        "file.open-folder" => ui.invoke_request_open_folder(),
        "file.save" => ui.invoke_request_save(),
        "file.save-as" => ui.invoke_request_save_as(),
        "file.reload" => ui.invoke_request_reload(),
        "file.close" => ui.invoke_request_close_tab(),
        "tab.next" => ui.invoke_request_next_tab(),
        "tab.previous" => ui.invoke_request_previous_tab(),
        "edit.undo" => ui.invoke_request_undo(),
        "edit.redo" => ui.invoke_request_redo(),
        "edit.cut" => ui.invoke_request_cut(),
        "edit.copy" => ui.invoke_request_copy(),
        "edit.paste" => ui.invoke_request_paste(),
        "edit.select-all" => ui.invoke_request_select_all(),
        "view.wrap" => ui.invoke_request_toggle_word_wrap(),
        "view.line-numbers" => ui.invoke_request_toggle_line_numbers(),
        "view.minimap" => ui.invoke_request_set_show_minimap(!ui.get_show_minimap()),
        "view.whitespace" => ui.invoke_request_set_show_whitespace(!ui.get_show_whitespace()),
        "view.focus" => ui.invoke_request_toggle_focus_mode(),
        "search.find"
        | "search.replace"
        | "search.goto"
        | "workspace.search"
        | "workspace.recent"
        | "workspace.new-file"
        | "workspace.new-folder"
        | "workspace.rename"
        | "workspace.trash"
        | "palette.open"
        | "settings.open" => {
            let weak = ui.as_weak();
            let id = id.to_string();
            Timer::single_shot(Duration::from_millis(0), move || {
                if let Some(ui) = weak.upgrade() {
                    match id.as_str() {
                        "search.find" => ui.invoke_show_find(),
                        "search.replace" => ui.invoke_show_replace(),
                        "search.goto" => ui.invoke_show_goto(),
                        "workspace.search" => ui.invoke_show_workspace(),
                        "workspace.recent" => ui.invoke_show_recent(),
                        "workspace.new-file" => ui.invoke_show_new_file_prompt(),
                        "workspace.new-folder" => ui.invoke_show_new_folder_prompt(),
                        "workspace.rename" => ui.invoke_show_rename_prompt(),
                        "workspace.trash" => ui.invoke_show_trash_prompt(),
                        "palette.open" => ui.invoke_show_command_palette(),
                        "settings.open" => ui.set_settings_visible(true),
                        _ => {}
                    }
                }
            });
        }
        _ => {}
    }
}

fn minimap_rows(text: &str) -> (Vec<MinimapUiRow>, usize) {
    let total = text.lines().count().max(1);
    let stride = total.div_ceil(400).max(1);
    let color = Color::from_argb_u8(150, 79, 167, 255);
    let rows = text
        .lines()
        .enumerate()
        .step_by(stride)
        .filter_map(|(index, line)| {
            let width = line.trim_end().chars().count().min(100) as f32 / 100.0;
            (width > 0.0).then_some(MinimapUiRow {
                line: index as i32 + 1,
                width: width.max(0.06),
                color,
            })
        })
        .collect();
    (rows, total)
}

fn ensure_active_tab_visible(ui: &AppWindow, snapshot: &AppSnapshot) {
    let Some(index) = snapshot.tabs.iter().position(|tab| tab.active) else {
        return;
    };
    let size = ui.window().size();
    let scale = ui.window().scale_factor();
    let logical = slint::LogicalSize::from_physical(size, scale);
    let rail_width = (logical.width - 120.0).max(96.0);
    let tab_width =
        ((logical.width - 150.0) / snapshot.tabs.len().max(1) as f32).clamp(96.0, 176.0) + 4.0;
    let start = index as f32 * tab_width;
    let end = start + tab_width;
    let current = ui.get_tab_viewport_x();
    let next = if start < current {
        start
    } else if end > current + rail_width {
        end - rail_width
    } else {
        current
    };
    ui.set_tab_viewport_x(next.max(0.0));
}

fn syntax_mode_from_label(label: &str) -> SyntaxMode {
    match label {
        "Plain Text" => SyntaxMode::PlainText,
        "Rust" => SyntaxMode::Rust,
        "Markdown" => SyntaxMode::Markdown,
        "TOML" => SyntaxMode::Toml,
        "JSON" => SyntaxMode::Json,
        "Slint" => SyntaxMode::Slint,
        _ => SyntaxMode::AutoDetect,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_accepts_multiple_paths_and_double_dash() {
        let action = parse_cli_args([
            OsString::from("main.rs"),
            OsString::from("--"),
            OsString::from("-notes.txt"),
        ])
        .expect("valid arguments");
        let CliAction::Run { paths } = action else {
            panic!("expected run action");
        };
        assert_eq!(
            paths,
            vec![PathBuf::from("main.rs"), PathBuf::from("-notes.txt")]
        );
    }

    #[test]
    fn cli_recognizes_help_version_and_rejects_unknown_options() {
        assert!(matches!(
            parse_cli_args([OsString::from("--help")]),
            Ok(CliAction::Help)
        ));
        assert!(matches!(
            parse_cli_args([OsString::from("-V")]),
            Ok(CliAction::Version)
        ));
        assert!(parse_cli_args([OsString::from("--wat")]).is_err());
    }
}
