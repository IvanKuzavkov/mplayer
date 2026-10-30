use super::*;
#[path = "albums.rs"]
mod album_tests;
use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

static DIRECTORY_ID: AtomicU64 = AtomicU64::new(0);

#[tokio::test]
async fn album_year_migration_preserves_catalog_and_refreshes_old_cached_metadata() {
    let temp = TempDirectory::new();
    let database = temp.0.join("library.sqlite");
    let pool = SqlitePoolOptions::new()
        .connect_with(
            SqliteConnectOptions::new()
                .filename(&database)
                .create_if_missing(true)
                .foreign_keys(true),
        )
        .await
        .unwrap();
    sqlx::migrate!("./migrations")
        .run_to(1, &pool)
        .await
        .unwrap();
    let old = Store { pool };
    let root = old.register_root(&temp.0).await.unwrap();
    let folder = old.ensure_folder(&root, &temp.0).await.unwrap();
    let path = temp.0.join("one.flac");
    sqlx::query("INSERT INTO media_tracks (id, folder_id, path, modified_secs, file_size, title, artist, album, duration_ms, sample_rate, channels, codec) VALUES (1, ?, ?, 7, 8, 'One', 'Artist', 'Album', 1000, 44100, 2, 'flac')")
        .bind(folder.id).bind(encode_path(&path)).execute(&old.pool).await.unwrap();
    sqlx::query("INSERT INTO tracks (id, media_id, logical_index, title, artist, album, duration_ms, sample_rate, channels, codec) VALUES (1, 1, 0, 'One', 'Artist', 'Album', 1000, 44100, 2, 'flac')")
        .execute(&old.pool).await.unwrap();
    let playlist = old.create_playlist("Keep").await.unwrap();
    old.add_playlist_entry(playlist, 1).await.unwrap();
    old.close().await;
    let store = Store::open(&database).await.unwrap();
    assert_eq!(
        store.library("", 0).await.unwrap().tracks[0].metadata.year,
        None
    );
    assert!(store.folder_snapshot(folder.id).await.unwrap().is_empty());
    assert_eq!(
        store.playlist_entries(playlist).await.unwrap()[0].track.id,
        1
    );
    let meta = TrackMetadata {
        title: "One".into(),
        artist: "Artist".into(),
        album: "Album".into(),
        year: Some(2026),
        ..Default::default()
    };
    store
        .commit_folder(
            &folder,
            vec![PreparedMedia::whole(record(path, meta.clone()))],
        )
        .await
        .unwrap();
    assert_eq!(
        store.folder_snapshot(folder.id).await.unwrap()[0].metadata,
        meta
    );
    assert_eq!(
        store.albums("Album", 0).await.unwrap().albums[0].year,
        Some(2026)
    );
    store.close().await;
    let reopened = Store::open(&database).await.unwrap();
    let entries = reopened.playlist_entries(playlist).await.unwrap();
    assert_eq!(entries[0].track.id, 1);
    assert_eq!(entries[0].track.metadata.year, Some(2026));
    reopened.close().await;
}

struct TempDirectory(PathBuf);

