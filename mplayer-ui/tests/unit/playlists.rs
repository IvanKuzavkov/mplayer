use super::*;

#[test]
fn duplicate_playlist_notice_stays_above_creation_and_minus_deletes_only_playlist() {
    for size in [[950.0, 620.0], [1150.0, 760.0]] {
        let mut snapshot = populated();
        snapshot.selected_playlist = None;
        let (sender, mut receiver) = mpsc::channel(64);
        let (_state, snapshots) = watch::channel(snapshot);
        let mut app = PlayerApp::new(sender, snapshots);
        app.view = View::Playlists;
        let context = egui::Context::default();
        frame(&mut app, &context, size, Vec::new());
        frame(&mut app, &context, size, Vec::new());
        let create = app.playlist_create.unwrap();
        assert_eq!(create.size(), egui::vec2(28.0, 28.0));
        for name in [" Favourites ", "favourites", "FAVOURITES"] {
            app.playlist_name = name.into();
            let output = frame(&mut app, &context, size, Vec::new());
            assert!(
                !texts(&output)
                    .iter()
                    .any(|(text, _)| text.contains("already exists"))
            );
            let output = click_at(&mut app, &context, size, create.center());
            let notice = texts(&output)
                .into_iter()
                .find(|(text, _)| text == "A playlist with this name already exists.")
                .unwrap()
                .1;
            assert!(notice.bottom() < create.top());
            assert_eq!(app.playlist_create.unwrap(), create);
            assert!(receiver.try_recv().is_err());
            assert_eq!(app.playlist_name, name);
        }
        app.playlist_name = "Different".into();
        let output = frame(&mut app, &context, size, Vec::new());
        assert!(
            !texts(&output)
                .iter()
                .any(|(text, _)| text.contains("already exists"))
        );
        let minus = app.playlist_remove[0];
        assert_eq!(minus.size(), create.size());
        click_at(&mut app, &context, size, minus.center());
        assert!(matches!(
            receiver.try_recv(),
            Ok(AppCommand::DeletePlaylist(1))
        ));
        assert!(receiver.try_recv().is_err());
    }
}

