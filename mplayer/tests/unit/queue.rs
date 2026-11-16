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
    h.app.handle(PlaybackCommand::ClearQueue).unwrap();
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
    assert_eq!(h.app.state.status, "Ready");
    assert!(
        matches!(h.audio.try_recv().unwrap(), AudioCommand::Stop { generation } if generation == h.app.generation && generation != old_generation)
    );

    h.playback.send_replace(previous);
    h.app.update_playback().unwrap();
    assert!(h.app.state.playback.track.is_none());
    assert_eq!(h.app.state.playback.state, PlaybackState::Stopped);
    assert!(h.app.state.queue.is_empty());
    assert!(h.audio.try_recv().is_err());
    h.store.close().await;
}
