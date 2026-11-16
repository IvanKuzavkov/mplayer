use super::*;

#[test]
fn player_metadata_fits_above_controls_at_minimum_height() {
    for size in [[950.0, 620.0], [1150.0, 760.0]] {
        let mut snapshot = populated();
        let current = snapshot.playback.track.as_mut().unwrap();
        current.metadata.title = "A long title that wraps onto multiple lines ".repeat(5);
        current.metadata.artist = "An artist with a very long name ".repeat(5);
        current.metadata.album = "An album with a very long name ".repeat(5);
        current.metadata.codec = "flac".into();
        current.metadata.bit_depth = Some(24);
        let expected = [
            current.metadata.title.clone(),
            current.metadata.artist.clone(),
            current.metadata.album.clone(),
            "FLAC · 44.1 kHz · 24 bit".into(),
        ];
        let (sender, _receiver) = command_channel(64);
        let (state, snapshots) = snapshot_channel(snapshot);
        let mut app = PlayerApp::new(sender, snapshots);
        let context = egui::Context::default();
        let mut controls = None;
        for notification in [None, Some("Audio output device changed. Playback paused.")] {
            state
                .send_modify(|state| state.playback.notification = notification.map(str::to_owned));
            frame(&mut app, &context, size, Vec::new());
            let output = frame(&mut app, &context, size, Vec::new());
            let mut previous_bottom = 0.0;
            for (index, expected) in expected.iter().enumerate() {
                let shape = output
                    .shapes
                    .iter()
                    .find(|shape| {
                        matches!(&shape.shape,
                            egui::Shape::Text(text) if &text.galley.job.text == expected
                                && text.pos.x > app.volume_icon.unwrap().left()
                        )
                    })
                    .expect("player metadata is rendered");
                let egui::Shape::Text(text) = &shape.shape else {
                    unreachable!()
                };
                let rect = text.visual_bounding_rect();
                assert!(
                    rect.top() >= shape.clip_rect.top(),
                    "metadata clipped at top: {expected}"
                );
                assert!(
                    rect.bottom() <= shape.clip_rect.bottom(),
                    "metadata clipped at bottom: {expected}"
                );
                assert!(rect.top() >= previous_bottom);
                assert!(rect.bottom() < app.seek_slider.unwrap().top());
                assert!(text.galley.rows.len() <= if index == 0 { 2 } else { 1 });
                previous_bottom = rect.bottom();
            }
            assert_eq!(
                *controls.get_or_insert(app.transport_rect.unwrap()),
                app.transport_rect.unwrap()
            );
        }
    }
}
