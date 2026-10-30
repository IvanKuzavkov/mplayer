use super::*;

async fn request(
    store: &Store,
    target: ScanRequest,
    probe: Arc<dyn MetadataProbe>,
) -> ScanProgress {
    let (sender, receiver) = watch::channel(ScanProgress::default());
    let (_cancel, task) = scan_task(store.clone(), target, probe, sender)
        .await
        .unwrap();
    task.await.unwrap().unwrap();
    receiver.borrow().clone()
}

#[tokio::test]
async fn targeted_scans_discover_dsd_extensions() {
    let temp = TempDirectory::new();
    let root = temp.music();
    tokio::fs::write(root.join("initial.flac"), b"valid")
        .await
        .unwrap();
    let store = Store::open(&temp.0.join("library.sqlite")).await.unwrap();
    let probe = Arc::new(Probe::default());
    run_scan(&store, &root, &probe).await;
    let folder = store.folders("", 0).await.unwrap().folders[0].id;
    for name in ["one.DsF", "two.DFF"] {
        tokio::fs::write(root.join(name), b"valid").await.unwrap();
    }
    let progress = request(&store, ScanRequest::Folder(folder), probe.clone()).await;
    assert_eq!(progress.errors, 0);
    assert_eq!(store.library("", 0).await.unwrap().total, 3);
    assert_eq!(probe.0.load(Ordering::Relaxed), 3);
    store.close().await;
}

#[tokio::test]
async fn folder_failure_is_isolated_and_targeted_retry_reuses_generation() {
    let temp = TempDirectory::new();
    let root = temp.music();
    let a = root.join("a");
    let b = root.join("b");
    for dir in [&a, &b] {
        tokio::fs::create_dir_all(dir).await.unwrap();
    }
    tokio::fs::write(a.join("one.flac"), b"valid")
        .await
        .unwrap();
    tokio::fs::write(a.join("two.flac"), b"valid")
        .await
        .unwrap();
    tokio::fs::write(b.join("one.flac"), b"valid")
        .await
        .unwrap();
    let store = Store::open(&temp.0.join("library.sqlite")).await.unwrap();
    let probe = Arc::new(Probe::default());
    run_scan(&store, &root, &probe).await;
    let original = store.library("", 0).await.unwrap();
    tokio::fs::remove_file(a.join("two.flac")).await.unwrap();
    tokio::fs::write(a.join("one.flac"), b"bad").await.unwrap();
    tokio::fs::write(b.join("new.flac"), b"valid")
        .await
        .unwrap();
    assert_eq!(run_scan(&store, &root, &probe).await.errors, 1);
    let folders = store.folders("", 0).await.unwrap();
    let failed = folders
        .folders
        .iter()
        .find(|f| f.path.ends_with("a"))
        .unwrap();
    let successful = folders
        .folders
        .iter()
        .find(|f| f.path.ends_with("b"))
        .unwrap();
    assert_eq!((failed.generation, failed.status), (1, ScanStatus::Error));
    assert_eq!(
        (successful.generation, successful.status),
        (2, ScanStatus::Ready)
    );
    assert_eq!(store.library("", 0).await.unwrap().total, 4);
    assert!(
        store
            .library("", 0)
            .await
            .unwrap()
            .tracks
            .iter()
            .all(|track| !track.missing)
    );
    tokio::fs::write(a.join("one.flac"), b"fixed audio")
        .await
        .unwrap();
    let calls = probe.0.load(Ordering::Relaxed);
    let retried = request(&store, ScanRequest::Folder(failed.id), probe.clone()).await;
    assert_eq!(retried.errors, 0);
    assert_eq!(probe.0.load(Ordering::Relaxed) - calls, 1);
    let updated = store.folders("", 0).await.unwrap();
    assert!(
        updated
            .folders
            .iter()
            .all(|f| f.generation == 2 && f.status == ScanStatus::Ready)
    );
    let tracks = store.library("", 0).await.unwrap();
    let deleted = tracks
        .tracks
        .iter()
        .find(|track| track.path.ends_with("two.flac"))
        .unwrap();
    assert!(deleted.missing);
    assert!(original.tracks.iter().any(|track| track.id == deleted.id));
    store.close().await;
}

