use super::*;
use mplayer_core::{
    AlbumKey, AlbumPage, AlbumSummary, FolderPage, FolderSummary, LibraryPage, PlaybackSnapshot,
    Playlist, PlaylistEntry, TrackMetadata,
};
use std::path::PathBuf;

fn track(id: i64) -> Track {
    Track {
        id,
        path: PathBuf::from(format!("/music/Album/{id}.flac")),
        missing: false,
        segment: None,
        metadata: TrackMetadata {
            title: format!("Track {id}"),
            artist: "An artist".into(),
            album: "An album".into(),
            duration_ms: 185_000,
            sample_rate: 44_100,
            channels: 2,
            bit_depth: Some(24),
            codec: "flac".into(),
            ..Default::default()
        },
    }
}
fn populated() -> AppSnapshot {
    let tracks: Vec<_> = (0..256).map(track).collect();
    AppSnapshot {
        library: LibraryPage {
            tracks: Arc::new(tracks.clone()),
            total: 512,
            ..Default::default()
        },
        folders: FolderPage {
            folders: Arc::new(vec![FolderSummary {
                id: 1,
                generation: 1,
                status: mplayer_core::ScanStatus::Ready,
                error: None,
                path: "/music/Album".into(),
                track_count: 256,
            }]),
            total: 1,
            ..Default::default()
        },
        albums: AlbumPage {
            albums: Arc::new(vec![AlbumSummary {
                year: None,
                key: AlbumKey {
                    title: "An album".into(),
                    artist: "An artist".into(),
                },
                track_count: 256,
                duration_ms: 185_000 * 256,
                artwork_path: Some(tracks[0].path.clone()),
            }]),
            total: 1,
            ..Default::default()
        },
        roots: vec!["/music".into()],
        playlists: vec![Playlist {
            id: 1,
            name: "Favourites".into(),
        }],
        selected_playlist: Some(1),
        playlist_entries: Arc::new(vec![PlaylistEntry {
            id: 1,
            track: tracks[0].clone(),
        }]),
        queue: Arc::new(tracks[..8].to_vec()),
        queue_index: Some(0),
        playback: PlaybackSnapshot {
            track: Some(tracks[0].clone()),
            state: PlaybackState::Playing,
            source_rate: 44_100,
            output_rate: 48_000,
            duration_ms: 185_000,
            volume: 0.75,
            devices: vec!["Failed device".into(), "Working device".into()],
            ..Default::default()
        },
        ..Default::default()
    }
}
fn text_shapes(shape: &egui::Shape, output: &mut Vec<(String, egui::Rect)>) {
    match shape {
        egui::Shape::Text(text) => {
            output.push((text.galley.job.text.clone(), text.visual_bounding_rect()))
        }
        egui::Shape::Vec(shapes) => {
            for shape in shapes {
                text_shapes(shape, output)
            }
        }
        _ => (),
    }
}
fn frame(
    app: &mut PlayerApp,
    context: &egui::Context,
    size: [f32; 2],
    events: Vec<egui::Event>,
) -> egui::FullOutput {
    let mut output = context.run_ui(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(size[0], size[1]),
            )),
            events,
            system_theme: context.system_theme(),
            ..Default::default()
        },
        |ui| app.show(ui),
    );
    output.textures_delta.clear();
    output
}
fn texts(output: &egui::FullOutput) -> Vec<(String, egui::Rect)> {
    let mut result = Vec::new();
    for shape in &output.shapes {
        text_shapes(&shape.shape, &mut result);
    }
    result
}
fn click(app: &mut PlayerApp, context: &egui::Context, label: &str) -> egui::FullOutput {
    frame(app, context, [1150.0, 760.0], Vec::new());
    let output = frame(app, context, [1150.0, 760.0], Vec::new());
    let position = texts(&output)
        .into_iter()
        .find(|(text, _)| text == label)
        .unwrap_or_else(|| panic!("missing {label}"))
        .1
        .center();
    click_at(app, context, [1150.0, 760.0], position)
}