impl TempDirectory {
    fn new() -> Self {
        let id = DIRECTORY_ID.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "mplayer-groups-{}-{nanos}-{id}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TempDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn record(path: PathBuf, metadata: TrackMetadata) -> MediaRecord {
    MediaRecord {
        path,
        modified_secs: 7,
        file_size: 8,
        metadata,
    }
}

fn metadata(
    title: &str,
    album: &str,
    artist: &str,
    album_artist: &str,
    disc: Option<u32>,
    track: Option<u32>,
) -> TrackMetadata {
    TrackMetadata {
        title: title.into(),
        album: album.into(),
        artist: artist.into(),
        album_artist: album_artist.into(),
        disc_number: disc,
        track_number: track,
        duration_ms: 1_000,
        ..Default::default()
    }
}

#[tokio::test]
async fn groups_use_immediate_folders_and_album_artist_with_disc_track_order() {
    let temp = TempDirectory::new();
    let store = Store::open(&temp.0.join("library.sqlite")).await.unwrap();
    let folder = temp.0.join("100%_music");
    let child = folder.join("disc-two");
    store
        .seed(vec![
            record(
                folder.join("one.flac"),
                metadata(
                    "Z tenth",
                    "Shared title",
                    "Solo A",
                    "Compilation",
                    Some(1),
                    Some(10),
                ),
            ),
            record(
                folder.join("two.flac"),
                metadata(
                    "Y second",
                    "Shared title",
                    "Solo B",
                    "Compilation",
                    Some(1),
                    Some(2),
                ),
            ),
            record(
                folder.join("three.flac"),
                metadata(
                    "B unnumbered",
                    "Shared title",
                    "Solo C",
                    "Compilation",
                    None,
                    None,
                ),
            ),
            record(
                child.join("four.flac"),
                metadata(
                    "A next disc",
                    "Shared title",
                    "Solo A",
                    "Compilation",
                    Some(2),
                    Some(1),
                ),
            ),
            record(
                child.join("five.flac"),
                metadata(
                    "Different release",
                    "Shared title",
                    "Different artist",
                    "",
                    None,
                    None,
                ),
            ),
            record(
                child.join("six.flac"),
                metadata(
                    "Fallback artist",
                    "Fallback album",
                    "Solo artist",
                    "",
                    None,
                    None,
                ),
            ),
        ])
        .await
        .unwrap();

    let folders = store.folders("%_", 0).await.unwrap();
    assert_eq!(folders.total, 2);
    assert!(folders.folders.iter().all(|item| item.track_count == 3));
    let direct = store
        .library_filtered("", 0, &LibraryFilter::Folder(folder.clone()))
        .await
        .unwrap();
    assert_eq!(direct.total, 3);
    assert!(
        direct
            .tracks
            .iter()
            .all(|track| track.path.parent() == Some(folder.as_path()))
    );

    let albums = store.albums("Shared title", 0).await.unwrap();
    assert_eq!(albums.total, 2);
    let compilation = albums
        .albums
        .iter()
        .find(|album| album.key.artist == "Compilation")
        .unwrap();
    assert_eq!(compilation.track_count, 4);
    assert_eq!(compilation.duration_ms, 4_000);
    assert_eq!(compilation.artwork_path, Some(folder.join("two.flac")));
    let album_filter = LibraryFilter::Album(compilation.key.clone());
    let album_tracks = store.library_filtered("", 0, &album_filter).await.unwrap();
    assert_eq!(
        album_tracks
            .tracks
            .iter()
            .map(|track| track.metadata.title.as_str())
            .collect::<Vec<_>>(),
        vec!["Y second", "Z tenth", "B unnumbered", "A next disc"]
    );
    assert_eq!(
        store
            .library_filtered("Compilation", 0, &album_filter)
            .await
            .unwrap()
            .total,
        4
    );
    assert_eq!(
        store
            .library_filtered("Solo B", 0, &album_filter)
            .await
            .unwrap()
            .total,
        1
    );
    let fallback = store.albums("Fallback album", 0).await.unwrap();
    assert_eq!(fallback.albums[0].key.artist, "Solo artist");
    let other = store.albums("Different artist", 0).await.unwrap();
    assert_eq!(other.albums[0].track_count, 1);
    sqlx::query("UPDATE tracks SET missing=1 WHERE title='Y second'")
        .execute(&store.pool)
        .await
        .unwrap();
    let albums = store.albums("Compilation", 0).await.unwrap();
    assert_eq!(albums.albums[0].artwork_path, Some(folder.join("one.flac")));
    sqlx::query("UPDATE tracks SET missing=1")
        .execute(&store.pool)
        .await
        .unwrap();
    assert!(
        store.albums("Compilation", 0).await.unwrap().albums[0]
            .artwork_path
            .is_none()
    );
    store.close().await;
}

#[tokio::test]
async fn group_queries_paginate_in_sql_and_filter_tracks_without_materializing_library() {
    let temp = TempDirectory::new();
    let store = Store::open(&temp.0.join("library.sqlite")).await.unwrap();
    let total = PAGE_SIZE + 3;
    let records = (0..total)
        .map(|index| {
            record(
                temp.0.join(format!("folder-{index:04}")).join("track.flac"),
                metadata(
                    "Track",
                    &format!("Album {index:04}"),
                    "Artist",
                    "",
                    None,
                    Some(1),
                ),
            )
        })
        .collect();
    store.seed(records).await.unwrap();

    let first_folders = store.folders("", 0).await.unwrap();
    assert_eq!(first_folders.total, total);
    assert_eq!(first_folders.folders.len(), PAGE_SIZE as usize);
    let last_folders = store.folders("", PAGE_SIZE).await.unwrap();
    assert_eq!(last_folders.total, total);
    assert_eq!(last_folders.folders.len(), 3);
    assert_eq!(last_folders.offset, PAGE_SIZE);
    assert_eq!(store.folders("folder-000", 0).await.unwrap().total, 10);

    let first_albums = store.albums("", 0).await.unwrap();
    assert_eq!(first_albums.total, total);
    assert_eq!(first_albums.albums.len(), PAGE_SIZE as usize);
    let last_albums = store.albums("", PAGE_SIZE).await.unwrap();
    assert_eq!(last_albums.albums.len(), 3);
    assert_eq!(last_albums.total, total);
    assert_eq!(store.albums("Album 000", 0).await.unwrap().total, 10);
    assert!(store.albums("%", 0).await.unwrap().albums.is_empty());

    for filter in [
        LibraryFilter::Folder(last_folders.folders[0].path.clone()),
        LibraryFilter::Album(last_albums.albums[0].key.clone()),
    ] {
        let page = store.library_filtered("", 0, &filter).await.unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.tracks.len(), 1);
        let after = store.library_filtered("", 1, &filter).await.unwrap();
        assert_eq!(after.total, 1);
        assert!(after.tracks.is_empty());
    }
    store.close().await;
}