#[tokio::test]
async fn removed_folders_and_their_tracks_are_deleted_on_full_and_targeted_scans() {
    for targeted in [false, true] {
        let temp = TempDirectory::new();
        let root = tokio::fs::canonicalize(temp.music()).await.unwrap();
        let album = root.join("album");
        let healthy = root.join("healthy");
        for path in [&album, &healthy] {
            tokio::fs::create_dir_all(path).await.unwrap();
            tokio::fs::write(path.join("one.flac"), b"valid")
                .await
                .unwrap();
        }
        let store = Store::open(&temp.0.join("library.sqlite")).await.unwrap();
        let probe = Arc::new(Probe::default());
        run_scan(&store, &root, &probe).await;
        let folder = store.folders("album", 0).await.unwrap().folders[0].clone();
        store.create_playlist("Keep").await.unwrap();
        let playlist = store.playlists().await.unwrap()[0].id;
        for track in store.library("", 0).await.unwrap().tracks.iter() {
            store.add_playlist_entry(playlist, track.id).await.unwrap();
        }
        tokio::fs::remove_dir_all(&album).await.unwrap();
        let progress = request(
            &store,
            if targeted {
                ScanRequest::Folder(folder.id)
            } else {
                ScanRequest::Library
            },
            probe.clone(),
        )
        .await;
        assert_eq!(progress.errors, 0);
        assert!(progress.folder_revision > 0);
        assert_eq!(store.folders("album", 0).await.unwrap().total, 0);
        assert!(store.folder(folder.id).await.is_err());
        assert!(store.folder_snapshot(folder.id).await.unwrap().is_empty());
        let tracks = store.library("", 0).await.unwrap();
        assert_eq!(tracks.total, 1);
        assert!(tracks.tracks[0].path.starts_with(&healthy));
        let entries = store.playlist_entries(playlist).await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].track.id, tracks.tracks[0].id);
        assert_eq!(store.roots().await.unwrap(), vec![root.clone()]);
        tokio::fs::create_dir_all(&album).await.unwrap();
        tokio::fs::write(album.join("one.flac"), b"valid")
            .await
            .unwrap();
        assert_eq!(request(&store, ScanRequest::Library, probe).await.errors, 0);
        assert_eq!(store.library("", 0).await.unwrap().total, 2);
        assert_eq!(store.playlist_entries(playlist).await.unwrap().len(), 1);
        store.close().await;
    }
}

#[tokio::test]
async fn removed_empty_folder_is_deleted_on_targeted_scan() {
    let temp = TempDirectory::new();
    let root = temp.music();
    let album = root.join("album");
    tokio::fs::create_dir_all(&album).await.unwrap();
    let store = Store::open(&temp.0.join("library.sqlite")).await.unwrap();
    let root_record = store.register_root(&root).await.unwrap();
    let folder = store.ensure_folder(&root_record, &album).await.unwrap();
    tokio::fs::remove_dir(&album).await.unwrap();
    assert_eq!(
        request(
            &store,
            ScanRequest::Folder(folder.id),
            Arc::new(Probe::default())
        )
        .await
        .errors,
        0
    );
    assert_eq!(store.folders("", 0).await.unwrap().total, 0);
    store.close().await;
}

#[tokio::test]
async fn not_found_inside_an_existing_folder_preserves_its_catalog() {
    let temp = TempDirectory::new();
    let root = temp.music();
    tokio::fs::write(root.join("one.flac"), b"valid")
        .await
        .unwrap();
    let store = Store::open(&temp.0.join("library.sqlite")).await.unwrap();
    run_scan(&store, &root, &Arc::new(Probe::default())).await;
    let folder = store.folders("", 0).await.unwrap().folders[0].id;
    let original = store.library("", 0).await.unwrap().tracks;
    let (progress, _) = watch::channel(ScanProgress::default());
    let mut scanner = Scanner {
        store: store.clone(),
        probe: Arc::new(Probe::default()),
        progress,
        state: ScanProgress::default(),
    };
    scanner
        .folder_read_error(
            &store.folder(folder).await.unwrap(),
            std::io::Error::from(std::io::ErrorKind::NotFound).into(),
        )
        .await
        .unwrap();
    assert_eq!(scanner.state.errors, 1);
    assert_eq!(store.library("", 0).await.unwrap().tracks, original);
    assert_eq!(
        store.folders("", 0).await.unwrap().folders[0].status,
        ScanStatus::Error
    );
    store.close().await;
}