fn click_at(
    app: &mut PlayerApp,
    context: &egui::Context,
    size: [f32; 2],
    position: egui::Pos2,
) -> egui::FullOutput {
    frame(
        app,
        context,
        size,
        vec![
            egui::Event::PointerMoved(position),
            egui::Event::PointerButton {
                pos: position,
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
        vec![egui::Event::PointerButton {
            pos: position,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        }],
    )
}

#[test]
fn all_views_and_library_tabs_render_at_both_window_sizes() {
    for system_theme in [egui::Theme::Light, egui::Theme::Dark] {
        for size in [[950.0, 620.0], [1150.0, 760.0]] {
            for view in [
                View::Library,
                View::ScanFolders,
                View::Queue,
                View::Playlists,
            ] {
                for tab in [LibraryTab::Tracks, LibraryTab::Folders, LibraryTab::Albums] {
                    let (sender, _receiver) = mpsc::channel(64);
                    let (_state, snapshots) = watch::channel(populated());
                    let mut app = PlayerApp::new(sender, snapshots);
                    app.view = view;
                    app.tab = tab;
                    let context = egui::Context::default();
                    let mut initial = context.run_ui(
                        egui::RawInput {
                            system_theme: Some(system_theme),
                            ..Default::default()
                        },
                        |_| {},
                    );
                    initial.textures_delta.clear();
                    frame(&mut app, &context, size, Vec::new());
                    let output = frame(&mut app, &context, size, Vec::new());
                    assert_eq!(context.theme(), system_theme);
                    assert!(!output.shapes.is_empty());
                    for shape in &output.shapes {
                        assert!(shape.clip_rect.is_finite());
                    }
                    let button = app.output_button.expect("output picker remains visible");
                    assert!(button.top() < 60.0 && button.right() <= size[0]);
                    assert!(!texts(&output).iter().any(|(text, _)| matches!(
                        text.as_str(),
                        "OUTPUT DEVICE" | "Volume" | "NOW PLAYING"
                    )));
                    assert!(
                        !context
                            .tessellate(output.shapes, output.pixels_per_point)
                            .is_empty()
                    );
                    if view == View::Library {
                        let scroll = app
                            .library_scroll
                            .expect("library must allocate its scrolling area");
                        assert!(
                            scroll.height() > size[1] - 260.0,
                            "scroll should fill library area: {scroll:?}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn system_theme_changes_update_panels_text_and_custom_controls_without_restart() {
    fn contrast(a: egui::Color32, b: egui::Color32) -> f32 {
        let luminance = |color: egui::Color32| {
            let linear = |value: u8| {
                let value = f32::from(value) / 255.0;
                if value <= 0.04045 {
                    value / 12.92
                } else {
                    ((value + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * linear(color.r()) + 0.7152 * linear(color.g()) + 0.0722 * linear(color.b())
        };
        let a = luminance(a);
        let b = luminance(b);
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }
    fn title_colors(shape: &egui::Shape, colors: &mut Vec<egui::Color32>) {
        match shape {
            egui::Shape::Vec(shapes) => {
                for shape in shapes {
                    title_colors(shape, colors);
                }
            }
            egui::Shape::Text(text) if text.galley.job.text == "Track 1" => {
                for section in &text.galley.job.sections {
                    colors.push(text.override_text_color.unwrap_or_else(|| {
                        if section.format.color == egui::Color32::PLACEHOLDER {
                            text.fallback_color
                        } else {
                            section.format.color
                        }
                    }));
                }
            }
            _ => (),
        }
    }
    let (sender, _receiver) = mpsc::channel(64);
    let (_state, snapshots) = watch::channel(populated());
    let mut app = PlayerApp::new(sender, snapshots);
    let context = egui::Context::default();
    for system_theme in [egui::Theme::Light, egui::Theme::Dark, egui::Theme::Light] {
        let mut control_colors = None;
        let mut output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1150.0, 760.0),
                )),
                system_theme: Some(system_theme),
                ..Default::default()
            },
            |ui| {
                app.show(ui);
                control_colors = Some((theme::accent(ui), ui.visuals().weak_text_color()));
            },
        );
        output.textures_delta.clear();
        let visuals = context.global_style().visuals.clone();
        assert_eq!(
            context.options(|options| options.theme_preference),
            egui::ThemePreference::System
        );
        assert_eq!(visuals.dark_mode, system_theme == egui::Theme::Dark);
        assert_eq!(
            context.style_of(egui::Theme::Dark).spacing.item_spacing,
            context.style_of(egui::Theme::Light).spacing.item_spacing
        );
        let mut colors = Vec::new();
        for shape in &output.shapes {
            title_colors(&shape.shape, &mut colors);
        }
        assert!(!colors.is_empty());
        for color in colors {
            assert!(contrast(color, visuals.panel_fill) >= 4.5);
        }
        let (accent, muted) = control_colors.unwrap();
        for background in [visuals.panel_fill, visuals.extreme_bg_color] {
            assert!(contrast(muted, background) >= 4.5);
            assert!(contrast(accent, background) >= 4.5);
        }
        assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Rect(rect) if rect.fill == visuals.panel_fill)));
        assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Rect(rect) if rect.fill == visuals.extreme_bg_color)));
        assert!(
            app.output_button.is_some() && app.mode_button.is_some() && app.queue_button.is_some()
        );
    }
}

#[test]
fn album_detail_header_replaces_search_and_keeps_tracks_below_artwork() {
    for size in [[950.0, 620.0], [1150.0, 760.0]] {
        for year in [None, Some(2026)] {
            let (sender, mut receiver) = mpsc::channel(64);
            let mut snapshot = populated();
            let key = snapshot.albums.albums[0].key.clone();
            Arc::make_mut(&mut snapshot.albums.albums)[0].year = year;
            snapshot.library_filter = LibraryFilter::Album(key.clone());
            snapshot.albums.query = "previous search".into();
            let (_state, snapshots) = watch::channel(snapshot);
            let mut app = PlayerApp::new(sender, snapshots);
            app.tab = LibraryTab::Albums;
            app.group = Some(LibraryFilter::Album(key.clone()));
            let context = egui::Context::default();
            frame(&mut app, &context, size, Vec::new());
            let output = frame(&mut app, &context, size, Vec::new());
            let labels = texts(&output);
            let cover = app.album_cover.unwrap();
            assert_eq!(cover.width(), cover.height());
            let back = app.back_button.unwrap();
            assert!(back.bottom() < cover.top());
            assert!((back.left() - cover.left()).abs() < 1.0);
            let title = labels
                .iter()
                .find(|(text, rect)| {
                    text == &key.title && rect.left() > cover.right() && rect.top() < cover.bottom()
                })
                .unwrap();
            assert!(title.1.right() <= app.library_scroll.unwrap().right());
            assert!(labels.iter().any(|(text, rect)| text == &key.artist
                && rect.left() > cover.right()
                && rect.top() < cover.bottom()));
            assert_eq!(
                labels.iter().any(|(text, _)| text == "2026"),
                year.is_some()
            );
            assert!(
                labels
                    .iter()
                    .any(|(text, rect)| text == "Track 1" && rect.top() > cover.bottom())
            );
            assert!(app.clear_button.is_none());
            assert!(
                !labels
                    .iter()
                    .any(|(text, _)| text.starts_with("Search ") || text == "Clear")
            );
            click_at(&mut app, &context, size, back.center());
            assert!(app.group.is_none());
            assert_eq!(app.search, "previous search");
            assert!(receiver.try_recv().is_err());
        }
    }
}

#[test]
fn album_header_does_not_reuse_metadata_from_a_previous_album_while_loading() {
    let (sender, _receiver) = mpsc::channel(64);
    let mut snapshot = populated();
    for track in Arc::make_mut(&mut snapshot.library.tracks) {
        track.metadata.year = Some(1999);
    }
    let (_state, snapshots) = watch::channel(snapshot);
    let mut app = PlayerApp::new(sender, snapshots);
    app.tab = LibraryTab::Albums;
    app.group = Some(LibraryFilter::Album(AlbumKey {
        title: "New album".into(),
        artist: "New artist".into(),
    }));
    let context = egui::Context::default();
    frame(&mut app, &context, [950.0, 620.0], Vec::new());
    let output = frame(&mut app, &context, [950.0, 620.0], Vec::new());
    let labels = texts(&output);
    assert!(labels.iter().any(|(text, _)| text == "New album"));
    assert!(labels.iter().any(|(text, _)| text == "Loading tracks…"));
    assert!(!labels.iter().any(|(text, _)| text == "1999"));
    assert!(app.album_cover.is_some());
}

#[test]
fn clear_search_icon_dispatches_an_empty_query_and_disables_when_empty() {
    let (sender, mut receiver) = mpsc::channel(64);
    let (_state, snapshots) = watch::channel(populated());
    let mut app = PlayerApp::new(sender, snapshots);
    app.tab = LibraryTab::Albums;
    app.search = "Draconian".into();
    let context = egui::Context::default();
    frame(&mut app, &context, [1150.0, 760.0], Vec::new());
    frame(&mut app, &context, [1150.0, 760.0], Vec::new());
    let position = app.clear_button.unwrap().center();
    click_at(&mut app, &context, [1150.0, 760.0], position);
    assert!(app.search.is_empty());
    assert!(
        matches!(receiver.try_recv(), Ok(AppCommand::ListAlbums { query, offset: 0 }) if query.is_empty())
    );
    click_at(&mut app, &context, [1150.0, 760.0], position);
    assert!(receiver.try_recv().is_err());
}

#[test]
fn library_tabs_dispatch_group_queries_and_folder_drilldown() {
    let (sender, mut receiver) = mpsc::channel(64);
    let (_state, snapshots) = watch::channel(populated());
    let mut app = PlayerApp::new(sender, snapshots);
    let context = egui::Context::default();
    click(&mut app, &context, "Folders");
    assert_eq!(app.tab, LibraryTab::Folders);
    assert!(matches!(
        receiver.try_recv(),
        Ok(AppCommand::ListFolders { offset: 0, .. })
    ));
    click(&mut app, &context, "Album");
    let command = receiver.try_recv();
    assert!(
        matches!(&command,Ok(AppCommand::BrowseLibrary{filter:LibraryFilter::Folder(path),..}) if path==&PathBuf::from("/music/Album")),
        "folder action: {command:?}, selected: {:?}",
        app.group
    );
    app.group = None;
    click(&mut app, &context, "Albums");
    assert_eq!(app.tab, LibraryTab::Albums);
    assert!(matches!(
        receiver.try_recv(),
        Ok(AppCommand::ListAlbums { offset: 0, .. })
    ));
}

#[test]
fn disconnected_output_shows_notification_instead_of_playback_error() {
    let mut snapshot = populated();
    snapshot.playback.state = PlaybackState::Paused;
    let notification = "Audio output device changed. Playback paused.";
    snapshot.playback.notification = Some(notification.into());
    let (sender, _receiver) = mpsc::channel(64);
    let (_state, snapshots) = watch::channel(snapshot);
    let mut app = PlayerApp::new(sender, snapshots);
    let context = egui::Context::default();
    let output = frame(&mut app, &context, [1150.0, 760.0], Vec::new());
    let labels = texts(&output);
    assert!(labels.iter().any(|(text, _)| text == notification));
    assert!(
        !labels
            .iter()
            .any(|(text, _)| text.contains("Playback error:"))
    );
}

#[test]
fn playback_messages_stay_above_controls_without_moving_player_content() {
    let notification = "Audio output device changed. Playback paused.";
    let long_error = "Output unavailable. ".repeat(30);
    for size in [[950.0, 620.0], [1150.0, 760.0]] {
        let mut snapshot = populated();
        let title = "Now playing title";
        snapshot.playback.track.as_mut().unwrap().metadata.title = title.into();
        let (sender, _receiver) = mpsc::channel(64);
        let (state, snapshots) = watch::channel(snapshot);
        let mut app = PlayerApp::new(sender, snapshots);
        let context = egui::Context::default();
        let artwork = |output: &egui::FullOutput| {
            output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Rect(rect)
                        if rect.fill == egui::Color32::from_rgb(37, 36, 33)
                            && rect.rect.width() > 100.0
                            && rect.rect.left() > size[0] - 360.0 =>
                    {
                        Some(rect.rect)
                    }
                    _ => None,
                })
                .expect("player artwork")
        };
        frame(&mut app, &context, size, Vec::new());
        let initial = frame(&mut app, &context, size, Vec::new());
        let cover = artwork(&initial);
        let title_rect = texts(&initial)
            .into_iter()
            .find(|(text, _)| text == title)
            .unwrap()
            .1;
        let controls = app.transport_rect.unwrap();
        let seeker = app.seek_slider.unwrap();
        for (notice, error) in [
            (Some(notification), None),
            (None, Some("Output unavailable")),
            (Some(notification), Some("Output unavailable")),
            (None, Some(long_error.as_str())),
            (None, None),
        ] {
            state.send_modify(|snapshot| {
                snapshot.playback.state = PlaybackState::Paused;
                snapshot.playback.notification = notice.map(str::to_owned);
                snapshot.playback.error = error.map(str::to_owned);
            });
            frame(&mut app, &context, size, Vec::new());
            let output = frame(&mut app, &context, size, Vec::new());
            assert_eq!(artwork(&output), cover);
            let labels = texts(&output);
            assert!(
                labels
                    .iter()
                    .any(|(text, rect)| text == title && *rect == title_rect)
            );
            assert_eq!(app.transport_rect.unwrap(), controls);
            assert_eq!(app.seek_slider.unwrap(), seeker);
            let message = error
                .map(|error| format!("Playback error: {error}"))
                .or_else(|| notice.map(str::to_owned));
            let message_rect = output.shapes.iter().find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if Some(&text.galley.job.text) == message.as_ref() => {
                    Some(text.visual_bounding_rect().intersect(shape.clip_rect))
                }
                _ => None,
            });
            if message.is_some() {
                let rect = message_rect.expect("visible playback message");
                assert!(rect.top() > title_rect.bottom());
                assert!(rect.top() > cover.bottom());
                assert!(rect.bottom() < seeker.top() - 16.0);
                if error != Some(long_error.as_str()) {
                    assert!((rect.center().x - seeker.center().x).abs() < 2.0);
                }
                if notice.is_some() && error.is_none() {
                    let mut settled = output.clone();
                    for _ in 0..20 {
                        settled = frame(&mut app, &context, size, Vec::new());
                    }
                    let settled_rect = texts(&settled)
                        .into_iter()
                        .find(|(text, _)| text == notification)
                        .unwrap()
                        .1;
                    assert!(settled_rect.top() < rect.top());
                    assert_eq!(app.transport_rect.unwrap(), controls);
                    assert_eq!(artwork(&settled), cover);
                }
            } else {
                assert!(message_rect.is_none());
            }
            assert_eq!(
                labels.iter().any(|(text, _)| text == notification),
                notice.is_some() && error.is_none()
            );
        }
    }
}

