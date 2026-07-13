use texti_core::AppState;
use texti_settings::{SettingsPaths, SettingsStore};

fn state_for(root: &std::path::Path) -> AppState {
    let store = SettingsStore::new(SettingsPaths::from_root(&root.join("settings")));
    AppState::new(store).unwrap()
}

#[test]
fn create_edit_save_rename_and_trash_file() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = state_for(dir.path());
    state.open_folder(dir.path()).unwrap();
    state.create_file_in_selection("draft.txt").unwrap();
    state
        .update_active_text("first pass\nsecond line".to_string())
        .unwrap();
    state.save_active().unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("draft.txt")).unwrap(),
        "first pass\nsecond line"
    );

    state.rename_selected("final.txt").unwrap();
    assert!(dir.path().join("final.txt").exists());
    state.trash_selected().unwrap();
    assert!(!dir.path().join("final.txt").exists());
}

#[test]
fn binary_file_opens_as_readonly_preview() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("blob.bin");
    std::fs::write(&path, b"abc\0def").unwrap();
    let mut state = state_for(dir.path());
    state.open_file(&path).unwrap();
    let snapshot = state.snapshot();
    assert!(snapshot.editor_text.contains("00000000"));
    assert!(snapshot.tabs.iter().any(|tab| tab.readonly));
    assert!(state.save_active_as(dir.path().join("copy.bin")).is_err());
    assert!(!dir.path().join("copy.bin").exists());
}

#[test]
fn save_failure_keeps_dirty_text() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = state_for(dir.path());
    state
        .update_active_text("cannot lose me".to_string())
        .unwrap();
    let missing_parent = dir.path().join("missing").join("note.txt");
    assert!(state.save_active_as(&missing_parent).is_err());
    let snapshot = state.snapshot();
    assert!(snapshot.editor_text.contains("cannot lose me"));
    assert!(snapshot.status.save_state.dirty());
}
