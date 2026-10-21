//! The write door and a settings row that lives in a subdirectory (`channels/telegram.yaml`).
//!
//! The handler-level test (a POST into a fresh root answering 204) needs `test_state` and
//! `workflow_call`, which are private to the `http.rs` test module; until the controller makes them
//! `pub(crate)`, the directory step the handler depends on is pinned here directly.

use super::create_setting_dirs;

/// A fresh root has no `channels/` directory, and the row's own path is what makes it.
#[test]
fn create_setting_dirs_makes_the_channels_directory_in_a_fresh_root() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join(".nucleos");
    std::fs::create_dir_all(&root).unwrap();
    assert!(!root.join("channels").exists());

    create_setting_dirs(&root, crate::machine_config::CHANNEL_TELEGRAM_FILE).unwrap();

    assert!(root.join("channels").is_dir());
    // Only the directory: the file itself is the write's business.
    assert!(!root.join("channels").join("telegram.yaml").exists());
    // And a second call, on a root that already has it, is not an error.
    create_setting_dirs(&root, crate::machine_config::CHANNEL_TELEGRAM_FILE).unwrap();
}

/// A flat row has no directory of its own, so nothing is created beyond the root.
#[test]
fn create_setting_dirs_leaves_a_flat_row_alone() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().to_path_buf();
    create_setting_dirs(&root, crate::machine_config::CALENDAR_FILE).unwrap();
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
}