#[test]
fn failed_output_still_allows_selecting_another_device_without_scrolling() {
    let mut snapshot = populated();
    snapshot.playback.state = PlaybackState::Error;
    snapshot.playback.error = Some("Device unplugged".into());
    snapshot.playback.selected_device = Some("Failed device".into());
    let (sender, mut receiver) = mpsc::channel(64);
    let (_state, snapshots) = watch::channel(snapshot);
    let mut app = PlayerApp::new(sender, snapshots);
    let context = egui::Context::default();
    frame(&mut app, &context, [1150.0, 760.0], Vec::new());
    let button = app.output_button.unwrap().center();
    click_at(&mut app, &context, [1150.0, 760.0], button);
    let output = frame(&mut app, &context, [1150.0, 760.0], Vec::new());
    assert!(
        texts(&output)
            .iter()
            .any(|(text, _)| text == "Working device"),
        "popup texts: {:?}",
        texts(&output)
    );
    click(&mut app, &context, "Working device");
    assert!(
        matches!(receiver.try_recv(),Ok(AppCommand::SetDevice(Some(name))) if name=="Working device")
    );
    assert!(!app.outbox.disconnected);
}

#[test]
fn final_error_snapshot_is_visible_after_service_shutdown() {
    let (sender, _receiver) = mpsc::channel(64);
    let (state, snapshots) = watch::channel(AppSnapshot::default());
    let mut app = PlayerApp::new(sender, snapshots);
    state.send_replace(AppSnapshot {
        status: "Initialization failed".into(),
        ..Default::default()
    });
    drop(state);
    app.refresh(&egui::Context::default());
    assert_eq!(app.snapshot.status, "Initialization failed");
    assert!(app.outbox.disconnected);
}
#[test]
fn outbox_retries_in_order_and_preserves_discrete_controls() {
    let (sender, mut receiver) = mpsc::channel(1);
    sender.try_send(AppCommand::Stop).unwrap();
    let mut outbox = Outbox::default();
    outbox.push(AppCommand::Next);
    outbox.push(AppCommand::Seek {
        position_ms: 10,
        request_id: 10,
    });
    outbox.push(AppCommand::Seek {
        position_ms: 20,
        request_id: 20,
    });
    assert_eq!(outbox.pending.len(), 2);
    outbox.flush(&sender);
    assert!(matches!(receiver.try_recv(), Ok(AppCommand::Stop)));
    outbox.flush(&sender);
    assert!(matches!(receiver.try_recv(), Ok(AppCommand::Next)));
    outbox.flush(&sender);
    assert!(matches!(
        receiver.try_recv(),
        Ok(AppCommand::Seek {
            position_ms: 20,
            request_id: 20
        })
    ));
    drop(receiver);
    outbox.push(AppCommand::Stop);
    outbox.flush(&sender);
    assert!(outbox.disconnected);
    assert!(!outbox.push(AppCommand::Next));
}
#[test]
fn durations_and_missing_titles_have_readable_fallbacks() {
    assert_eq!(theme::duration(999), "0:00");
    assert_eq!(theme::duration(3_661_000), "1:01:01");
    let mut track = track(8);
    track.metadata.title.clear();
    assert_eq!(track_title(&track), "8");
}

