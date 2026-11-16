use super::*;

struct AlbumProbe;
impl MetadataProbe for AlbumProbe {
    fn probe(&self, path: &Path) -> Result<TrackMetadata> {
        let number: u32 = path.file_stem().unwrap().to_str().unwrap().parse()?;
        Ok(TrackMetadata {
            title: format!("Track {number}"),
            album: "Album".into(),
            artist: "Artist".into(),
            disc_number: Some(number / 100),
            track_number: Some(number % 100),
            ..Default::default()
        })
    }
}

#[tokio::test]
async fn enqueue_album_preserves_current_queue_and_play_album_replaces_it() {
    let mut h = Harness::new().await;
    let root = h._directory.0.join("album");
    std::fs::create_dir_all(&root).unwrap();
    for number in [201, 102, 101] {
        std::fs::write(root.join(format!("{number}.flac")), b"fixture").unwrap();
    }
    let (progress, _) = watch::channel(ScanProgress::default());
    let (_cancel, scan) = scan_task(
        h.store.clone(),
        ScanRequest::Roots(vec![root]),
        Arc::new(AlbumProbe),
        progress,
    )
    .await
    .unwrap();
    scan.await.unwrap().unwrap();
    let key = AlbumKey {
        title: "Album".into(),
        artist: "Artist".into(),
    };
    let ordered = h.store.album_tracks(&key).await.unwrap();
    assert_eq!(
        ordered
            .iter()
            .map(|track| track.metadata.title.as_str())
            .collect::<Vec<_>>(),
        ["Track 101", "Track 102", "Track 201"]
    );
    h.app.state.queue = Arc::new(vec![track(999)]);
    h.app.play_index(0).unwrap();
    let generation = h.app.generation;
    h.audio.try_iter().for_each(drop);
    let player = tokio::spawn(h.app.run(h.shutdown.clone()));
    let library = tokio::spawn(h.library.run(h.shutdown.clone()));
    wait_for(&mut h.player_snapshots, |state| state.queue.len() == 1).await;
    h.audio.try_iter().for_each(drop);
    h.commands
        .send(PlaybackCommand::EnqueueAlbum(key.clone()).into())
        .await
        .unwrap();
    wait_for(&mut h.player_snapshots, |state| state.queue.len() == 4).await;
    {
        let state = h.player_snapshots.borrow();
        assert_eq!(state.queue_index, Some(0));
        assert_eq!(state.queue[0].id, 999);
        assert_eq!(&state.queue[1..], ordered);
    }
    assert!(
        matches!(h.audio.try_recv().unwrap(), AudioCommand::SetNext(Some(track)) if track == ordered[0])
    );
    h.commands
        .send(PlaybackCommand::PlayAlbum(key.clone()).into())
        .await
        .unwrap();
    wait_for(&mut h.player_snapshots, |state| state.queue.len() == 3).await;
    assert_eq!(h.player_snapshots.borrow().queue.as_slice(), ordered);
    assert!(
        matches!(h.audio.try_recv().unwrap(), AudioCommand::Load { track, next: Some(next), generation: next_generation } if track == ordered[0] && next == ordered[1] && next_generation == generation + 1)
    );
    let missing = AlbumKey {
        title: "Deleted album".into(),
        ..key
    };
    for command in [
        PlaybackCommand::PlayAlbum(missing.clone()),
        PlaybackCommand::EnqueueAlbum(missing),
    ] {
        h.player_snapshots.borrow_and_update();
        h.commands.send(command.into()).await.unwrap();
        wait_for(&mut h.player_snapshots, |state| {
            state.playback.error.is_some()
        })
        .await;
        assert_eq!(h.player_snapshots.borrow().queue.as_slice(), ordered);
        assert!(h.audio.try_recv().is_err());
    }
    h.commands.0.shutdown();
    player.await.unwrap().unwrap();
    library.await.unwrap().unwrap();
    h.store.close().await;
}