fn drag_row(
    app: &mut PlayerApp,
    context: &egui::Context,
    size: [f32; 2],
    source: egui::Pos2,
    target: egui::Pos2,
) {
    frame(
        app,
        context,
        size,
        vec![
            egui::Event::PointerMoved(source),
            egui::Event::PointerButton {
                pos: source,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
        ],
    );
    frame(
        app,
        context,
        size,
        vec![egui::Event::PointerMoved(source + egui::vec2(8.0, 0.0))],
    );
    frame(app, context, size, vec![egui::Event::PointerMoved(target)]);
    frame(
        app,
        context,
        size,
        vec![egui::Event::PointerButton {
            pos: target,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        }],
    );
}

#[test]
fn playlist_header_queue_rows_removal_and_dragging_use_entry_identity() {
    for size in [[950.0, 620.0], [1150.0, 760.0]] {
        let mut snapshot = populated();
        let mut missing = track(2);
        missing.missing = true;
        snapshot.playlist_entries = Arc::new(vec![
            PlaylistEntry {
                id: 101,
                track: track(1),
            },
            PlaylistEntry {
                id: 102,
                track: track(1),
            },
            PlaylistEntry {
                id: 103,
                track: missing,
            },
        ]);
        let (sender, mut receiver) = mpsc::channel(64);
        let (state, snapshots) = watch::channel(snapshot);
        let mut app = PlayerApp::new(sender, snapshots);
        app.view = View::Playlists;
        let context = egui::Context::default();
        frame(&mut app, &context, size, Vec::new());
        let output = frame(&mut app, &context, size, Vec::new());
        let labels = texts(&output);
        assert!(labels.iter().any(|(text, _)| text == "Favourites"));
        assert!(labels.iter().any(|(text, _)| text == "3 tracks"));
        assert!(
            !labels
                .iter()
                .any(|(text, _)| matches!(text.as_str(), "Up" | "Down" | "Remove" | "01"))
        );
        let play = app.playlist_play.unwrap();
        assert_eq!(play.size(), egui::vec2(28.0, 28.0));
        let name = labels
            .iter()
            .find(|(text, _)| text == "Favourites")
            .unwrap()
            .1;
        let count = labels
            .iter()
            .find(|(text, _)| text == "3 tracks")
            .unwrap()
            .1;
        assert!(name.bottom() < count.top());
        assert!((name.left() - count.left()).abs() < 2.0);
        assert!((count.center().y - play.center().y).abs() < 3.0);
        assert!(count.right() < play.left());
        assert!(play.bottom() < app.playlist_rows[0].top());
        assert!(play.right() < app.volume_icon.unwrap().left());
        click_at(&mut app, &context, size, play.center());
        assert!(matches!(
            receiver.try_recv(),
            Ok(AppCommand::PlayPlaylist(1))
        ));
        let minus = app.queue_buttons[2].center();
        click_at(&mut app, &context, size, minus);
        assert!(matches!(
            receiver.try_recv(),
            Ok(AppCommand::RemovePlaylistEntry(103))
        ));
        assert!(receiver.try_recv().is_err());
        let source = app.playlist_rows[0].center();
        let target = app.playlist_rows[2].center_bottom() - egui::vec2(0.0, 4.0);
        drag_row(&mut app, &context, size, source, target);
        assert!(matches!(
            receiver.try_recv(),
            Ok(AppCommand::MovePlaylistEntry {
                entry_id: 101,
                position: 2
            })
        ));
        assert!(receiver.try_recv().is_err());
        state.send_modify(|snapshot| {
            let entries = Arc::make_mut(&mut snapshot.playlist_entries);
            let moved = entries.remove(0);
            entries.insert(2, moved);
        });
        frame(&mut app, &context, size, Vec::new());
        let source = app.playlist_rows[2].center();
        let target = app.playlist_rows[0].center_top() + egui::vec2(0.0, 4.0);
        drag_row(&mut app, &context, size, source, target);
        assert!(matches!(
            receiver.try_recv(),
            Ok(AppCommand::MovePlaylistEntry {
                entry_id: 101,
                position: 0
            })
        ));
        assert!(receiver.try_recv().is_err());
        let minus = app.queue_buttons[0].center();
        click_at(&mut app, &context, size, minus);
        assert!(matches!(
            receiver.try_recv(),
            Ok(AppCommand::RemovePlaylistEntry(102))
        ));
    }
}

#[test]
fn playlist_overview_creates_opens_and_returns_from_detail() {
    let mut snapshot = populated();
    snapshot.selected_playlist = None;
    snapshot.playlist_entries = Arc::default();
    let (sender, mut receiver) = mpsc::channel(64);
    let (state, snapshots) = watch::channel(snapshot.clone());
    let mut app = PlayerApp::new(sender, snapshots);
    let context = egui::Context::default();
    click(&mut app, &context, "Playlists");
    frame(&mut app, &context, [1150.0, 760.0], Vec::new());
    let create = app.playlist_create.unwrap().center();
    click_at(&mut app, &context, [1150.0, 760.0], create);
    assert!(
        receiver.try_recv().is_err(),
        "empty names cannot be created"
    );
    app.playlist_name = "  Weekend  ".into();
    frame(&mut app, &context, [1150.0, 760.0], Vec::new());
    let create = app.playlist_create.unwrap().center();
    click_at(&mut app, &context, [1150.0, 760.0], create);
    assert!(
        matches!(receiver.try_recv(), Ok(AppCommand::CreatePlaylist(name)) if name == "Weekend")
    );
    assert!(app.playlist_name.is_empty());
    click(&mut app, &context, "Favourites");
    assert!(matches!(
        receiver.try_recv(),
        Ok(AppCommand::SelectPlaylist(Some(1)))
    ));
    snapshot.selected_playlist = Some(1);
    state.send_replace(snapshot.clone());
    let output = frame(&mut app, &context, [1150.0, 760.0], Vec::new());
    assert!(
        texts(&output)
            .iter()
            .any(|(text, _)| text.starts_with("Add tracks from their context menu"))
    );
    let play = app.playlist_play.unwrap().center();
    click_at(&mut app, &context, [1150.0, 760.0], play);
    assert!(receiver.try_recv().is_err(), "empty playlists cannot play");
    let back = app.back_button.unwrap().center();
    click_at(&mut app, &context, [1150.0, 760.0], back);
    assert!(matches!(
        receiver.try_recv(),
        Ok(AppCommand::SelectPlaylist(None))
    ));
    snapshot.selected_playlist = None;
    state.send_replace(snapshot);
    let output = frame(&mut app, &context, [1150.0, 760.0], Vec::new());
    assert!(app.playlist_create.is_some());
    assert!(!texts(&output).iter().any(|(text, _)| text == "Create"));
}