#[tokio::test]
async fn offline_roots_preserve_folders_tracks_and_playlists() {
    let temp = TempDirectory::new();
    let root = temp.music();
    let album = root.join("album");
    tokio::fs::create_dir_all(&album).await.unwrap();
    tokio::fs::write(album.join("one.flac"), b"valid")
        .await
        .unwrap();
    let store = Store::open(&temp.0.join("library.sqlite")).await.unwrap();
    let probe = Arc::new(Probe::default());
    run_scan(&store, &root, &probe).await;
    let original = store.library("", 0).await.unwrap().tracks[0].clone();
    store.create_playlist("Keep").await.unwrap();
    let playlist = store.playlists().await.unwrap()[0].id;
    store
        .add_playlist_entry(playlist, original.id)
        .await
        .unwrap();
    let folder = store.folders("", 0).await.unwrap().folders[0].id;
    let offline = temp.0.join("offline");
    tokio::fs::rename(&root, &offline).await.unwrap();
    assert!(
        request(&store, ScanRequest::Library, probe.clone())
            .await
            .errors
            > 0
    );
    assert!(
        request(&store, ScanRequest::Folder(folder), probe)
            .await
            .errors
            > 0
    );
    assert_eq!(
        store.library("", 0).await.unwrap().tracks[0].id,
        original.id
    );
    assert!(
        !store.playlist_entries(playlist).await.unwrap()[0]
            .track
            .missing
    );
    assert_eq!(
        store.folders("", 0).await.unwrap().folders[0].status,
        ScanStatus::Error
    );
    store.close().await;
}

#[tokio::test]
async fn importing_files_scans_siblings_and_global_roots_absorb_descendants() {
    let temp = TempDirectory::new();
    let root = temp.music();
    let album = root.join("album");
    tokio::fs::create_dir_all(&album).await.unwrap();
    for name in ["one.flac", "two.flac"] {
        tokio::fs::write(album.join(name), b"valid").await.unwrap();
    }
    let nested = album.join("nested");
    tokio::fs::create_dir_all(&nested).await.unwrap();
    tokio::fs::write(nested.join("nested.flac"), b"valid")
        .await
        .unwrap();
    let store = Store::open(&temp.0.join("library.sqlite")).await.unwrap();
    let probe = Arc::new(Probe::default());
    request(
        &store,
        ScanRequest::Roots(vec![album.join("one.flac")]),
        probe.clone(),
    )
    .await;
    assert_eq!(store.library("", 0).await.unwrap().total, 2);
    let ids: Vec<_> = store
        .library("", 0)
        .await
        .unwrap()
        .tracks
        .iter()
        .map(|track| track.id)
        .collect();
    run_scan(&store, &root, &probe).await;
    run_scan(&store, &album, &probe).await;
    assert_eq!(
        store.roots().await.unwrap(),
        vec![tokio::fs::canonicalize(&root).await.unwrap()]
    );
    let tracks = store.library("", 0).await.unwrap();
    assert_eq!(tracks.total, 3);
    assert!(
        ids.iter()
            .all(|id| tracks.tracks.iter().any(|track| track.id == *id))
    );
    assert_eq!(store.folders("", 0).await.unwrap().total, 2);
    store.close().await;
}

struct CueProbe(AtomicUsize);
impl MetadataProbe for CueProbe {
    fn probe(&self, _: &Path) -> Result<TrackMetadata> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Ok(TrackMetadata {
            title: "Embedded".into(),
            artist: "Embedded artist".into(),
            album: "Embedded album".into(),
            sample_rate: 44100,
            channels: 2,
            duration_ms: 10_000,
            ..Default::default()
        })
    }
}
const CUE: &str = "PERFORMER \"Album artist\"\nTITLE \"Album title\"\nFILE \"image.flac\" WAVE\n TRACK 01 AUDIO\n  TITLE \"First\"\n  INDEX 01 00:00:00\n TRACK 02 AUDIO\n  PERFORMER \"Guest\"\n  TITLE \"Second\"\n  INDEX 00 00:00:74\n  INDEX 01 00:01:01\n";

