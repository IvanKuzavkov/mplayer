use super::*;

#[tokio::test]
async fn clearing_queue_resets_player_immediately_and_ignores_late_updates() {
    let mut h = Harness::new().await;
    h.app.state.queue = Arc::new(vec![track(1), track(2)]);
    h.app.play_index(0).unwrap();
    let old_generation = h.app.generation;
    h.audio.try_iter().for_each(drop);
    h.app.requested_volume = 0.25;
    let previous = PlaybackSnapshot {
        generation: old_generation,
        state: PlaybackState::Playing,
        track: Some(track(1)),
        position_ms: 500,
        duration_ms: 10_000,
        source_rate: 44_100,
        output_rate: 48_000,
        device: "Selected DAC".into(),
        selected_device: Some("Selected DAC".into()),
        devices: vec!["Selected DAC".into()],
        volume: 0.25,
        notification: Some("Previous notice".into()),
        ..Default::default()
    };
    h.app.state.playback = previous.clone();
    let path = track(1).path;
    h.app.artwork_track = Some(path.clone());
    h.app.artwork_reading = Some(path.clone());
    h.app.state.artwork = Some(Artwork {
        data: b"previous cover".as_slice().into(),
        mime_type: "test/data".into(),
    });
    let (sender, result) = tokio::sync::oneshot::channel();
    h.app.artwork_task = Some(tokio::spawn(async { result.await.unwrap() }));

    h.app.handle(AppCommand::ClearQueue).await.unwrap();
    assert!(h.app.state.queue.is_empty());
    assert!(h.app.state.queue_index.is_none());
    let playback = &h.app.state.playback;
    assert_eq!(playback.state, PlaybackState::Stopped);
    assert!(playback.track.is_none());
    assert_eq!(playback.position_ms, 0);
    assert_eq!(playback.duration_ms, 0);
    assert_eq!(playback.source_rate, 0);
    assert_eq!(playback.output_rate, 0);
    assert!(playback.device.is_empty());
    assert!(playback.notification.is_none());
    assert_eq!(playback.volume, previous.volume);
    assert_eq!(playback.selected_device, previous.selected_device);
    assert_eq!(playback.devices, previous.devices);
    assert!(h.app.state.artwork.is_none());
    assert!(h.app.artwork_track.is_none());
    assert_eq!(h.app.state.status, "Ready");
    assert!(
        matches!(h.audio.try_recv().unwrap(), AudioCommand::Stop { generation } if generation == h.app.generation && generation != old_generation)
    );

    h.playback.send_replace(previous);
    h.app.update_playback().unwrap();
    sender
        .send((
            path,
            Ok(Some(Artwork {
                data: b"late cover".as_slice().into(),
                mime_type: "test/data".into(),
            })),
        ))
        .unwrap();
    h.app.finish_artwork().await;
    assert!(h.app.state.playback.track.is_none());
    assert_eq!(h.app.state.playback.state, PlaybackState::Stopped);
    assert!(h.app.state.artwork.is_none());
    assert!(h.app.state.queue.is_empty());
    assert!(h.audio.try_recv().is_err());
    h.app.store.close().await;
}