#[tokio::test]
async fn failed_folder_commit_rolls_back_contents_generation_and_playlist_references() {
    let temp = TempDirectory::new();
    let store = Store::open(&temp.0.join("library.sqlite")).await.unwrap();
    let path = temp.0.join("album/one.flac");
    store
        .seed(vec![record(
            path.clone(),
            metadata("Original", "Album", "Artist", "", None, None),
        )])
        .await
        .unwrap();
    let original = store.library("", 0).await.unwrap().tracks[0].clone();
    store.create_playlist("Keep").await.unwrap();
    let playlist = store.playlists().await.unwrap()[0].id;
    store
        .add_playlist_entry(playlist, original.id)
        .await
        .unwrap();
    let folder = store.folders("", 0).await.unwrap().folders[0].clone();
    let mut pending = store.folder(folder.id).await.unwrap();
    pending.generation += 1;
    let mut invalid = record(path, metadata("Changed", "Album", "Artist", "", None, None));
    invalid.metadata.duration_ms = u64::MAX;
    assert!(
        store
            .commit_folder(&pending, vec![PreparedMedia::whole(invalid)])
            .await
            .is_err()
    );
    assert_eq!(store.library("", 0).await.unwrap().tracks[0], original);
    assert_eq!(
        store.folders("", 0).await.unwrap().folders[0].generation,
        folder.generation
    );
    assert_eq!(
        store.playlist_entries(playlist).await.unwrap()[0].track,
        original
    );
    store.close().await;
}