#[tokio::test]
async fn cue_frame_carry_commits_correct_track_bounds_without_changing_the_sidecar() {
    let temp = TempDirectory::new();
    let root = temp.music();
    tokio::fs::write(root.join("image.flac"), b"audio")
        .await
        .unwrap();
    let contents = CUE.replace("INDEX 01 00:01:01", "INDEX 01 00:00:75");
    let path = root.join("album.cue");
    tokio::fs::write(&path, &contents).await.unwrap();
    let store = Store::open(&temp.0.join("library.sqlite")).await.unwrap();
    let progress = request(
        &store,
        ScanRequest::Roots(vec![root]),
        Arc::new(CueProbe(AtomicUsize::new(0))),
    )
    .await;
    assert_eq!(progress.errors, 0);
    let tracks = store.library("", 0).await.unwrap().tracks;
    let first = tracks
        .iter()
        .find(|track| track.metadata.title == "First")
        .unwrap();
    let second = tracks
        .iter()
        .find(|track| track.metadata.title == "Second")
        .unwrap();
    assert_eq!(first.segment.unwrap().end_frame, Some(44100));
    assert_eq!(second.segment.unwrap().start_frame, 44100);
    assert_eq!(first.metadata.duration_ms, 1000);
    assert_eq!(second.metadata.duration_ms, 9000);
    assert_eq!(tokio::fs::read_to_string(path).await.unwrap(), contents);
    store.close().await;
}

#[tokio::test]
async fn cue_sidecars_expand_files_reuse_cache_and_preserve_ids_across_edits_and_removal() {
    let temp = TempDirectory::new();
    let root = temp.music();
    tokio::fs::write(root.join("image.flac"), b"audio")
        .await
        .unwrap();
    tokio::fs::write(root.join("other.flac"), b"audio")
        .await
        .unwrap();
    let store = Store::open(&temp.0.join("library.sqlite")).await.unwrap();
    let probe = Arc::new(CueProbe(AtomicUsize::new(0)));
    request(
        &store,
        ScanRequest::Roots(vec![root.clone()]),
        probe.clone(),
    )
    .await;
    let whole = store
        .library("", 0)
        .await
        .unwrap()
        .tracks
        .iter()
        .find(|track| track.path.ends_with("image.flac"))
        .unwrap()
        .id;
    tokio::fs::write(root.join("album.cue"), format!("\u{feff}{CUE}"))
        .await
        .unwrap();
    assert_eq!(
        request(&store, ScanRequest::Library, probe.clone())
            .await
            .errors,
        0
    );
    let tracks = store.library("", 0).await.unwrap();
    assert_eq!(tracks.tracks.iter().filter(|t| !t.missing).count(), 3);
    assert!(
        tracks
            .tracks
            .iter()
            .find(|t| t.id == whole)
            .unwrap()
            .missing
    );
    let first = tracks
        .tracks
        .iter()
        .find(|t| t.metadata.title == "First")
        .unwrap();
    let second = tracks
        .tracks
        .iter()
        .find(|t| t.metadata.title == "Second")
        .unwrap();
    assert_eq!(first.metadata.album, "Album title");
    assert_eq!(first.metadata.artist, "Album artist");
    assert_eq!(second.metadata.artist, "Guest");
    assert_eq!(first.segment.unwrap().end_frame, Some(44688));
    assert_eq!(second.segment.unwrap().start_frame, 44688);
    assert_eq!(probe.0.load(Ordering::Relaxed), 2);
    let second_id = second.id;
    tokio::fs::write(root.join("album.cue"), CUE.replace("Second", "Renamed"))
        .await
        .unwrap();
    request(&store, ScanRequest::Library, probe.clone()).await;
    assert_eq!(
        store.library("Renamed", 0).await.unwrap().tracks[0].id,
        second_id
    );
    tokio::fs::remove_file(root.join("album.cue"))
        .await
        .unwrap();
    request(&store, ScanRequest::Library, probe.clone()).await;
    let tracks = store.library("", 0).await.unwrap();
    assert!(
        !tracks
            .tracks
            .iter()
            .find(|t| t.id == whole)
            .unwrap()
            .missing
    );
    assert!(
        tracks
            .tracks
            .iter()
            .find(|t| t.id == second_id)
            .unwrap()
            .missing
    );
    assert_eq!(probe.0.load(Ordering::Relaxed), 2);
    store.close().await;
}

