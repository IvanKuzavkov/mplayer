use std::{
    path::Path,
    sync::{
        Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use super::*;
use crate::{PAGE_SIZE, TrackMetadata};
#[path = "cue.rs"]
mod cue_tests;
#[path = "folders.rs"]
mod folders;

static DIRECTORY_ID: AtomicU64 = AtomicU64::new(0);

#[tokio::test]
async fn scan_walks_nested_folders_and_skips_non_audio_and_symlinks() {
    let temp = TempDirectory::new();
    let root = temp.music();
    let nested = root.join("album").join("disc");
    tokio::fs::create_dir_all(&nested).await.unwrap();
    let mut expected = vec![
        root.join("first.FLAC"),
        nested.join("second.ogg"),
        nested.join("third.DSF"),
        nested.join("fourth.dFf"),
    ];
    for path in &expected {
        tokio::fs::write(path, b"fixture").await.unwrap();
    }
    tokio::fs::write(nested.join("notes.txt"), b"ignored")
        .await
        .unwrap();
    #[cfg(unix)]
    {
        tokio::fs::symlink(&root, nested.join("cycle"))
            .await
            .unwrap();
        tokio::fs::symlink(&expected[0], nested.join("duplicate.flac"))
            .await
            .unwrap();
    }
    let store = Store::open(&temp.0.join("library.sqlite")).await.unwrap();
    let probe = Arc::new(Probe::default());
    let progress = run_scan(&store, &root, &probe).await;
    assert!(!progress.running);
    assert_eq!(progress.errors, 0);
    let mut actual: Vec<_> = store
        .library("", 0)
        .await
        .unwrap()
        .tracks
        .iter()
        .map(|track| track.path.clone())
        .collect();
    expected = expected
        .into_iter()
        .map(|path| std::fs::canonicalize(path).unwrap())
        .collect();
    expected.sort();
    actual.sort();
    assert_eq!(actual, expected);
    assert_eq!(probe.0.load(Ordering::Relaxed), 4);
    store.close().await;
}

#[tokio::test]
async fn cancellation_before_work_publishes_final_progress() {
    let temp = TempDirectory::new();
    let root = temp.music();
    for index in 0..8 {
        tokio::fs::write(root.join(format!("track-{index}.wav")), b"fixture")
            .await
            .unwrap();
    }
    let store = Store::open(&temp.0.join("library.sqlite")).await.unwrap();
    let (sender, mut receiver) = watch::channel(ScanProgress::default());
    let (cancel, task) = scan_task(
        store.clone(),
        ScanRequest::Roots(vec![root]),
        Arc::new(Probe::default()),
        sender,
    )
    .await
    .unwrap();
    assert!(receiver.borrow().running);
    cancel.cancel().unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    assert!(!receiver.borrow_and_update().running);
    assert!(receiver.changed().await.is_err());
    assert_eq!(store.library("", 0).await.unwrap().total, 0);
    store.close().await;
}

struct TempDirectory(PathBuf);

impl TempDirectory {
    fn new() -> Self {
        let id = DIRECTORY_ID.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("mplayer-core-{}-{nanos}-{id}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn music(&self) -> PathBuf {
        let path = self.0.join("music");
        std::fs::create_dir_all(&path).unwrap();
        path
    }
}

impl Drop for TempDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[derive(Default)]
struct Probe(AtomicUsize);

impl MetadataProbe for Probe {
    fn probe(&self, path: &Path) -> Result<TrackMetadata> {
        self.0.fetch_add(1, Ordering::Relaxed);
        if std::fs::read(path)? == b"bad" {
            anyhow::bail!("invalid audio fixture");
        }
        Ok(TrackMetadata {
            title: path.file_stem().unwrap().to_string_lossy().into_owned(),
            artist: "Test artist".into(),
            sample_rate: 44_100,
            channels: 2,
            ..Default::default()
        })
    }
}

async fn run_scan(store: &Store, root: &Path, probe: &Arc<Probe>) -> ScanProgress {
    let (sender, receiver) = watch::channel(ScanProgress::default());
    let (_cancel, task) = scan_task(
        store.clone(),
        ScanRequest::Roots(vec![root.into()]),
        probe.clone(),
        sender,
    )
    .await
    .unwrap();
    task.await.unwrap().unwrap();
    receiver.borrow().clone()
}

#[tokio::test]
async fn incremental_scans_keep_ids_and_detect_changes_and_missing_files() {
    let temp = TempDirectory::new();
    let root = temp.music();
    let file = root.join("first.FLAC");
    std::fs::write(&file, b"valid").unwrap();
    let store = Store::open(&temp.0.join("library.sqlite")).await.unwrap();
    let probe = Arc::new(Probe::default());
    assert_eq!(run_scan(&store, &root, &probe).await.processed, 1);
    let initial = store.library("", 0).await.unwrap().tracks[0].clone();
    run_scan(&store, &root, &probe).await;
    assert_eq!(probe.0.load(Ordering::Relaxed), 1);
    std::fs::write(&file, b"modified fixture").unwrap();
    run_scan(&store, &root, &probe).await;
    assert_eq!(probe.0.load(Ordering::Relaxed), 2);
    let current = store.library("", 0).await.unwrap().tracks[0].clone();
    assert_eq!(initial.id, current.id);
    std::fs::remove_file(&file).unwrap();
    run_scan(&store, &root, &probe).await;
    assert!(store.library("", 0).await.unwrap().tracks[0].missing);
    assert_eq!(
        store.roots().await.unwrap(),
        vec![std::fs::canonicalize(root).unwrap()]
    );
    store.close().await;
}

#[tokio::test]
async fn failed_and_cancelled_scans_do_not_mark_tracks_missing() {
    let temp = TempDirectory::new();
    let root = temp.music();
    let file = root.join("first.flac");
    std::fs::write(&file, b"valid").unwrap();
    let store = Store::open(&temp.0.join("library.sqlite")).await.unwrap();
    let probe = Arc::new(Probe::default());
    run_scan(&store, &root, &probe).await;
    std::fs::remove_file(file).unwrap();
    std::fs::write(root.join("broken.mp3"), b"bad").unwrap();
    assert_eq!(run_scan(&store, &root, &probe).await.errors, 1);
    assert!(!store.library("", 0).await.unwrap().tracks[0].missing);
    let (sender, receiver) = watch::channel(ScanProgress::default());
    let (cancel, task) = scan_task(store.clone(), ScanRequest::Roots(vec![root]), probe, sender)
        .await
        .unwrap();
    cancel.cancel().unwrap();
    assert!(task.await.unwrap().is_err());
    assert!(!receiver.borrow().running);
    assert!(!store.library("", 0).await.unwrap().tracks[0].missing);
    store.close().await;
}

#[tokio::test]
async fn cancellation_during_a_probe_preserves_existing_library_state() {
    struct CancellingProbe {
        cancel: Mutex<Option<ScanCancellationToken>>,
        release: Mutex<std::sync::mpsc::Receiver<()>>,
    }
    impl MetadataProbe for CancellingProbe {
        fn probe(&self, _: &Path) -> Result<TrackMetadata> {
            self.cancel.lock().unwrap().take().unwrap().cancel()?;
            let _ = self
                .release
                .lock()
                .unwrap()
                .recv_timeout(std::time::Duration::from_secs(3));
            Ok(TrackMetadata::default())
        }
    }
    let temp = TempDirectory::new();
    let root = temp.music();
    let original = root.join("original.flac");
    std::fs::write(&original, b"valid").unwrap();
    let store = Store::open(&temp.0.join("library.sqlite")).await.unwrap();
    run_scan(&store, &root, &Arc::new(Probe::default())).await;
    std::fs::remove_file(original).unwrap();
    std::fs::write(root.join("replacement.flac"), b"valid").unwrap();
    let (release, receiver) = std::sync::mpsc::channel();
    let probe = Arc::new(CancellingProbe {
        cancel: Mutex::new(None),
        release: Mutex::new(receiver),
    });
    let (sender, receiver) = watch::channel(ScanProgress::default());
    let (cancel, task) = scan_task(
        store.clone(),
        ScanRequest::Roots(vec![root]),
        probe.clone(),
        sender,
    )
    .await
    .unwrap();
    *probe.cancel.lock().unwrap() = Some(cancel);
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    let _ = release.send(());
    assert!(!receiver.borrow().running);
    assert_eq!(receiver.borrow().processed, 0);
    let folder = store.folders("", 0).await.unwrap().folders[0].clone();
    assert_eq!(folder.status, ScanStatus::Cancelled);
    assert_eq!(folder.generation, 1);
    let library = store.library("", 0).await.unwrap();
    assert_eq!(library.total, 1);
    assert!(!library.tracks[0].missing);
    store.close().await;
}

#[tokio::test]
async fn large_import_bounds_simultaneous_metadata_probes() {
    struct ConcurrentProbe {
        active: AtomicUsize,
        peak: AtomicUsize,
    }
    impl MetadataProbe for ConcurrentProbe {
        fn probe(&self, _: &Path) -> Result<TrackMetadata> {
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(active, Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(3));
            self.active.fetch_sub(1, Ordering::SeqCst);
            Ok(TrackMetadata::default())
        }
    }
    let temp = TempDirectory::new();
    let root = temp.music();
    for index in 0..40 {
        std::fs::write(root.join(format!("track-{index}.flac")), b"valid").unwrap();
    }
    let store = Store::open(&temp.0.join("library.sqlite")).await.unwrap();
    let probe = Arc::new(ConcurrentProbe {
        active: AtomicUsize::new(0),
        peak: AtomicUsize::new(0),
    });
    let (sender, receiver) = watch::channel(ScanProgress::default());
    let (_cancel, task) = scan_task(
        store.clone(),
        ScanRequest::Roots(vec![root]),
        probe.clone(),
        sender,
    )
    .await
    .unwrap();
    task.await.unwrap().unwrap();
    assert_eq!(receiver.borrow().processed, 40);
    assert_eq!(store.library("", 0).await.unwrap().total, 40);
    assert!((1..=PROBE_CONCURRENCY).contains(&probe.peak.load(Ordering::SeqCst)));
    store.close().await;
}

#[tokio::test]
async fn playlists_settings_and_search_survive_reopening() {
    let temp = TempDirectory::new();
    let root = temp.music();
    for name in ["100%_real.flac", "second.wav", "third.ogg"] {
        std::fs::write(root.join(name), b"valid").unwrap();
    }
    let database = temp.0.join("library.sqlite");
    let store = Store::open(&database).await.unwrap();
    run_scan(&store, &root, &Arc::new(Probe::default())).await;
    let tracks = store.library("", 0).await.unwrap().tracks;
    assert_eq!(store.library("%_", 0).await.unwrap().total, 1);
    assert!(
        store
            .library("", PAGE_SIZE)
            .await
            .unwrap()
            .tracks
            .is_empty()
    );
    store.create_playlist("Favourites").await.unwrap();
    assert!(store.create_playlist("  ").await.is_err());
    let playlist = store.playlists().await.unwrap().remove(0);
    for track in tracks.iter() {
        store
            .add_playlist_entry(playlist.id, track.id)
            .await
            .unwrap();
    }
    let entries = store.playlist_entries(playlist.id).await.unwrap();
    store.move_playlist_entry(entries[2].id, 0).await.unwrap();
    let reordered = store.playlist_entries(playlist.id).await.unwrap();
    assert_eq!(reordered[0].track.id, tracks[2].id);
    store.remove_playlist_entry(reordered[1].id).await.unwrap();
    store.set_setting("volume", "0.7").await.unwrap();
    store.close().await;
    let reopened = Store::open(&database).await.unwrap();
    assert_eq!(
        reopened.get_setting("volume").await.unwrap().as_deref(),
        Some("0.7")
    );
    assert_eq!(
        reopened.playlist_entries(playlist.id).await.unwrap().len(),
        2
    );
    reopened.delete_playlist(playlist.id).await.unwrap();
    assert!(
        reopened
            .playlist_entries(playlist.id)
            .await
            .unwrap()
            .is_empty()
    );
    reopened.close().await;
}

#[cfg(unix)]
#[tokio::test]
async fn non_utf8_paths_round_trip_without_collisions() {
    use std::os::unix::ffi::OsStringExt;
    let temp = TempDirectory::new();
    let root = temp.music();
    let name = std::ffi::OsString::from_vec(b"track-\xff.flac".to_vec());
    let path = root.join(name);
    let store = Store::open(&temp.0.join("library.sqlite")).await.unwrap();
    // macOS filesystems reject invalid UTF-8 filenames, so exercise the storage
    // encoding directly rather than requiring such a file to exist on disk.
    let root_record = store.register_root(&root).await.unwrap();
    let folder = store.ensure_folder(&root_record, &root).await.unwrap();
    store
        .commit_folder(
            &folder,
            vec![PreparedMedia::whole(MediaRecord {
                path: path.clone(),
                modified_secs: 0,
                file_size: 5,
                metadata: TrackMetadata {
                    title: "non-UTF8 fixture".into(),
                    ..Default::default()
                },
            })],
        )
        .await
        .unwrap();
    assert_eq!(store.library("", 0).await.unwrap().tracks[0].path, path);
    store.close().await;
}

/// Reproduce with:
/// cargo test -p mplayer-core --release benchmark_large_library -- --ignored --nocapture
///
/// The probe reads synthetic files and returns deterministic metadata. These
/// timings measure filesystem/database orchestration, not FLAC decode speed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "manual 10,000-file library benchmark"]
#[test_log::test]
async fn benchmark_large_library() {
    const TRACK_COUNT: u64 = 10_000;
    const SEARCH_SAMPLES: usize = 25;

    let temp = TempDirectory::new();
    let root = temp.music();
    let started = std::time::Instant::now();
    for index in 0..TRACK_COUNT {
        std::fs::write(
            root.join(format!("track-{index:05}.flac")),
            b"valid fixture",
        )
        .unwrap();
    }
    let fixture_generation = started.elapsed();
    let store = Store::open(&temp.0.join("library.sqlite")).await.unwrap();
    let probe = Arc::new(Probe::default());

    let started = std::time::Instant::now();
    let imported = run_scan(&store, &root, &probe).await;
    let initial_scan = started.elapsed();
    assert_eq!(imported.processed, TRACK_COUNT);
    assert_eq!(imported.errors, 0);
    let initial_probes = probe.0.load(Ordering::Relaxed);
    assert_eq!(initial_probes, TRACK_COUNT as usize);

    let started = std::time::Instant::now();
    let rescanned = run_scan(&store, &root, &probe).await;
    let unchanged_rescan = started.elapsed();
    assert_eq!(rescanned.processed, TRACK_COUNT);
    assert_eq!(rescanned.errors, 0);
    assert_eq!(probe.0.load(Ordering::Relaxed), initial_probes);

    let started = std::time::Instant::now();
    let first_page = store.library("", 0).await.unwrap();
    let first_page_time = started.elapsed();
    assert_eq!(first_page.total, TRACK_COUNT);
    assert_eq!(first_page.tracks.len(), PAGE_SIZE as usize);
    let mut search_times = Vec::with_capacity(SEARCH_SAMPLES);
    for index in 0..SEARCH_SAMPLES {
        let offset = (index as u64 * PAGE_SIZE) % TRACK_COUNT;
        let started = std::time::Instant::now();
        let page = store.library("track-", offset).await.unwrap();
        search_times.push(started.elapsed());
        assert_eq!(page.total, TRACK_COUNT);
        assert_eq!(page.offset, offset);
        assert!(page.tracks.len() <= PAGE_SIZE as usize);
        assert!(!page.tracks.is_empty());
    }
    search_times.sort_unstable();
    tracing::info!(
        "library_benchmark tracks={TRACK_COUNT} synthetic_metadata_probe=true fixture_generation_ms={} initial_scan_ms={} unchanged_rescan_ms={} first_page_us={} search_samples={SEARCH_SAMPLES} search_p50_us={} search_p95_us={} initial_probes={initial_probes} rescan_additional_probes=0",
        fixture_generation.as_millis(),
        initial_scan.as_millis(),
        unchanged_rescan.as_millis(),
        first_page_time.as_micros(),
        search_times[SEARCH_SAMPLES / 2].as_micros(),
        search_times[SEARCH_SAMPLES * 95 / 100].as_micros(),
    );
    store.close().await;
}
