use super::*;

#[test]
fn folder_and_playlist_lists_share_track_row_backgrounds() {
    for size in [[950.0, 620.0], [1150.0, 760.0]] {
        let mut snapshot = populated();
        snapshot.selected_playlist = None;
        snapshot.playback.track = None;
        snapshot.library.tracks = Arc::new(vec![track(1), track(2)]);
        snapshot.library.total = 2;
        snapshot.roots = vec!["/music/one".into(), "/music/two".into()];
        snapshot.playlists.push(Playlist {
            id: 2,
            name: "Second".into(),
        });
        let mut folder = snapshot.folders.folders[0].clone();
        folder.id = 2;
        folder.path = "/music/second".into();
        Arc::make_mut(&mut snapshot.folders.folders).push(folder);
        snapshot.folders.total = 2;
        let mut expected = None;
        for (view, tab) in [
            (View::Library, LibraryTab::Tracks),
            (View::Library, LibraryTab::Folders),
            (View::ScanFolders, LibraryTab::Tracks),
            (View::Playlists, LibraryTab::Tracks),
        ] {
            let (sender, _receiver) = command_channel(64);
            let (_state, snapshots) = snapshot_channel(snapshot.clone());
            let mut app = PlayerApp::new(sender, snapshots);
            app.view = view;
            app.tab = tab;
            let context = egui::Context::default();
            frame(&mut app, &context, size, Vec::new());
            let output = frame(&mut app, &context, size, Vec::new());
            let fills: Vec<_> = output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::Shape::Rect(rect)
                        if rect.rect.height() == 54.0
                            && rect.rect.width() > 150.0
                            && rect.rect.left() > 205.0
                            && rect.rect.right() < app.volume_icon.unwrap().left() =>
                    {
                        Some(rect.fill)
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(fills.len(), 2, "rows in {view:?}/{tab:?}");
            assert_ne!(fills[0], fills[1]);
            assert_eq!(&fills, expected.get_or_insert(fills.clone()));
        }
    }
}

#[test]
fn scan_folder_minus_removes_its_own_path_and_obeys_scan_and_import_guards() {
    for size in [[950.0, 620.0], [1150.0, 760.0]] {
        let mut snapshot = populated();
        snapshot.roots = vec!["/music/first".into(), "/music/second".into()];
        let (sender, mut receiver) = command_channel(64);
        let (state, snapshots) = snapshot_channel(snapshot);
        let mut app = PlayerApp::new(sender, snapshots);
        app.view = View::ScanFolders;
        let context = egui::Context::default();
        frame(&mut app, &context, size, Vec::new());
        frame(&mut app, &context, size, Vec::new());
        let minus = app.scan_remove[1];
        assert_eq!(minus.size(), egui::vec2(28.0, 28.0));
        click_at(&mut app, &context, size, minus.center());
        assert!(
            matches!(receiver.try_recv(), Ok(AppCommand::Library(LibraryCommand::RemoveScanFolder(path))) if path == std::path::Path::new("/music/second"))
        );
        assert!(receiver.try_recv().is_err());
        for (running, queued) in [(true, 0), (false, 1)] {
            state.send_modify(|snapshot| {
                snapshot.scan.running = running;
                snapshot.queued_imports = queued;
            });
            frame(&mut app, &context, size, Vec::new());
            click_at(&mut app, &context, size, minus.center());
            assert!(receiver.try_recv().is_err());
        }
    }
}