#[tokio::test]
async fn cue_validation_is_atomic_and_supports_multiple_files() {
    let temp = TempDirectory::new();
    let root = temp.music();
    for name in ["image.flac", "second.flac"] {
        tokio::fs::write(root.join(name), b"audio").await.unwrap();
    }
    let store = Store::open(&temp.0.join("library.sqlite")).await.unwrap();
    let probe = Arc::new(CueProbe(AtomicUsize::new(0)));
    let multi = format!("{CUE}FILE \"second.flac\" WAVE\n TRACK 03 AUDIO\n  INDEX 01 00:00:00\n");
    tokio::fs::write(root.join("album.cue"), &multi)
        .await
        .unwrap();
    assert_eq!(
        request(
            &store,
            ScanRequest::Roots(vec![root.clone()]),
            probe.clone()
        )
        .await
        .errors,
        0
    );
    let previous = store.library("", 0).await.unwrap().tracks;
    for invalid in [
        CUE.replace("image.flac", "missing.flac"),
        CUE.replace("image.flac", "../image.flac"),
        CUE.replace("00:01:01", "00:00:00"),
        CUE.replace("00:01:01", "00:12:00"),
        CUE.replace("TRACK 02 AUDIO", "TRACK 02 MODE1/2352"),
        CUE.replace("  TITLE \"First\"", "  PREGAP 00:00:01"),
        CUE.replace("INDEX 01 00:00:00", "INDEX 00 00:00:00"),
    ] {
        tokio::fs::write(root.join("album.cue"), invalid)
            .await
            .unwrap();
        assert_eq!(
            request(&store, ScanRequest::Library, probe.clone())
                .await
                .errors,
            1
        );
        assert_eq!(store.library("", 0).await.unwrap().tracks, previous);
    }
    tokio::fs::write(root.join("album.cue"), &multi)
        .await
        .unwrap();
    tokio::fs::write(root.join("duplicate.cue"), CUE)
        .await
        .unwrap();
    assert_eq!(request(&store, ScanRequest::Library, probe).await.errors, 1);
    assert_eq!(store.library("", 0).await.unwrap().tracks, previous);
    store.close().await;
}

#[tokio::test]
async fn audio_cache_uses_whole_seconds_and_size() {
    let temp = TempDirectory::new();
    let root = temp.music();
    let path = root.join("one.flac");
    let timestamp = UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
    tokio::fs::write(&path, b"valid").await.unwrap();
    let set_time = |time| {
        std::fs::File::open(&path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(time))
            .unwrap()
    };
    set_time(timestamp);
    let store = Store::open(&temp.0.join("library.sqlite")).await.unwrap();
    let probe = Arc::new(Probe::default());
    run_scan(&store, &root, &probe).await;
    tokio::fs::write(&path, b"other").await.unwrap();
    set_time(timestamp + std::time::Duration::from_millis(100));
    run_scan(&store, &root, &probe).await;
    assert_eq!(probe.0.load(Ordering::Relaxed), 1);
    set_time(timestamp + std::time::Duration::from_secs(1));
    run_scan(&store, &root, &probe).await;
    assert_eq!(probe.0.load(Ordering::Relaxed), 2);
    store.close().await;
}

#[cfg(unix)]
#[tokio::test]
async fn unreadable_folder_does_not_block_a_healthy_sibling_or_mark_files_missing() {
    use std::os::unix::fs::PermissionsExt;
    struct Restore(PathBuf);
    impl Drop for Restore {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
        }
    }
    let temp = TempDirectory::new();
    let root = temp.music();
    let blocked = root.join("blocked");
    let healthy = root.join("healthy");
    for path in [&blocked, &healthy] {
        tokio::fs::create_dir_all(path).await.unwrap();
        tokio::fs::write(path.join("one.flac"), b"valid")
            .await
            .unwrap();
    }
    let store = Store::open(&temp.0.join("library.sqlite")).await.unwrap();
    let probe = Arc::new(Probe::default());
    run_scan(&store, &root, &probe).await;
    let restore = Restore(blocked.clone());
    std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o000)).unwrap();
    // A privileged test process may bypass filesystem access restrictions.
    if std::fs::read_dir(&blocked).is_ok() {
        drop(restore);
        store.close().await;
        return;
    }
    tokio::fs::write(healthy.join("two.flac"), b"valid")
        .await
        .unwrap();
    assert!(request(&store, ScanRequest::Library, probe).await.errors > 0);
    let folders = store.folders("", 0).await.unwrap();
    let failed = folders
        .folders
        .iter()
        .find(|folder| folder.path.ends_with("blocked"))
        .unwrap();
    let successful = folders
        .folders
        .iter()
        .find(|folder| folder.path.ends_with("healthy"))
        .unwrap();
    assert_eq!((failed.status, failed.generation), (ScanStatus::Error, 1));
    assert_eq!(
        (
            successful.status,
            successful.generation,
            successful.track_count
        ),
        (ScanStatus::Ready, 2, 2)
    );
    assert!(
        store
            .library("", 0)
            .await
            .unwrap()
            .tracks
            .iter()
            .all(|track| !track.missing)
    );
    drop(restore);
    store.close().await;
}
