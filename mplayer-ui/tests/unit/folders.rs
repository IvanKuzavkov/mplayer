use super::*;

#[test]
fn multiple_selected_roots_wait_for_command_capacity() {
    let context = egui::Context::default();
    let (sender, receiver) = oneshot::channel();
    let mut picker = FolderPicker {
        requested: None,
        pending: Some(receiver),
        task: None,
        ready: None,
        error: None,
    };
    let folders = vec![
        PathBuf::from("first music folder"),
        PathBuf::from("second music folder"),
    ];
    sender.send(Some(folders.clone())).unwrap();
    assert!(picker.show(&context, false).is_none());
    assert!(picker.is_open());
    assert_eq!(picker.show(&context, true), Some(folders));
    picker.close();
    assert!(!picker.is_open());
}

#[test]
fn cancellation_and_unexpected_close_allow_opening_again() {
    let context = egui::Context::default();
    for cancelled in [false, true] {
        let (sender, receiver) = oneshot::channel();
        let mut picker = FolderPicker {
            requested: None,
            pending: Some(receiver),
            task: None,
            ready: None,
            error: None,
        };
        if cancelled {
            sender.send(None).unwrap();
        } else {
            drop(sender);
        }
        assert!(picker.show(&context, true).is_none());
        assert!(!picker.is_open());
        assert_eq!(picker.take_error().is_some(), !cancelled);
        picker.open(None, &context);
        assert!(picker.is_open());
    }
}

#[test]
fn repeated_open_does_not_replace_an_active_dialog_request() {
    let context = egui::Context::default();
    let mut picker = FolderPicker::default();
    picker.open(Some("first".into()), &context);
    picker.open(Some("second".into()), &context);
    assert_eq!(picker.requested, Some(Some("first".into())));
}
