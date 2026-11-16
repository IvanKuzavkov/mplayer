use super::*;

#[test]
fn folder_details_expand_independently_and_rescan_uses_icon_and_scan_state() {
    for size in [[950.0, 620.0], [1150.0, 760.0]] {
        let mut snapshot = populated();
        let folders = Arc::make_mut(&mut snapshot.folders.folders);
        folders[0].status = mplayer_core::ScanStatus::Error;
        folders[0].error = Some("Cannot read album".into());
        let mut second = folders[0].clone();
        second.id = 2;
        second.path = "/music/Another album".into();
        second.status = mplayer_core::ScanStatus::Ready;
        second.error = None;
        folders.push(second);
        let mut long = folders[0].clone();
        long.id = 3;
        long.path = format!("/music/{}", "A long folder name ".repeat(20)).into();
        folders.push(long);
        snapshot.folders.total = 3;
        let (sender, mut receiver) = command_channel(64);
        let (state, snapshots) = snapshot_channel(snapshot);
        let mut app = PlayerApp::new(sender, snapshots);
        app.tab = LibraryTab::Folders;
        let context = egui::Context::default();
        frame(&mut app, &context, size, Vec::new());
        let output = frame(&mut app, &context, size, Vec::new());
        let labels = texts(&output);
        assert!(labels.iter().any(|(text, _)| text == "Error"));
        assert!(!labels.iter().any(|(text, _)| text == "/music/Album"
            || text == "Cannot read album"
            || text == "Rescan"));
        for [expand, rescan] in &app.folder_actions {
            assert_eq!(expand.size(), egui::vec2(28.0, 28.0));
            assert_eq!(expand.size(), rescan.size());
            assert!(expand.right() < rescan.left());
            assert!(rescan.right() <= app.library_scroll.unwrap().right());
        }
        let expand = app.folder_actions[0][0].center();
        click_at(&mut app, &context, size, expand);
        let mut output = frame(&mut app, &context, size, Vec::new());
        for _ in 0..20 {
            output = frame(&mut app, &context, size, Vec::new());
        }
        let labels = texts(&output);
        for expected in ["/music/Album", "Cannot read album", "256 tracks"] {
            assert!(
                labels.iter().any(|(text, _)| text == expected),
                "missing {expected}"
            );
        }
        assert!(
            !labels
                .iter()
                .any(|(text, _)| text == "/music/Another album")
        );
        assert!(app.group.is_none());
        assert!(receiver.try_recv().is_err());
        let rescan = app.folder_actions[0][1].center();
        click_at(&mut app, &context, size, rescan);
        assert!(matches!(
            receiver.try_recv(),
            Ok(AppCommand::Library(LibraryCommand::RescanFolder(1)))
        ));
        assert!(app.group.is_none());
        state.send_modify(|snapshot| snapshot.scan.running = true);
        click_at(&mut app, &context, size, rescan);
        assert!(receiver.try_recv().is_err());
        click_at(&mut app, &context, size, expand);
        for _ in 0..20 {
            output = frame(&mut app, &context, size, Vec::new());
        }
        assert!(
            !texts(&output)
                .iter()
                .any(|(text, _)| text == "/music/Album")
        );
    }
}

#[test]
fn folder_header_replaces_search_with_path_count_status_and_tracks() {
    for size in [[950.0, 620.0], [1150.0, 760.0]] {
        for failed in [false, true] {
            let mut snapshot = populated();
            let path = PathBuf::from("/music/Album");
            snapshot.library_filter = LibraryFilter::Folder(path.clone());
            snapshot.folders.query = "album search".into();
            if failed {
                let folder = &mut Arc::make_mut(&mut snapshot.folders.folders)[0];
                folder.status = mplayer_core::ScanStatus::Error;
                folder.error = Some("Cannot read album".into());
            }
            let (sender, _receiver) = command_channel(64);
            let (_state, snapshots) = snapshot_channel(snapshot);
            let mut app = PlayerApp::new(sender, snapshots);
            app.tab = LibraryTab::Folders;
            app.group = Some(LibraryFilter::Folder(path));
            let context = egui::Context::default();
            frame(&mut app, &context, size, Vec::new());
            let output = frame(&mut app, &context, size, Vec::new());
            let labels = texts(&output);
            let path_rect = labels
                .iter()
                .find(|(text, _)| text == "/music/Album")
                .unwrap()
                .1;
            let count_rect = labels
                .iter()
                .find(|(text, _)| text == "256 tracks")
                .unwrap()
                .1;
            let status = if failed {
                "Cannot read album"
            } else {
                "Status: OK"
            };
            let status_rect = labels.iter().find(|(text, _)| text == status).unwrap().1;
            assert!(path_rect.bottom() <= count_rect.top());
            assert!(count_rect.bottom() <= status_rect.top());
            // Glyph ink bearings differ between the large path and smaller count.
            assert!((path_rect.left() - count_rect.left()).abs() <= 3.0);
            assert!((count_rect.left() - status_rect.left()).abs() < 2.0);
            assert!(status_rect.bottom() < app.library_scroll.unwrap().top());
            assert_eq!(
                labels.iter().any(|(text, _)| text == "Cannot read album"),
                failed
            );
            assert!(labels.iter().any(|(text, _)| text == "Track 1"));
            assert!(!labels.iter().any(|(text, _)| text.starts_with("Search ")));
            assert!(app.clear_button.is_none());
            let back = app.back_button.unwrap().center();
            click_at(&mut app, &context, size, back);
            assert!(app.group.is_none());
            assert_eq!(app.search, "album search");
        }
    }
}

#[test]
fn folder_header_does_not_show_another_folders_summary_while_loading() {
    let (sender, _receiver) = command_channel(64);
    let (_state, snapshots) = snapshot_channel(populated());
    let mut app = PlayerApp::new(sender, snapshots);
    app.tab = LibraryTab::Folders;
    app.group = Some(LibraryFilter::Folder("/music/New folder".into()));
    let context = egui::Context::default();
    frame(&mut app, &context, [1150.0, 760.0], Vec::new());
    let output = frame(&mut app, &context, [1150.0, 760.0], Vec::new());
    let labels = texts(&output);
    assert!(labels.iter().any(|(text, _)| text == "/music/New folder"));
    assert!(
        labels
            .iter()
            .any(|(text, _)| text == "Folder information unavailable")
    );
    assert!(labels.iter().any(|(text, _)| text == "Loading tracks…"));
    assert!(!labels.iter().any(|(text, _)| text.contains("Status: OK")));
}