#[tokio::test]
async fn interrupted_status_is_recovered_without_modifying_tracks() {
    let temp = TempDirectory::new();
    let database = temp.0.join("library.sqlite");
    let store = Store::open(&database).await.unwrap();
    store
        .seed(vec![record(
            temp.0.join("album/one.flac"),
            TrackMetadata::default(),
        )])
        .await
        .unwrap();
    let folder = store.folders("", 0).await.unwrap().folders[0].clone();
    store
        .folder_status(folder.id, ScanStatus::Scanning, None)
        .await
        .unwrap();
    store.close().await;
    let reopened = Store::open(&database).await.unwrap();
    let updated = reopened.folders("", 0).await.unwrap().folders[0].clone();
    assert_eq!(updated.status, ScanStatus::Cancelled);
    assert_eq!(updated.generation, folder.generation);
    assert!(!reopened.library("", 0).await.unwrap().tracks[0].missing);
    reopened.close().await;
}

impl Store {
    async fn seed(&self, records: Vec<MediaRecord>) -> Result<()> {
        let mut folders = std::collections::BTreeMap::<PathBuf, Vec<PreparedMedia>>::new();
        for record in records {
            folders
                .entry(record.path.parent().unwrap().into())
                .or_default()
                .push(PreparedMedia::whole(record));
        }
        for (path, media) in folders {
            let root = self.register_root(&path).await?;
            let folder = self.ensure_folder(&root, &path).await?;
            self.commit_folder(&folder, media).await?;
        }
        Ok(())
    }
}

#[tokio::test]
async fn removing_scan_root_is_atomic_and_keeps_other_roots_and_music_files() {
    let temp = TempDirectory::new();
    let store = Store::open(&temp.0.join("library.sqlite")).await.unwrap();
    let first = temp.0.join("music");
    let other = temp.0.join("music-other");
    for root in [&first, &other] {
        tokio::fs::create_dir_all(root.join("album")).await.unwrap();
        tokio::fs::write(root.join("album/track.flac"), b"keep music")
            .await
            .unwrap();
        let registered = store.register_root(root).await.unwrap();
        let folder = store
            .ensure_folder(&registered, &root.join("album"))
            .await
            .unwrap();
        store
            .commit_folder(
                &folder,
                vec![PreparedMedia::whole(record(
                    root.join("album/track.flac"),
                    TrackMetadata::default(),
                ))],
            )
            .await
            .unwrap();
    }
    let tracks = store.library("", 0).await.unwrap().tracks;
    let playlist = store.create_playlist("Keep playlist").await.unwrap();
    for track in tracks.iter() {
        store.add_playlist_entry(playlist, track.id).await.unwrap();
    }
    sqlx::query("CREATE TRIGGER block_folder_removal BEFORE DELETE ON folders BEGIN SELECT RAISE(ABORT, 'fixture'); END")
        .execute(&store.pool).await.unwrap();
    assert!(store.remove_scan_folder(&first).await.is_err());
    assert_eq!(store.roots().await.unwrap().len(), 2);
    assert_eq!(store.library("", 0).await.unwrap().total, 2);
    assert_eq!(store.playlist_entries(playlist).await.unwrap().len(), 2);
    sqlx::query("DROP TRIGGER block_folder_removal")
        .execute(&store.pool)
        .await
        .unwrap();
    store.remove_scan_folder(&first).await.unwrap();
    store.remove_scan_folder(&first).await.unwrap(); // Stale requests are harmless.
    assert_eq!(store.roots().await.unwrap(), vec![other.clone()]);
    let remaining = store.library("", 0).await.unwrap();
    assert_eq!(remaining.total, 1);
    assert!(remaining.tracks[0].path.starts_with(&other));
    assert_eq!(store.folders("", 0).await.unwrap().total, 1);
    assert_eq!(store.playlists().await.unwrap().len(), 1);
    assert_eq!(
        store.playlist_entries(playlist).await.unwrap()[0].track.id,
        remaining.tracks[0].id
    );
    for root in [&first, &other] {
        assert_eq!(
            tokio::fs::read(root.join("album/track.flac"))
                .await
                .unwrap(),
            b"keep music"
        );
    }
    let orphan_media: i64 = sqlx::query_scalar("SELECT count(*) FROM media_tracks m LEFT JOIN folders f ON f.id=m.folder_id WHERE f.id IS NULL")
        .fetch_one(&store.pool).await.unwrap();
    assert_eq!(orphan_media, 0);
    store.close().await;
}