#[test]
fn filtered_search_coalesces_without_changing_group_when_rejected() {
    let (sender, _receiver) = mpsc::channel(1);
    sender.try_send(AppCommand::Stop).unwrap();
    let (_state, snapshots) = watch::channel(populated());
    let mut app = PlayerApp::new(sender, snapshots);
    let mut outbox = Outbox::default();
    outbox.push(AppCommand::BrowseLibrary {
        filter: LibraryFilter::All,
        query: "a".into(),
        offset: 0,
    });
    outbox.push(AppCommand::BrowseLibrary {
        filter: LibraryFilter::All,
        query: "album".into(),
        offset: 0,
    });
    outbox.push(AppCommand::BrowseLibrary {
        filter: LibraryFilter::Folder("/music".into()),
        query: "other".into(),
        offset: 0,
    });
    assert_eq!(outbox.pending.len(), 2);
    assert!(
        matches!(outbox.pending.front(), Some(AppCommand::BrowseLibrary { query, .. }) if query == "album")
    );
    for _ in 0..MAX_PENDING {
        app.outbox.push(AppCommand::Next);
    }
    app.open_group(LibraryFilter::Folder("/new-folder".into()));
    assert!(app.group.is_none());
    assert!(app.message.as_deref().unwrap().contains("busy"));
}

#[test]
fn native_folder_action_remains_available_while_scanning() {
    let mut snapshot = populated();
    snapshot.roots.clear();
    snapshot.scan.running = true;
    let (sender, mut receiver) = mpsc::channel(64);
    let (_state, snapshots) = watch::channel(snapshot);
    let mut app = PlayerApp::new(sender, snapshots);
    let context = egui::Context::default();
    click(&mut app, &context, "Scan folders");
    let add = app.scan_actions[0].unwrap();
    click_at(&mut app, &context, [1150.0, 760.0], add.center());
    assert!(app.folders.is_open());
    let roots = vec![
        PathBuf::from("/first/music"),
        PathBuf::from("/second/music"),
    ];
    app.folders.complete_for_test(roots.clone());
    frame(&mut app, &context, [1150.0, 760.0], Vec::new());
    assert!(matches!(receiver.try_recv(),Ok(AppCommand::Import(paths)) if paths==roots));
    assert!(!app.folders.is_open());
    assert_eq!(app.view, View::ScanFolders);
}

#[test]
fn sidebar_contains_only_three_tabs_and_queue_is_available_from_player() {
    let (sender, _receiver) = mpsc::channel(64);
    let (_state, snapshots) = watch::channel(populated());
    let mut app = PlayerApp::new(sender, snapshots);
    let context = egui::Context::default();
    frame(&mut app, &context, [1150.0, 760.0], Vec::new());
    let output = frame(&mut app, &context, [1150.0, 760.0], Vec::new());
    let labels: Vec<_> = texts(&output)
        .into_iter()
        .filter(|(_, rect)| rect.right() < 205.0 && rect.bottom() < 731.0)
        .map(|(text, _)| text)
        .collect();
    assert_eq!(labels, ["Media library", "Scan folders", "Playlists"]);
    assert!(
        !texts(&output)
            .iter()
            .any(|(text, _)| text == "Music library")
    );
    for (label, view) in [
        ("Scan folders", View::ScanFolders),
        ("Playlists", View::Playlists),
        ("Media library", View::Library),
    ] {
        click(&mut app, &context, label);
        assert_eq!(app.view, view);
    }
    let queue = app.queue_button.unwrap().center();
    click_at(&mut app, &context, [1150.0, 760.0], queue);
    assert_eq!(app.view, View::Queue);
    click_at(&mut app, &context, [1150.0, 760.0], queue);
    assert_eq!(app.view, View::Library);
}

#[test]
fn scan_folder_rows_are_passive_and_actions_work_with_a_long_list() {
    for size in [[950.0, 620.0], [1150.0, 760.0]] {
        let mut snapshot = populated();
        snapshot.roots = (0..100)
            .map(|index| PathBuf::from(format!("/music/{index:03}")))
            .collect();
        let selected = snapshot.roots[0].clone();
        let (sender, mut receiver) = mpsc::channel(64);
        let (state, snapshots) = watch::channel(snapshot.clone());
        let mut app = PlayerApp::new(sender, snapshots);
        app.view = View::ScanFolders;
        let context = egui::Context::default();
        frame(&mut app, &context, size, Vec::new());
        let output = frame(&mut app, &context, size, Vec::new());
        let remove = app.scan_remove[0];
        for action in app.scan_actions.into_iter().flatten() {
            assert!(
                action.bottom() < size[1] - 29.0 && action.top() > size[1] - 100.0,
                "actions must stay at the bottom: {action:?}"
            );
        }
        let row = texts(&output)
            .into_iter()
            .find(|(text, _)| text == "/music/000")
            .unwrap()
            .1;
        click_at(&mut app, &context, size, row.center());
        assert!(
            receiver.try_recv().is_err(),
            "clicking a folder row is passive"
        );
        frame(&mut app, &context, size, Vec::new());
        click_at(&mut app, &context, size, remove.center());
        assert!(
            matches!(receiver.try_recv(), Ok(AppCommand::RemoveScanFolder(path)) if path == selected)
        );
        let rescan = app.scan_actions[1].unwrap().center();
        click_at(&mut app, &context, size, rescan);
        assert!(matches!(receiver.try_recv(), Ok(AppCommand::Rescan)));
        snapshot.scan.running = true;
        state.send_replace(snapshot);
        frame(&mut app, &context, size, Vec::new());
        click_at(&mut app, &context, size, remove.center());
        click_at(&mut app, &context, size, rescan);
        assert!(
            receiver.try_recv().is_err(),
            "removal and rescan are disabled while scanning"
        );
        click(&mut app, &context, "Cancel");
        assert!(matches!(receiver.try_recv(), Ok(AppCommand::CancelScan)));
    }
}

