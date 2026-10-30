use super::*;

#[test]
fn artwork_hover_shows_album_actions_without_opening_album_or_old_tooltip() {
    for size in [[950.0, 620.0], [1150.0, 760.0]] {
        for detail in [false, true] {
            let mut snapshot = populated();
            let key = snapshot.albums.albums[0].key.clone();
            if detail {
                snapshot.library_filter = LibraryFilter::Album(key.clone());
            }
            let (sender, mut receiver) = mpsc::channel(64);
            let (_state, snapshots) = watch::channel(snapshot);
            let mut app = PlayerApp::new(sender, snapshots);
            app.tab = LibraryTab::Albums;
            if detail {
                app.group = Some(LibraryFilter::Album(key.clone()));
            }
            let context = egui::Context::default();
            frame(&mut app, &context, size, Vec::new());
            frame(&mut app, &context, size, Vec::new());
            assert!(app.album_action_buttons.is_empty());
            let cover = app.album_artworks[0];
            frame(
                &mut app,
                &context,
                size,
                vec![egui::Event::PointerMoved(cover.center())],
            );
            let [enqueue, play] = app.album_action_buttons[0];
            assert_eq!(enqueue.size(), egui::vec2(28.0, 28.0));
            assert_eq!(play.size(), enqueue.size());
            assert!(cover.contains_rect(enqueue));
            assert!(cover.contains_rect(play));
            assert!(enqueue.right() < play.left());
            click_at(&mut app, &context, size, enqueue.center());
            assert!(
                matches!(receiver.try_recv(), Ok(AppCommand::EnqueueAlbum(album)) if album == key)
            );
            assert!(receiver.try_recv().is_err());
            assert_eq!(app.group.is_some(), detail);
            click_at(&mut app, &context, size, play.center());
            assert!(
                matches!(receiver.try_recv(), Ok(AppCommand::PlayAlbum(album)) if album == key)
            );
            assert!(receiver.try_recv().is_err());
            assert_eq!(app.group.is_some(), detail);
            frame(
                &mut app,
                &context,
                size,
                vec![egui::Event::PointerMoved(egui::pos2(0.0, 0.0))],
            );
            assert!(app.album_action_buttons.is_empty());
        }
    }
}