#[cfg(unix)]
#[tokio::test]
async fn distinct_native_folder_paths_stay_distinct_when_display_names_collide() {
    use std::os::unix::ffi::OsStringExt;
    let temp = TempDirectory::new();
    let store = Store::open(&temp.0.join("library.sqlite")).await.unwrap();
    let first = temp
        .0
        .join(std::ffi::OsString::from_vec(b"folder-\xff".to_vec()));
    let second = temp
        .0
        .join(std::ffi::OsString::from_vec(b"folder-\xfe".to_vec()));
    assert_eq!(first.to_string_lossy(), second.to_string_lossy());
    store
        .seed(vec![
            record(
                first.join("track.flac"),
                metadata("First", "", "", "", None, None),
            ),
            record(
                second.join("track.flac"),
                metadata("Second", "", "", "", None, None),
            ),
        ])
        .await
        .unwrap();
    let groups = store.folders("folder-", 0).await.unwrap();
    assert_eq!(groups.total, 2);
    assert_ne!(groups.folders[0].path, groups.folders[1].path);
    assert_eq!(
        store
            .library_filtered("", 0, &LibraryFilter::Folder(first))
            .await
            .unwrap()
            .tracks[0]
            .metadata
            .title,
        "First"
    );
    store.close().await;
}

#[tokio::test]
async fn cancellation_while_waiting_for_folder_write_preserves_the_previous_snapshot() {
    use std::sync::{Arc, Mutex};
    struct BlockingProbe {
        entered: Arc<tokio::sync::Notify>,
        release: Mutex<std::sync::mpsc::Receiver<()>>,
    }
    impl crate::MetadataProbe for BlockingProbe {
        fn probe(&self, _: &Path) -> Result<TrackMetadata> {
            self.entered.notify_one();
            self.release
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(3))?;
            Ok(TrackMetadata {
                title: "Replacement".into(),
                ..Default::default()
            })
        }
    }
    let temp = TempDirectory::new();
    let root = tokio::fs::canonicalize(&temp.0).await.unwrap();
    let path = root.join("one.flac");
    tokio::fs::write(&path, b"new audio").await.unwrap();
    let store = Store::open(&root.join("library.sqlite")).await.unwrap();
    store
        .seed(vec![record(
            path,
            metadata("Original", "Album", "Artist", "", None, None),
        )])
        .await
        .unwrap();
    let previous = store.library("", 0).await.unwrap().tracks;
    let original_folder = store.folders("", 0).await.unwrap().folders[0].clone();
    let (release, released) = std::sync::mpsc::channel();
    let entered = Arc::new(tokio::sync::Notify::new());
    let (sender, mut progress) = tokio::sync::watch::channel(crate::ScanProgress::default());
    let (cancel, task) = crate::scan_task(
        store.clone(),
        crate::ScanRequest::Folder(original_folder.id),
        Arc::new(BlockingProbe {
            entered: entered.clone(),
            release: Mutex::new(released),
        }),
        sender,
    )
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    // Hold the SQLite writer after probing starts, so the prepared folder cannot commit.
    let writer = store.pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
    release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while progress.borrow().processed == 0 {
            progress.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    cancel.cancel().unwrap();
    tokio::task::yield_now().await;
    writer.rollback().await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    assert!(!progress.borrow().running);
    assert_eq!(store.library("", 0).await.unwrap().tracks, previous);
    let cancelled = store.folders("", 0).await.unwrap().folders[0].clone();
    assert_eq!(cancelled.generation, original_folder.generation);
    assert_eq!(cancelled.status, ScanStatus::Cancelled);
    store.close().await;
}

#[path = "playlists.rs"]
mod playlist_tests;