#[tokio::test]
async fn embedded_artwork_is_uploaded_and_drawn_in_the_now_playing_panel() {
    use image::ImageEncoder;
    let mut encoded = Vec::new();
    image::codecs::png::PngEncoder::new(&mut encoded)
        .write_image(
            &[255, 20, 10, 255, 20, 255, 10, 255],
            2,
            1,
            image::ExtendedColorType::Rgba8,
        )
        .unwrap();
    let mut snapshot = populated();
    snapshot.artwork = Some(mplayer_core::Artwork {
        data: encoded.into(),
        mime_type: "image/png".into(),
    });
    let (sender, _receiver) = mpsc::channel(64);
    let (_state, snapshots) = watch::channel(snapshot);
    let mut app = PlayerApp::new(sender, snapshots);
    let context = egui::Context::default();
    tokio::time::timeout(Duration::from_secs(3), async {
        while app.cover.texture().is_none() {
            frame(&mut app, &context, [1150.0, 760.0], Vec::new());
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let texture = app.cover.texture().unwrap().id();
    let output = frame(&mut app, &context, [1150.0, 760.0], Vec::new());
    let primitives = context.tessellate(output.shapes, output.pixels_per_point);
    assert!(primitives.iter().any(|primitive|matches!(&primitive.primitive,egui::epaint::Primitive::Mesh(mesh) if mesh.texture_id==texture)),"the decoded cover must reach the rendered player");
}

#[test]
fn seek_preview_waits_for_its_acknowledgement_and_tracks_dragging() {
    let (sender, mut receiver) = mpsc::channel(64);
    let (state, snapshots) = watch::channel(populated());
    let mut app = PlayerApp::new(sender, snapshots);
    let context = egui::Context::default();
    app.seek_to(60_000);
    let first_id = match receiver.try_recv().unwrap() {
        AppCommand::Seek { request_id, .. } => request_id,
        command => panic!("expected seek: {command:?}"),
    };
    state.send_modify(|snapshot| snapshot.playback.position_ms = 1000);
    app.refresh(&context);
    assert_eq!(app.displayed_position(&app.snapshot.playback), 60_000.0);
    app.seek_to(120_000);
    let second_id = match receiver.try_recv().unwrap() {
        AppCommand::Seek { request_id, .. } => request_id,
        command => panic!("expected seek: {command:?}"),
    };
    assert_ne!(first_id, second_id);
    state.send_modify(|snapshot| {
        snapshot.playback.seek_request_id = first_id;
        snapshot.playback.position_ms = 60_050;
    });
    app.refresh(&context);
    assert_eq!(app.displayed_position(&app.snapshot.playback), 120_000.0);
    app.seek_draft = Some(90_000.0);
    state.send_modify(|snapshot| {
        snapshot.playback.seek_request_id = second_id;
        snapshot.playback.position_ms = 120_100;
    });
    app.refresh(&context);
    assert!(app.pending_seek.is_none());
    assert_eq!(app.displayed_position(&app.snapshot.playback), 90_000.0);
    app.seek_draft = None;
    assert_eq!(app.displayed_position(&app.snapshot.playback), 120_100.0);
}

#[test]
fn pending_seek_clears_on_cue_track_changes_errors_and_disconnection() {
    for change in 0..5 {
        let (sender, _receiver) = mpsc::channel(64);
        let (state, snapshots) = watch::channel(populated());
        let mut app = PlayerApp::new(sender, snapshots);
        app.seek_to(60_000);
        app.seek_draft = Some(80_000.0);
        match change {
            0 => state.send_modify(|snapshot| {
                // CUE tracks may share a physical path.
                snapshot.playback.track.as_mut().unwrap().id += 1;
            }),
            1 => state.send_modify(|snapshot| snapshot.playback.generation += 1),
            2 => state.send_modify(|snapshot| snapshot.playback.state = PlaybackState::Error),
            3 => state.send_modify(|snapshot| snapshot.playback.state = PlaybackState::Stopped),
            _ => drop(state),
        }
        app.refresh(&egui::Context::default());
        assert!(app.pending_seek.is_none());
        assert!(app.seek_draft.is_none());
    }
}

#[test]
fn rejected_seek_does_not_leave_an_unconfirmed_preview() {
    let (sender, _receiver) = mpsc::channel(1);
    sender.try_send(AppCommand::Stop).unwrap();
    let (_state, snapshots) = watch::channel(populated());
    let mut app = PlayerApp::new(sender, snapshots);
    for _ in 0..MAX_PENDING {
        assert!(app.outbox.push(AppCommand::Next));
    }
    app.seek_draft = Some(60_000.0);
    app.seek_to(60_000);
    assert!(app.pending_seek.is_none());
    assert!(app.seek_draft.is_none());
    assert!(app.message.as_deref().unwrap().contains("busy"));
}

#[test]
fn output_controls_are_symmetric_and_volume_matches_the_seek_slider() {
    for size in [[950.0, 620.0], [1150.0, 760.0]] {
        for system_theme in [egui::Theme::Light, egui::Theme::Dark] {
            let (sender, mut receiver) = mpsc::channel(64);
            let (_state, snapshots) = watch::channel(populated());
            let mut app = PlayerApp::new(sender, snapshots);
            let context = egui::Context::default();
            let mut initial = context.run_ui(
                egui::RawInput {
                    system_theme: Some(system_theme),
                    ..Default::default()
                },
                |_| {},
            );
            initial.textures_delta.clear();
            frame(&mut app, &context, size, Vec::new());
            frame(&mut app, &context, size, Vec::new());
            let speaker = app.volume_icon.unwrap();
            let output = app.output_button.unwrap();
            let volume = app.volume_slider.unwrap();
            let seek = app.seek_slider.unwrap();
            assert_eq!(speaker.size(), output.size());
            assert!((speaker.left() - seek.left()).abs() < 1.0);
            assert!((output.right() - seek.right()).abs() < 1.0);
            assert!((volume.center().x - seek.center().x).abs() < 1.0);
            assert!((speaker.center().y - output.center().y).abs() < 1.0);
            assert!((volume.center().y - output.center().y).abs() < 1.0);
            assert_eq!(volume.height(), seek.height());
            assert!(
                ((volume.left() - speaker.right()) - (output.left() - volume.right())).abs() < 1.0
            );
            click_at(&mut app, &context, size, volume.center());
            let commands: Vec<_> = std::iter::from_fn(|| receiver.try_recv().ok()).collect();
            assert!(!commands.is_empty());
            assert!(commands.iter().all(|command| matches!(command, AppCommand::SetVolume(volume) if (0.45..0.55).contains(volume))));
        }
    }
}

#[test]
fn clicking_seek_keeps_the_slider_and_time_at_the_requested_position() {
    let size = [1150.0, 760.0];
    let (sender, mut receiver) = mpsc::channel(64);
    let (state, snapshots) = watch::channel(populated());
    let mut app = PlayerApp::new(sender, snapshots);
    let context = egui::Context::default();
    frame(&mut app, &context, size, Vec::new());
    frame(&mut app, &context, size, Vec::new());
    let rail = app.seek_slider.unwrap();
    click_at(&mut app, &context, size, rail.center());
    let (target, request_id) = match receiver.try_recv().unwrap() {
        AppCommand::Seek {
            position_ms,
            request_id,
        } => (position_ms, request_id),
        command => panic!("expected seek: {command:?}"),
    };
    assert!((80_000..105_000).contains(&target));
    state.send_modify(|snapshot| snapshot.playback.position_ms = 1000);
    let output = frame(&mut app, &context, size, Vec::new());
    assert_eq!(
        app.displayed_position(&app.snapshot.playback),
        target as f64
    );
    assert!(
        texts(&output)
            .iter()
            .any(|(text, _)| text == &theme::duration(target))
    );
    state.send_modify(|snapshot| {
        snapshot.playback.seek_request_id = request_id;
        snapshot.playback.position_ms = target + 100;
    });
    frame(&mut app, &context, size, Vec::new());
    assert!(app.pending_seek.is_none());
    assert_eq!(
        app.displayed_position(&app.snapshot.playback),
        (target + 100) as f64
    );
}

#[test]
fn player_controls_stay_at_the_bottom_with_long_metadata_and_errors() {
    for size in [[950.0, 620.0], [1150.0, 760.0]] {
        let (sender, _receiver) = mpsc::channel(64);
        let (state, snapshots) = watch::channel(populated());
        let mut app = PlayerApp::new(sender, snapshots);
        let context = egui::Context::default();
        frame(&mut app, &context, size, Vec::new());
        frame(&mut app, &context, size, Vec::new());
        let initial = app.transport_rect.unwrap();
        let window = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(size[0], size[1]));
        assert!(
            window.contains_rect(initial),
            "controls exceed the window: {initial:?}"
        );
        assert!(
            initial.bottom() > size[1] - 90.0,
            "controls must be bottom anchored: {initial:?}"
        );
        state.send_modify(|snapshot| {
            snapshot.playback.track.as_mut().unwrap().metadata.title =
                "A very long title ".repeat(30);
            snapshot.playback.error = Some("Output unavailable. ".repeat(15));
        });
        frame(&mut app, &context, size, Vec::new());
        frame(&mut app, &context, size, Vec::new());
        assert_eq!(app.transport_rect.unwrap(), initial);
        let button = app.output_button.unwrap().center();
        click_at(&mut app, &context, size, button);
        let output = frame(&mut app, &context, size, Vec::new());
        assert!(
            texts(&output)
                .iter()
                .any(|(text, _)| text == "Working device")
        );
    }
}

#[test]
fn player_mode_and_queue_buttons_are_symmetric_and_transport_commands_still_work() {
    for size in [[950.0, 620.0], [1150.0, 760.0]] {
        let (sender, mut receiver) = mpsc::channel(64);
        let (_state, snapshots) = watch::channel(populated());
        let mut app = PlayerApp::new(sender, snapshots);
        let context = egui::Context::default();
        frame(&mut app, &context, size, Vec::new());
        frame(&mut app, &context, size, Vec::new());
        let mode = app.mode_button.unwrap();
        let queue = app.queue_button.unwrap();
        let [previous, play, next] = app.transport_buttons.map(Option::unwrap);
        assert!(mode.right() < previous.left());
        assert!(next.right() < queue.left());
        assert!(
            (play.center().x - mode.center().x - (queue.center().x - play.center().x)).abs() < 1.0
        );
        assert!((play.center().x - app.seek_slider.unwrap().center().x).abs() < 1.0);
        assert_eq!(mode.size(), queue.size());
        assert!((mode.center().y - queue.center().y).abs() < 1.0);
        click_at(&mut app, &context, size, previous.center());
        assert!(matches!(receiver.try_recv(), Ok(AppCommand::Previous)));
        click_at(&mut app, &context, size, play.center());
        assert!(matches!(receiver.try_recv(), Ok(AppCommand::PauseResume)));
        click_at(&mut app, &context, size, next.center());
        assert!(matches!(receiver.try_recv(), Ok(AppCommand::Next)));
        click_at(&mut app, &context, size, queue.center());
        assert_eq!(app.view, View::Queue);
        assert!(receiver.try_recv().is_err());
    }
}

#[test]
fn play_mode_button_cycles_through_all_modes_and_disables_when_disconnected() {
    let size = [1150.0, 760.0];
    let (sender, mut receiver) = mpsc::channel(64);
    let (state, snapshots) = watch::channel(populated());
    let mut app = PlayerApp::new(sender, snapshots);
    let context = egui::Context::default();
    for expected in [PlayMode::Repeat, PlayMode::Shuffle, PlayMode::Sequential] {
        frame(&mut app, &context, size, Vec::new());
        frame(&mut app, &context, size, Vec::new());
        let position = app.mode_button.unwrap().center();
        click_at(&mut app, &context, size, position);
        assert!(
            matches!(receiver.try_recv(), Ok(AppCommand::SetPlayMode(mode)) if mode == expected)
        );
        state.send_modify(|snapshot| snapshot.play_mode = expected);
    }
    app.outbox.disconnected = true;
    frame(&mut app, &context, size, Vec::new());
    let position = app.mode_button.unwrap().center();
    click_at(&mut app, &context, size, position);
    assert!(receiver.try_recv().is_err());
}

#[test]
fn next_button_at_queue_end_is_enabled_only_for_repeat_and_shuffle() {
    for mode in [PlayMode::Sequential, PlayMode::Repeat, PlayMode::Shuffle] {
        let size = [1150.0, 760.0];
        let (sender, mut receiver) = mpsc::channel(64);
        let mut snapshot = populated();
        snapshot.play_mode = mode;
        snapshot.queue_index = Some(snapshot.queue.len() - 1);
        let (_state, snapshots) = watch::channel(snapshot);
        let mut app = PlayerApp::new(sender, snapshots);
        let context = egui::Context::default();
        frame(&mut app, &context, size, Vec::new());
        frame(&mut app, &context, size, Vec::new());
        let position = app.transport_buttons[2].unwrap().center();
        click_at(&mut app, &context, size, position);
        if mode == PlayMode::Sequential {
            assert!(receiver.try_recv().is_err());
        } else {
            assert!(matches!(receiver.try_recv(), Ok(AppCommand::Next)));
        }
    }
}

#[test]
fn library_duration_and_queue_buttons_align_with_varied_metadata() {
    for size in [[950.0, 620.0], [1150.0, 760.0]] {
        let mut snapshot = populated();
        let mut tracks = vec![track(1), track(2), track(3)];
        for (index, item) in tracks.iter_mut().enumerate() {
            item.metadata.track_number = Some([1, 99, 12345][index]);
            item.metadata.duration_ms = [127_000, 604_000, 3_661_000][index];
        }
        tracks[1].metadata.title = "A long title ".repeat(40);
        tracks[1].metadata.artist = "A long artist ".repeat(40);
        snapshot.library.tracks = Arc::new(tracks);
        snapshot.library.total = 3;
        let (sender, _receiver) = mpsc::channel(64);
        let (_state, snapshots) = watch::channel(snapshot);
        let mut app = PlayerApp::new(sender, snapshots);
        let context = egui::Context::default();
        frame(&mut app, &context, size, Vec::new());
        let output = frame(&mut app, &context, size, Vec::new());
        let labels = texts(&output);
        let buttons = &app.queue_buttons;
        assert_eq!(buttons.len(), 3);
        for rect in buttons {
            assert!((rect.center().x - buttons[0].center().x).abs() < 0.5);
        }
        let inset = app.library_scroll.unwrap().right() - buttons[0].center().x;
        assert!((15.0..30.0).contains(&inset), "queue button inset: {inset}");
        let mut duration_right = None;
        for duration in ["2:07", "10:04", "1:01:01"] {
            let rect = labels.iter().find(|(text, _)| text == duration).unwrap().1;
            let right = *duration_right.get_or_insert(rect.right());
            assert!(
                // Glyph ink bounds vary slightly with the final digit at this size.
                (rect.right() - right).abs() <= 1.0,
                "duration should align right: {rect:?}"
            );
            assert!(rect.right() < buttons[0].left());
        }
        assert!(
            !labels
                .iter()
                .any(|(text, _)| matches!(text.as_str(), "Play" | "···"))
        );
    }
}

#[test]
fn track_double_click_plays_and_plus_only_enqueues() {
    let size = [1150.0, 760.0];
    let (sender, mut receiver) = mpsc::channel(64);
    let (_state, snapshots) = watch::channel(populated());
    let mut app = PlayerApp::new(sender, snapshots);
    let context = egui::Context::default();
    frame(&mut app, &context, size, Vec::new());
    let output = frame(&mut app, &context, size, Vec::new());
    let labels = texts(&output);
    let title = labels
        .iter()
        .find(|(text, _)| text == "Track 1")
        .unwrap()
        .1
        .center();
    let plus = app.queue_buttons[1].center();
    let hover = frame(
        &mut app,
        &context,
        size,
        vec![egui::Event::PointerMoved(title)],
    );
    assert_eq!(hover.platform_output.cursor_icon, egui::CursorIcon::Default);
    click_at(&mut app, &context, size, title);
    assert!(
        receiver.try_recv().is_err(),
        "single click should not start playback"
    );
    click_at(&mut app, &context, size, title);
    assert!(matches!(receiver.try_recv(), Ok(AppCommand::Play(track)) if track.id == 1));
    assert!(receiver.try_recv().is_err());
    let copy = frame(&mut app, &context, size, vec![egui::Event::Copy]);
    assert!(
        !copy
            .platform_output
            .commands
            .iter()
            .any(|command| { matches!(command, egui::OutputCommand::CopyText(_)) }),
        "double-clicking a row must not select text"
    );
    for _ in 0..2 {
        click_at(&mut app, &context, size, plus);
        assert!(matches!(receiver.try_recv(), Ok(AppCommand::Enqueue(track)) if track.id == 1));
        assert!(
            receiver.try_recv().is_err(),
            "double-clicking + must not also play"
        );
    }
}

#[test]
fn missing_tracks_cannot_play_or_enqueue_from_the_row() {
    let size = [1150.0, 760.0];
    let mut snapshot = populated();
    let mut item = track(10);
    item.missing = true;
    snapshot.library.tracks = Arc::new(vec![item]);
    snapshot.library.total = 1;
    let (sender, mut receiver) = mpsc::channel(64);
    let (_state, snapshots) = watch::channel(snapshot);
    let mut app = PlayerApp::new(sender, snapshots);
    let context = egui::Context::default();
    frame(&mut app, &context, size, Vec::new());
    let output = frame(&mut app, &context, size, Vec::new());
    let labels = texts(&output);
    let title = labels
        .iter()
        .find(|(text, _)| text == "Track 10")
        .unwrap()
        .1
        .center();
    for position in [title, app.queue_buttons[0].center()] {
        click_at(&mut app, &context, size, position);
        click_at(&mut app, &context, size, position);
        assert!(receiver.try_recv().is_err());
    }
}

#[test]
fn queue_double_click_and_context_menu_use_the_selected_duplicate_entry() {
    let size = [1150.0, 760.0];
    let mut snapshot = populated();
    snapshot.queue = Arc::new(vec![track(4), track(4)]);
    let (sender, mut receiver) = mpsc::channel(64);
    let (_state, snapshots) = watch::channel(snapshot);
    let mut app = PlayerApp::new(sender, snapshots);
    app.view = View::Queue;
    let context = egui::Context::default();
    frame(&mut app, &context, size, Vec::new());
    let output = frame(&mut app, &context, size, Vec::new());
    let position = texts(&output)
        .into_iter()
        .filter(|(text, _)| text == "Track 4")
        .nth(1)
        .unwrap()
        .1
        .center();
    click_at(&mut app, &context, size, position);
    click_at(&mut app, &context, size, position);
    assert!(matches!(receiver.try_recv(), Ok(AppCommand::PlayQueue(1))));
    for pressed in [true, false] {
        frame(
            &mut app,
            &context,
            size,
            vec![
                egui::Event::PointerMoved(position),
                egui::Event::PointerButton {
                    pos: position,
                    button: egui::PointerButton::Secondary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
    }
    click(&mut app, &context, "Remove from queue");
    assert!(matches!(
        receiver.try_recv(),
        Ok(AppCommand::RemoveQueue(1))
    ));
    assert!(receiver.try_recv().is_err());
}

#[test]
fn queue_minus_removes_the_selected_entry_even_when_its_file_is_missing() {
    for missing in [false, true] {
        let mut snapshot = populated();
        let mut entry = track(4);
        entry.missing = missing;
        snapshot.queue = Arc::new(vec![entry.clone(), entry]);
        let (sender, mut receiver) = mpsc::channel(64);
        let (_state, snapshots) = watch::channel(snapshot);
        let mut app = PlayerApp::new(sender, snapshots);
        app.view = View::Queue;
        let context = egui::Context::default();
        frame(&mut app, &context, [1150.0, 760.0], Vec::new());
        let output = frame(&mut app, &context, [1150.0, 760.0], Vec::new());
        assert!(!texts(&output).iter().any(|(text, _)| text == "02"));
        let minus = app.queue_buttons[1].center();
        click_at(&mut app, &context, [1150.0, 760.0], minus);
        assert!(matches!(
            receiver.try_recv(),
            Ok(AppCommand::RemoveQueue(1))
        ));
        assert!(receiver.try_recv().is_err());
    }
}

struct LibraryArtworkProbe {
    artwork: mplayer_core::Artwork,
    calls: std::sync::Mutex<Vec<PathBuf>>,
    held: std::sync::atomic::AtomicBool,
    active: std::sync::atomic::AtomicUsize,
    maximum: std::sync::atomic::AtomicUsize,
}

impl LibraryArtworkProbe {
    fn new() -> Arc<Self> {
        use image::ImageEncoder;
        let mut encoded = Vec::new();
        image::codecs::png::PngEncoder::new(&mut encoded)
            .write_image(
                &[40, 130, 200, 255].repeat(512 * 256),
                512,
                256,
                image::ExtendedColorType::Rgba8,
            )
            .unwrap();
        Arc::new(Self {
            artwork: mplayer_core::Artwork {
                data: encoded.into(),
                mime_type: "image/png".into(),
            },
            calls: Default::default(),
            held: Default::default(),
            active: Default::default(),
            maximum: Default::default(),
        })
    }
}

impl mplayer_core::MetadataProbe for LibraryArtworkProbe {
    fn probe(&self, _: &std::path::Path) -> mplayer_core::Result<TrackMetadata> {
        Ok(TrackMetadata::default())
    }

    fn artwork(
        &self,
        path: &std::path::Path,
    ) -> mplayer_core::Result<Option<mplayer_core::Artwork>> {
        use std::sync::atomic::Ordering::Relaxed;
        let active = self.active.fetch_add(1, Relaxed) + 1;
        self.maximum.fetch_max(active, Relaxed);
        self.calls.lock().unwrap().push(path.to_owned());
        let started = std::time::Instant::now();
        while self.held.load(Relaxed) && started.elapsed() < Duration::from_secs(3) {
            std::thread::sleep(Duration::from_millis(1));
        }
        self.active.fetch_sub(1, Relaxed);
        if path.file_stem().is_some_and(|stem| stem == "absent") {
            return Ok(None);
        }
        if path.file_stem().is_some_and(|stem| stem == "broken") {
            return Ok(Some(mplayer_core::Artwork {
                data: Arc::from(&b"invalid image"[..]),
                mime_type: "image/png".into(),
            }));
        }
        Ok(Some(self.artwork.clone()))
    }
}

async fn thumbnail(
    cache: &mut thumbnails::Thumbnails,
    path: &std::path::Path,
    context: &egui::Context,
) -> egui::TextureHandle {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            cache.begin_frame(context);
            let texture = cache.texture(path).cloned();
            cache.finish_frame(context);
            if let Some(texture) = texture {
                return texture;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn library_thumbnails_bound_workers_skip_old_requests_and_reuse_results() {
    use std::sync::atomic::Ordering::Relaxed;
    let probe = LibraryArtworkProbe::new();
    probe.held.store(true, Relaxed);
    let mut cache = thumbnails::Thumbnails::default();
    cache.probe = Some(probe.clone());
    let context = egui::Context::default();
    cache.begin_frame(&context);
    for id in 0..40 {
        let path = PathBuf::from(format!("/music/{id}.flac"));
        cache.texture(&path);
        cache.texture(&path);
    }
    cache.finish_frame(&context);
    assert_eq!(cache.pending_count(), 2);
    tokio::time::timeout(Duration::from_secs(2), async {
        while probe.active.load(Relaxed) < 2 {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    cache.begin_frame(&context);
    let latest = std::path::Path::new("/new/track.flac");
    cache.texture(latest);
    cache.finish_frame(&context);
    assert_eq!(cache.pending_count(), 2);
    probe.held.store(false, Relaxed);
    let texture = thumbnail(&mut cache, latest, &context).await;
    assert_eq!(texture.size(), [256, 128]);
    for _ in 0..10 {
        assert_eq!(
            thumbnail(&mut cache, latest, &context).await.id(),
            texture.id()
        );
    }
    assert_eq!(probe.maximum.load(Relaxed), 2);
    let calls = probe.calls.lock().unwrap();
    assert_eq!(calls.len(), 3);
    assert_eq!(
        calls.iter().filter(|path| path.as_path() == latest).count(),
        1
    );
}

#[tokio::test]
async fn thumbnail_failures_are_cached_and_rescans_discard_stale_results() {
    use std::sync::atomic::Ordering::Relaxed;
    let probe = LibraryArtworkProbe::new();
    let mut cache = thumbnails::Thumbnails::default();
    cache.probe = Some(probe.clone());
    let context = egui::Context::default();
    let absent = std::path::Path::new("/music/absent.flac");
    let broken = std::path::Path::new("/music/broken.flac");
    cache.begin_frame(&context);
    cache.texture(absent);
    cache.texture(broken);
    cache.finish_frame(&context);
    tokio::time::timeout(Duration::from_secs(3), async {
        while cache.pending_count() > 0 {
            tokio::time::sleep(Duration::from_millis(2)).await;
            cache.begin_frame(&context);
        }
    })
    .await
    .unwrap();
    for _ in 0..10 {
        cache.begin_frame(&context);
        assert!(cache.texture(absent).is_none());
        assert!(cache.texture(broken).is_none());
        cache.finish_frame(&context);
        assert_eq!(cache.pending_count(), 0);
    }
    assert_eq!(probe.calls.lock().unwrap().len(), 2);

    let path = std::path::Path::new("/music/changed.flac");
    probe.held.store(true, Relaxed);
    cache.begin_frame(&context);
    cache.texture(path);
    cache.finish_frame(&context);
    tokio::time::timeout(Duration::from_secs(2), async {
        while probe.active.load(Relaxed) == 0 {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    cache.invalidate();
    probe.held.store(false, Relaxed);
    let _ = thumbnail(&mut cache, path, &context).await;
    assert_eq!(
        probe
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|item| item.as_path() == path)
            .count(),
        2
    );
}

#[tokio::test]
async fn all_tracks_render_square_thumbnails_and_album_cards_put_names_below_covers() {
    for size in [[950.0, 620.0], [1150.0, 760.0]] {
        let probe = LibraryArtworkProbe::new();
        let (sender, _receiver) = mpsc::channel(64);
        let (_state, snapshots) = watch::channel(populated());
        let mut app = PlayerApp::new(sender, snapshots).with_artwork_probe(probe.clone());
        let context = egui::Context::default();
        let path = track(0).path;
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                frame(&mut app, &context, size, Vec::new());
                if app.thumbnails.texture(&path).is_some() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .unwrap();
        let texture = app.thumbnails.texture(&path).unwrap().id();
        let output = frame(&mut app, &context, size, Vec::new());
        assert!(!texts(&output).iter().any(|(text, _)| text == "01"));
        let primitives = context.tessellate(output.shapes, output.pixels_per_point);
        let image = primitives
            .iter()
            .find_map(|primitive| match &primitive.primitive {
                egui::epaint::Primitive::Mesh(mesh) if mesh.texture_id == texture => {
                    Some(mesh.calc_bounds())
                }
                _ => None,
            })
            .expect("thumbnail must reach the rendered track row");
        // Rounded images include a half-pixel antialiasing fringe on each side.
        assert!(
            (image.width() - 40.0).abs() <= 1.5,
            "track cover bounds: {image:?}"
        );
        assert!((image.width() - image.height()).abs() < 1.0);

        app.view = View::Queue;
        let output = frame(&mut app, &context, size, Vec::new());
        assert!(!texts(&output).iter().any(|(text, _)| text == "01"));
        let primitives = context.tessellate(output.shapes, output.pixels_per_point);
        let queue_image = primitives
            .iter()
            .find_map(|primitive| match &primitive.primitive {
                egui::epaint::Primitive::Mesh(mesh) if mesh.texture_id == texture => {
                    Some(mesh.calc_bounds())
                }
                _ => None,
            })
            .expect("queue must render the same embedded artwork as All Tracks");
        assert!((queue_image.width() - 40.0).abs() <= 1.5);
        assert!((queue_image.width() - queue_image.height()).abs() < 1.0);

        app.view = View::Library;
        app.tab = LibraryTab::Albums;
        frame(&mut app, &context, size, Vec::new());
        let output = frame(&mut app, &context, size, Vec::new());
        let labels = texts(&output);
        let primitives = context.tessellate(output.shapes, output.pixels_per_point);
        let image = primitives
            .iter()
            .find_map(|primitive| match &primitive.primitive {
                egui::epaint::Primitive::Mesh(mesh) if mesh.texture_id == texture => {
                    Some(mesh.calc_bounds())
                }
                _ => None,
            })
            .expect("album card must reuse the embedded cover");
        assert!(image.width() > 120.0);
        assert!((image.width() - image.height()).abs() < 1.0);
        let title = labels
            .iter()
            .find(|(text, rect)| text == "An album" && rect.right() < size[0] - 318.0)
            .unwrap()
            .1;
        let artist = labels
            .iter()
            .find(|(text, rect)| text == "An artist" && rect.right() < size[0] - 318.0)
            .unwrap()
            .1;
        assert!(title.top() > image.bottom());
        assert!(artist.top() > title.bottom());
        assert_eq!(
            probe
                .calls
                .lock()
                .unwrap()
                .iter()
                .filter(|item| **item == path)
                .count(),
            1
        );
    }
}

#[path = "library_folders.rs"]
mod folder_tests;

#[path = "playlists.rs"]
mod playlist_tests;

#[path = "list_rows.rs"]
mod list_row_tests;

#[path = "albums.rs"]
mod album_tests;

#[path = "queue.rs"]
mod queue_tests;

#[path = "player_layout.rs"]
mod player_layout_tests;
