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
        h.app.store.clone(),
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
    let ordered = h.app.store.album_tracks(&key).await.unwrap();
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
    h.app
        .handle(AppCommand::EnqueueAlbum(key.clone()))
        .await
        .unwrap();
    assert_eq!(h.app.generation, generation);
    assert_eq!(h.app.state.queue_index, Some(0));
    assert_eq!(h.app.state.queue[0].id, 999);
    assert_eq!(&h.app.state.queue[1..], ordered);
    assert!(
        matches!(h.audio.try_recv().unwrap(), AudioCommand::SetNext(Some(track)) if track == ordered[0])
    );
    assert!(h.audio.try_recv().is_err());
    h.app
        .handle(AppCommand::PlayAlbum(key.clone()))
        .await
        .unwrap();
    assert_eq!(h.app.state.queue.as_slice(), ordered);
    assert_eq!(h.app.state.queue_index, Some(0));
    assert_eq!(h.app.generation, generation + 1);
    assert!(
        matches!(h.audio.try_recv().unwrap(), AudioCommand::Load { track, next: Some(next), .. } if track == ordered[0] && next == ordered[1])
    );
    let missing = AlbumKey {
        title: "Deleted album".into(),
        ..key
    };
    for command in [
        AppCommand::PlayAlbum(missing.clone()),
        AppCommand::EnqueueAlbum(missing),
    ] {
        assert!(h.app.handle(command).await.is_err());
        assert_eq!(h.app.state.queue.as_slice(), ordered);
        assert_eq!(h.app.generation, generation + 1);
        assert!(h.audio.try_recv().is_err());
    }
    h.app.store.close().await;
}
