use super::*;

#[test]
fn queue_back_returns_to_previous_view_and_clear_icon_obeys_availability() {
    for size in [[950.0, 620.0], [1150.0, 760.0]] {
        let (sender, mut receiver) = command_channel(64);
        let (state, snapshots) = snapshot_channel(populated());
        let mut app = PlayerApp::new(sender, snapshots);
        let context = egui::Context::default();
        for previous in [View::Library, View::Playlists, View::ScanFolders] {
            app.view = previous;
            frame(&mut app, &context, size, Vec::new());
            frame(&mut app, &context, size, Vec::new());
            let queue = app.queue_button.unwrap().center();
            click_at(&mut app, &context, size, queue);
            frame(&mut app, &context, size, Vec::new());
            assert_eq!(app.view, View::Queue);
            let back = app.back_button.unwrap();
            let clear = app.clear_button.unwrap();
            assert_eq!(back.size(), egui::vec2(28.0, 28.0));
            assert_eq!(clear.size(), back.size());
            assert!(back.right() < clear.left());
            click_at(&mut app, &context, size, back.center());
            assert_eq!(app.view, previous);
            assert!(receiver.try_recv().is_err());
        }
        let queue = app.queue_button.unwrap().center();
        click_at(&mut app, &context, size, queue);
        frame(&mut app, &context, size, Vec::new());
        let clear = app.clear_button.unwrap().center();
        click_at(&mut app, &context, size, clear);
        assert!(matches!(
            receiver.try_recv(),
            Ok(AppCommand::Playback(PlaybackCommand::ClearQueue))
        ));
        assert!(receiver.try_recv().is_err());
        state.send_modify(|state| state.queue = Arc::default());
        frame(&mut app, &context, size, Vec::new());
        click_at(&mut app, &context, size, clear);
        assert!(receiver.try_recv().is_err());
        state.send_modify(|state| state.queue = Arc::new(vec![track(1)]));
        app.outbox.disconnected = true;
        frame(&mut app, &context, size, Vec::new());
        click_at(&mut app, &context, size, clear);
        assert!(app.outbox.pending.is_empty());
        let back = app.back_button.unwrap().center();
        click_at(&mut app, &context, size, back);
        assert_eq!(app.view, View::ScanFolders);
    }
}
