use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::bail;
use sqlx::{
    QueryBuilder, Row, Sqlite, SqlitePool,
    sqlite::{
        SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteRow, SqliteSynchronous,
    },
};

use crate::{
    AlbumKey, AlbumPage, AlbumSummary, FolderPage, FolderSummary, LibraryFilter, LibraryPage,
    PAGE_SIZE, Playlist, PlaylistEntry, Result, ScanStatus, Track, TrackMetadata, TrackSegment,
};

#[derive(Clone)]
pub struct Store {
    pool: SqlitePool,
}

#[derive(Clone)]
pub(crate) struct RootRecord {
    pub id: i64,
    pub path: PathBuf,
    pub generation: i64,
}
#[derive(Clone)]
pub(crate) struct FolderRecord {
    pub id: i64,
    pub path: PathBuf,
    pub generation: i64,
}
#[derive(Clone)]
pub(crate) struct MediaRecord {
    pub path: PathBuf,
    pub modified_secs: i64,
    pub file_size: u64,
    pub metadata: TrackMetadata,
}
pub(crate) struct PreparedTrack {
    pub index: u8,
    pub metadata: TrackMetadata,
    pub segment: Option<crate::TrackSegment>,
    pub cue_path: Option<PathBuf>,
}
pub(crate) struct PreparedMedia {
    pub media: MediaRecord,
    pub tracks: Vec<PreparedTrack>,
}
impl PreparedMedia {
    pub fn whole(media: MediaRecord) -> Self {
        let track = PreparedTrack {
            index: 0,
            metadata: media.metadata.clone(),
            segment: None,
            cue_path: None,
        };
        Self {
            media,
            tracks: vec![track],
        }
    }
}

impl Store {
    #[tracing::instrument(skip_all, fields(path = %path.display()), err)]
    pub async fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            tokio::fs::create_dir_all(parent).await?;
        }
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .foreign_keys(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .busy_timeout(Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(options)
            .await?;
        sqlx::migrate!("./migrations").run(&pool).await?;
        let store = Self { pool };
        store
            .interrupt_scans(ScanStatus::Cancelled, "Scan interrupted")
            .await?;
        tracing::info!("library database ready");
        Ok(store)
    }

    #[tracing::instrument(skip_all, fields(offset), err)]
    pub async fn library(&self, query: &str, offset: u64) -> Result<LibraryPage> {
        self.library_filtered(query, offset, &LibraryFilter::All)
            .await
    }

    #[tracing::instrument(skip_all, fields(offset, ?filter), err)]
    pub async fn library_filtered(
        &self,
        query: &str,
        offset: u64,
        filter: &LibraryFilter,
    ) -> Result<LibraryPage> {
        let mut tx = self.pool.begin().await?;
        let mut count = track_query(
            "SELECT count(*) FROM tracks t JOIN media_tracks m ON m.id=t.media_id JOIN folders f ON f.id=m.folder_id",
            query,
            filter,
        );
        let total = count
            .build_query_scalar::<i64>()
            .fetch_one(&mut *tx)
            .await?;
        let mut tracks = track_query(
            "SELECT t.*, m.path FROM tracks t JOIN media_tracks m ON m.id=t.media_id JOIN folders f ON f.id=m.folder_id",
            query,
            filter,
        );
        if matches!(filter, LibraryFilter::Album(_)) {
            tracks.push(" ORDER BY COALESCE(t.disc_number, 1), t.track_number IS NULL, t.track_number, t.title COLLATE NOCASE, t.id");
        } else {
            tracks.push(" ORDER BY t.title COLLATE NOCASE, t.id");
        }
        tracks
            .push(" LIMIT ")
            .push_bind(PAGE_SIZE as i64)
            .push(" OFFSET ")
            .push_bind(i64::try_from(offset)?);
        let rows = tracks.build().fetch_all(&mut *tx).await?;
        tx.commit().await?;
        Ok(LibraryPage {
            tracks: std::sync::Arc::new(rows.iter().map(track_from_row).collect::<Result<_>>()?),
            total: total as u64,
            offset,
            query: query.into(),
        })
    }

    /// All available album tracks in disc/track order, independently of the displayed page.
    #[tracing::instrument(skip_all, err)]
    pub async fn album_tracks(&self, key: &AlbumKey) -> Result<Vec<Track>> {
        let mut query = track_query(
            "SELECT t.*, m.path FROM tracks t JOIN media_tracks m ON m.id=t.media_id JOIN folders f ON f.id=m.folder_id",
            "",
            &LibraryFilter::Album(key.clone()),
        );
        query.push(" AND t.missing=0 ORDER BY COALESCE(t.disc_number, 1), t.track_number IS NULL, t.track_number, t.title COLLATE NOCASE, t.id");
        query
            .build()
            .fetch_all(&self.pool)
            .await?
            .iter()
            .map(track_from_row)
            .collect()
    }

    #[tracing::instrument(skip_all, fields(offset), err)]
    pub async fn folders(&self, query: &str, offset: u64) -> Result<FolderPage> {
        let pattern = search_pattern(query);
        let mut tx = self.pool.begin().await?;
        let total: i64 =
            sqlx::query_scalar("SELECT count(*) FROM folders WHERE display_path LIKE ? ESCAPE '!'")
                .bind(&pattern)
                .fetch_one(&mut *tx)
                .await?;
        let rows = sqlx::query("SELECT f.*, (SELECT count(*) FROM tracks t JOIN media_tracks m ON m.id=t.media_id WHERE m.folder_id=f.id AND t.missing=0) AS track_count FROM folders f WHERE display_path LIKE ? ESCAPE '!' ORDER BY display_path COLLATE NOCASE, path LIMIT ? OFFSET ?")
            .bind(&pattern).bind(PAGE_SIZE as i64).bind(i64::try_from(offset)?).fetch_all(&mut *tx).await?;
        tx.commit().await?;
        let folders = rows
            .iter()
            .map(|row| {
                Ok(FolderSummary {
                    id: row.try_get("id")?,
                    generation: row.try_get("generation")?,
                    status: row.try_get("status")?,
                    error: row.try_get("error")?,
                    path: decode_path(row.try_get("path")?)?,
                    track_count: u64::try_from(row.try_get::<i64, _>("track_count")?)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(FolderPage {
            folders: folders.into(),
            total: total as u64,
            offset,
            query: query.into(),
        })
    }

    #[tracing::instrument(skip_all, fields(offset), err)]
    pub async fn albums(&self, query: &str, offset: u64) -> Result<AlbumPage> {
        let pattern = search_pattern(query);
        let mut tx = self.pool.begin().await?;
        let total: i64 = sqlx::query_scalar("SELECT count(*) FROM (SELECT album, COALESCE(NULLIF(album_artist, ''), artist) AS group_artist FROM tracks GROUP BY album, group_artist HAVING album LIKE ? ESCAPE '!' OR group_artist LIKE ? ESCAPE '!')")
            .bind(&pattern).bind(&pattern).fetch_one(&mut *tx).await?;
        let rows = sqlx::query(
            "SELECT t.album, COALESCE(NULLIF(t.album_artist, ''), t.artist) AS group_artist,
                    count(*) AS track_count, sum(t.duration_ms) AS duration_ms, min(t.year) AS year,
                    (SELECT m.path FROM tracks a JOIN media_tracks m ON m.id=a.media_id
                     WHERE a.album=t.album
                       AND COALESCE(NULLIF(a.album_artist, ''), a.artist)
                           = COALESCE(NULLIF(t.album_artist, ''), t.artist)
                       AND a.missing=0
                     ORDER BY COALESCE(a.disc_number, 1), a.track_number IS NULL,
                              a.track_number, a.id LIMIT 1) AS artwork_path
             FROM tracks t GROUP BY t.album, group_artist
             HAVING t.album LIKE ? ESCAPE '!' OR group_artist LIKE ? ESCAPE '!'
             ORDER BY t.album COLLATE NOCASE, group_artist COLLATE NOCASE,
                      t.album, group_artist LIMIT ? OFFSET ?",
        )
        .bind(&pattern)
        .bind(&pattern)
        .bind(PAGE_SIZE as i64)
        .bind(i64::try_from(offset)?)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        let albums = rows
            .iter()
            .map(|row| {
                Ok(AlbumSummary {
                    key: AlbumKey {
                        title: row.try_get("album")?,
                        artist: row.try_get("group_artist")?,
                    },
                    year: row
                        .try_get::<Option<i64>, _>("year")?
                        .map(u32::try_from)
                        .transpose()?,
                    track_count: u64::try_from(row.try_get::<i64, _>("track_count")?)?,
                    duration_ms: u64::try_from(row.try_get::<i64, _>("duration_ms")?)?,
                    artwork_path: row
                        .try_get::<Option<Vec<u8>>, _>("artwork_path")?
                        .map(decode_path)
                        .transpose()?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(AlbumPage {
            albums: albums.into(),
            total: total as u64,
            offset,
            query: query.into(),
        })
    }

    #[tracing::instrument(skip_all, err)]
    pub async fn roots(&self) -> Result<Vec<PathBuf>> {
        let rows = sqlx::query("SELECT path FROM scan_folders ORDER BY id")
            .fetch_all(&self.pool)
            .await?;
        rows.iter()
            .map(|row| decode_path(row.try_get("path")?))
            .collect()
    }

    /// Remove a scan root and its catalog entries. Never modifies music files.
    #[tracing::instrument(skip_all, fields(path = %path.display()), err)]
    pub async fn remove_scan_folder(&self, path: &Path) -> Result<()> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let root: Option<i64> = sqlx::query_scalar("SELECT id FROM scan_folders WHERE path = ?")
            .bind(encode_path(path))
            .fetch_optional(&mut *tx)
            .await?;
        if let Some(id) = root {
            sqlx::query("DELETE FROM tracks WHERE media_id IN (SELECT m.id FROM media_tracks m JOIN folders f ON f.id=m.folder_id WHERE f.scan_folder_id=?)")
                .bind(id).execute(&mut *tx).await?;
            sqlx::query("DELETE FROM media_tracks WHERE folder_id IN (SELECT id FROM folders WHERE scan_folder_id=?)")
                .bind(id).execute(&mut *tx).await?;
            sqlx::query("DELETE FROM folders WHERE scan_folder_id=?")
                .bind(id)
                .execute(&mut *tx)
                .await?;
            sqlx::query("DELETE FROM scan_folders WHERE id=?")
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    #[tracing::instrument(skip_all, err)]
    pub async fn playlists(&self) -> Result<Vec<Playlist>> {
        let rows = sqlx::query("SELECT id, name FROM playlists ORDER BY name COLLATE NOCASE, id")
            .fetch_all(&self.pool)
            .await?;
        rows.iter()
            .map(|row| {
                Ok(Playlist {
                    id: row.try_get("id")?,
                    name: row.try_get("name")?,
                })
            })
            .collect()
    }

    #[tracing::instrument(skip_all, fields(playlist_id = id), err)]
    pub async fn playlist_entries(&self, id: i64) -> Result<Vec<PlaylistEntry>> {
        let rows = sqlx::query("SELECT tracks.*, m.path, playlist_entries.id AS entry_id FROM playlist_entries JOIN tracks ON tracks.id = playlist_entries.track_id JOIN media_tracks m ON m.id=tracks.media_id WHERE playlist_entries.playlist_id = ? ORDER BY playlist_entries.position, playlist_entries.id")
            .bind(id).fetch_all(&self.pool).await?;
        rows.iter()
            .map(|row| {
                Ok(PlaylistEntry {
                    id: row.try_get("entry_id")?,
                    track: track_from_row(row)?,
                })
            })
            .collect()
    }

    #[tracing::instrument(skip_all, err)]
    pub async fn create_playlist(&self, name: &str) -> Result<i64> {
        let name = name.trim();
        if name.is_empty() {
            bail!("playlist name cannot be empty");
        }
        // Serialize the check and insert, including requests from other connections.
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM playlists WHERE trim(name) = ? COLLATE NOCASE)",
        )
        .bind(name)
        .fetch_one(&mut *tx)
        .await?;
        if exists {
            bail!("A playlist with this name already exists.");
        }
        let id = sqlx::query_scalar("INSERT INTO playlists(name) VALUES (?) RETURNING id")
            .bind(name)
            .fetch_one(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(id)
    }

    #[tracing::instrument(skip_all, fields(playlist_id = id), err)]
    pub async fn delete_playlist(&self, id: i64) -> Result<()> {
        sqlx::query("DELETE FROM playlists WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    #[tracing::instrument(skip_all, fields(playlist_id, track_id), err)]
    pub async fn add_playlist_entry(&self, playlist_id: i64, track_id: i64) -> Result<()> {
        sqlx::query("INSERT INTO playlist_entries(playlist_id, track_id, position) SELECT ?, ?, COALESCE(MAX(position) + 1, 0) FROM playlist_entries WHERE playlist_id = ?")
            .bind(playlist_id).bind(track_id).bind(playlist_id).execute(&self.pool).await?;
        Ok(())
    }

    #[tracing::instrument(skip_all, fields(entry_id), err)]
    pub async fn remove_playlist_entry(&self, entry_id: i64) -> Result<()> {
        sqlx::query("DELETE FROM playlist_entries WHERE id = ?")
            .bind(entry_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    #[tracing::instrument(skip_all, fields(entry_id, position), err)]
    pub async fn move_playlist_entry(&self, entry_id: i64, position: usize) -> Result<()> {
        // Acquire the writer reservation before reading order; scans may be
        // writing concurrently, and upgrading a deferred WAL read can fail.
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let playlist_id: Option<i64> =
            sqlx::query_scalar("SELECT playlist_id FROM playlist_entries WHERE id = ?")
                .bind(entry_id)
                .fetch_optional(&mut *tx)
                .await?;
        let Some(playlist_id) = playlist_id else {
            bail!("playlist entry no longer exists");
        };
        let mut ids: Vec<i64> = sqlx::query_scalar(
            "SELECT id FROM playlist_entries WHERE playlist_id = ? ORDER BY position, id",
        )
        .bind(playlist_id)
        .fetch_all(&mut *tx)
        .await?;
        ids.retain(|id| *id != entry_id);
        ids.insert(position.min(ids.len()), entry_id);
        for (position, id) in ids.into_iter().enumerate() {
            sqlx::query("UPDATE playlist_entries SET position = ? WHERE id = ?")
                .bind(position as i64)
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    #[tracing::instrument(skip_all, fields(key), err)]
    pub async fn get_setting(&self, key: &str) -> Result<Option<String>> {
        Ok(
            sqlx::query_scalar("SELECT value FROM settings WHERE key = ?")
                .bind(key)
                .fetch_optional(&self.pool)
                .await?,
        )
    }

    #[tracing::instrument(skip_all, fields(key), err)]
    pub async fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        sqlx::query("INSERT INTO settings(key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value")
            .bind(key).bind(value).execute(&self.pool).await?;
        Ok(())
    }

    #[tracing::instrument(skip_all)]
    pub async fn close(&self) {
        self.pool.close().await;
    }

    #[tracing::instrument(skip_all, fields(path = %path.display()), err)]
    pub(crate) async fn register_root(&self, path: &Path) -> Result<RootRecord> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let rows = sqlx::query("SELECT id, path, generation FROM scan_folders ORDER BY id")
            .fetch_all(&mut *tx)
            .await?;
        let roots = rows.iter().map(root_from_row).collect::<Result<Vec<_>>>()?;
        if let Some(root) = roots.iter().find(|root| path.starts_with(&root.path)) {
            tx.commit().await?;
            return Ok(root.clone());
        }
        let id: i64 = sqlx::query_scalar("INSERT INTO scan_folders(path) VALUES (?) RETURNING id")
            .bind(encode_path(path))
            .fetch_one(&mut *tx)
            .await?;
        for child in roots.iter().filter(|root| root.path.starts_with(path)) {
            sqlx::query(
                "UPDATE folders SET scan_folder_id = ?, generation = 0 WHERE scan_folder_id = ?",
            )
            .bind(id)
            .bind(child.id)
            .execute(&mut *tx)
            .await?;
            sqlx::query("DELETE FROM scan_folders WHERE id = ?")
                .bind(child.id)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(RootRecord {
            id,
            path: path.into(),
            generation: 0,
        })
    }

    pub(crate) async fn scan_roots(&self) -> Result<Vec<RootRecord>> {
        sqlx::query("SELECT id, path, generation FROM scan_folders ORDER BY id")
            .fetch_all(&self.pool)
            .await?
            .iter()
            .map(root_from_row)
            .collect()
    }

    pub(crate) async fn begin_root_scan(&self, id: i64) -> Result<i64> {
        Ok(sqlx::query_scalar("UPDATE scan_folders SET generation = generation + 1, status = 'scanning', error = NULL WHERE id = ? RETURNING generation")
            .bind(id).fetch_one(&self.pool).await?)
    }

    pub(crate) async fn root_status(
        &self,
        id: i64,
        status: ScanStatus,
        error: Option<&str>,
    ) -> Result<()> {
        sqlx::query("UPDATE scan_folders SET status = ?, error = ? WHERE id = ?")
            .bind(status)
            .bind(error)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub(crate) async fn root_folders(&self, id: i64) -> Result<Vec<FolderRecord>> {
        sqlx::query("SELECT f.id, f.path, f.scan_folder_id, r.generation FROM folders f JOIN scan_folders r ON r.id = f.scan_folder_id WHERE r.id = ? ORDER BY f.path")
            .bind(id).fetch_all(&self.pool).await?.iter().map(folder_from_row).collect()
    }

    pub(crate) async fn folder(&self, id: i64) -> Result<FolderRecord> {
        folder_from_row(&sqlx::query("SELECT f.id, f.path, f.scan_folder_id, r.generation FROM folders f JOIN scan_folders r ON r.id = f.scan_folder_id WHERE f.id = ?")
            .bind(id).fetch_one(&self.pool).await?)
    }

    pub(crate) async fn ensure_folder(
        &self,
        root: &RootRecord,
        path: &Path,
    ) -> Result<FolderRecord> {
        let id: i64 = sqlx::query_scalar("INSERT INTO folders(scan_folder_id, path, display_path) VALUES (?, ?, ?) ON CONFLICT(path) DO UPDATE SET scan_folder_id = excluded.scan_folder_id RETURNING id")
            .bind(root.id).bind(encode_path(path)).bind(path.to_string_lossy().as_ref()).fetch_one(&self.pool).await?;
        Ok(FolderRecord {
            id,
            path: path.into(),
            generation: root.generation,
        })
    }

    pub(crate) async fn folder_status(
        &self,
        id: i64,
        status: ScanStatus,
        error: Option<&str>,
    ) -> Result<()> {
        sqlx::query("UPDATE folders SET status = ?, error = ? WHERE id = ?")
            .bind(status)
            .bind(error)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub(crate) async fn folder_snapshot(&self, id: i64) -> Result<Vec<MediaRecord>> {
        sqlx::query("SELECT * FROM media_tracks WHERE folder_id = ? AND metadata_version = 2")
            .bind(id)
            .fetch_all(&self.pool)
            .await?
            .iter()
            .map(|row| {
                Ok(MediaRecord {
                    path: decode_path(row.try_get("path")?)?,
                    modified_secs: row.try_get("modified_secs")?,
                    file_size: u64::try_from(row.try_get::<i64, _>("file_size")?)?,
                    metadata: metadata_from_row(row)?,
                })
            })
            .collect()
    }

    #[tracing::instrument(skip_all, fields(folder_id = folder.id, generation = folder.generation, files = media.len()), err)]
    pub(crate) async fn commit_folder(
        &self,
        folder: &FolderRecord,
        media: Vec<PreparedMedia>,
    ) -> Result<()> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        sqlx::query("UPDATE tracks SET missing = 1 WHERE media_id IN (SELECT id FROM media_tracks WHERE folder_id = ?)")
            .bind(folder.id).execute(&mut *tx).await?;
        sqlx::query("UPDATE media_tracks SET missing = 1 WHERE folder_id = ?")
            .bind(folder.id)
            .execute(&mut *tx)
            .await?;
        for item in media {
            let file = item.media;
            let meta = &file.metadata;
            let media_id: i64 = bind_metadata(sqlx::query("INSERT INTO media_tracks(folder_id, path, modified_secs, file_size, title, artist, album, album_artist, track_number, disc_number, duration_ms, sample_rate, channels, bit_depth, codec, year, metadata_version) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 2) ON CONFLICT(path) DO UPDATE SET modified_secs=excluded.modified_secs, file_size=excluded.file_size, title=excluded.title, artist=excluded.artist, album=excluded.album, album_artist=excluded.album_artist, track_number=excluded.track_number, disc_number=excluded.disc_number, duration_ms=excluded.duration_ms, sample_rate=excluded.sample_rate, channels=excluded.channels, bit_depth=excluded.bit_depth, codec=excluded.codec, year=excluded.year, metadata_version=2, missing=0 RETURNING id")
                .bind(folder.id).bind(encode_path(&file.path)).bind(file.modified_secs).bind(i64::try_from(file.file_size)?), meta)?
                .fetch_one(&mut *tx).await?.try_get("id")?;
            for track in item.tracks {
                let start = track
                    .segment
                    .map(|s| i64::try_from(s.start_frame))
                    .transpose()?;
                let end = track
                    .segment
                    .and_then(|s| s.end_frame)
                    .map(i64::try_from)
                    .transpose()?;
                bind_metadata(sqlx::query("INSERT INTO tracks(media_id, logical_index, cue_path, start_frame, end_frame, title, artist, album, album_artist, track_number, disc_number, duration_ms, sample_rate, channels, bit_depth, codec, year) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT(media_id, logical_index) DO UPDATE SET cue_path=excluded.cue_path, start_frame=excluded.start_frame, end_frame=excluded.end_frame, title=excluded.title, artist=excluded.artist, album=excluded.album, album_artist=excluded.album_artist, track_number=excluded.track_number, disc_number=excluded.disc_number, duration_ms=excluded.duration_ms, sample_rate=excluded.sample_rate, channels=excluded.channels, bit_depth=excluded.bit_depth, codec=excluded.codec, year=excluded.year, missing=0")
                    .bind(media_id).bind(track.index).bind(track.cue_path.as_deref().map(encode_path)).bind(start).bind(end), &track.metadata)?
                    .execute(&mut *tx).await?;
            }
        }
        sqlx::query(
            "UPDATE folders SET generation = ?, status = 'ready', error = NULL WHERE id = ?",
        )
        .bind(folder.generation)
        .bind(folder.id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    #[tracing::instrument(skip_all, fields(folder_id = id), err)]
    pub(crate) async fn delete_folder(&self, id: i64) -> Result<()> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        sqlx::query("DELETE FROM tracks WHERE media_id IN (SELECT id FROM media_tracks WHERE folder_id = ?)")
            .bind(id).execute(&mut *tx).await?;
        sqlx::query("DELETE FROM media_tracks WHERE folder_id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM folders WHERE id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        tracing::info!("removed missing folder and associated tracks");
        Ok(())
    }

    pub(crate) async fn interrupt_scans(&self, status: ScanStatus, error: &str) -> Result<()> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        for query in [
            "UPDATE folders SET status = ?, error = ? WHERE status = 'scanning'",
            "UPDATE scan_folders SET status = ?, error = ? WHERE status = 'scanning'",
        ] {
            sqlx::query(query)
                .bind(status)
                .bind(error)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }
}

fn search_pattern(query: &str) -> String {
    // Search input is literal, including SQLite LIKE metacharacters.
    format!(
        "%{}%",
        query
            .replace('!', "!!")
            .replace('%', "!%")
            .replace('_', "!_")
    )
}

fn track_query(prefix: &'static str, query: &str, filter: &LibraryFilter) -> QueryBuilder<Sqlite> {
    let pattern = search_pattern(query);
    let mut builder = QueryBuilder::new(prefix);
    builder
        .push(" WHERE (t.title LIKE ")
        .push_bind(pattern.clone())
        .push(" ESCAPE '!' OR t.artist LIKE ")
        .push_bind(pattern.clone())
        .push(" ESCAPE '!' OR t.album LIKE ")
        .push_bind(pattern.clone())
        .push(" ESCAPE '!' OR t.album_artist LIKE ")
        .push_bind(pattern)
        .push(" ESCAPE '!')");
    match filter {
        LibraryFilter::All => (),
        LibraryFilter::Folder(path) => {
            builder.push(" AND f.path = ").push_bind(encode_path(path));
        }
        LibraryFilter::Album(album) => {
            builder
                .push(" AND t.album = ")
                .push_bind(album.title.clone())
                .push(" AND COALESCE(NULLIF(t.album_artist, ''), t.artist) = ")
                .push_bind(album.artist.clone());
        }
    }
    builder
}

fn track_from_row(row: &SqliteRow) -> Result<Track> {
    Ok(Track {
        id: row.try_get("id")?,
        path: decode_path(row.try_get("path")?)?,
        missing: row.try_get("missing")?,
        segment: row
            .try_get::<Option<i64>, _>("start_frame")?
            .map(|start| -> Result<TrackSegment> {
                Ok(TrackSegment {
                    start_frame: start.try_into()?,
                    end_frame: row
                        .try_get::<Option<i64>, _>("end_frame")?
                        .map(u64::try_from)
                        .transpose()?,
                })
            })
            .transpose()?,
        metadata: metadata_from_row(row)?,
    })
}

#[cfg(unix)]
fn encode_path(path: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes().to_vec()
}

#[cfg(unix)]
fn decode_path(bytes: Vec<u8>) -> Result<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    Ok(std::ffi::OsString::from_vec(bytes).into())
}

#[cfg(windows)]
fn encode_path(path: &Path) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str()
        .encode_wide()
        .flat_map(u16::to_le_bytes)
        .collect()
}

#[cfg(windows)]
fn decode_path(bytes: Vec<u8>) -> Result<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    if !bytes.len().is_multiple_of(2) {
        bail!("invalid stored Windows path");
    }
    let words = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect::<Vec<_>>();
    Ok(std::ffi::OsString::from_wide(&words).into())
}

#[cfg(test)]
#[path = "../tests/unit/storage.rs"]
mod tests;

fn root_from_row(row: &SqliteRow) -> Result<RootRecord> {
    Ok(RootRecord {
        id: row.try_get("id")?,
        path: decode_path(row.try_get("path")?)?,
        generation: row.try_get("generation")?,
    })
}
fn folder_from_row(row: &SqliteRow) -> Result<FolderRecord> {
    Ok(FolderRecord {
        id: row.try_get("id")?,
        path: decode_path(row.try_get("path")?)?,
        generation: row.try_get("generation")?,
    })
}
fn metadata_from_row(row: &SqliteRow) -> Result<TrackMetadata> {
    Ok(TrackMetadata {
        title: row.try_get("title")?,
        artist: row.try_get("artist")?,
        album: row.try_get("album")?,
        album_artist: row.try_get("album_artist")?,
        year: row
            .try_get::<Option<i64>, _>("year")?
            .map(u32::try_from)
            .transpose()?,
        track_number: row
            .try_get::<Option<i64>, _>("track_number")?
            .map(u32::try_from)
            .transpose()?,
        disc_number: row
            .try_get::<Option<i64>, _>("disc_number")?
            .map(u32::try_from)
            .transpose()?,
        duration_ms: u64::try_from(row.try_get::<i64, _>("duration_ms")?)?,
        sample_rate: u32::try_from(row.try_get::<i64, _>("sample_rate")?)?,
        channels: u16::try_from(row.try_get::<i64, _>("channels")?)?,
        bit_depth: row
            .try_get::<Option<i64>, _>("bit_depth")?
            .map(u32::try_from)
            .transpose()?,
        codec: row.try_get("codec")?,
    })
}
fn bind_metadata<'q>(
    query: sqlx::query::Query<'q, Sqlite, sqlx::sqlite::SqliteArguments>,
    meta: &TrackMetadata,
) -> Result<sqlx::query::Query<'q, Sqlite, sqlx::sqlite::SqliteArguments>> {
    Ok(query
        .bind(meta.title.clone())
        .bind(meta.artist.clone())
        .bind(meta.album.clone())
        .bind(meta.album_artist.clone())
        .bind(meta.track_number.map(i64::from))
        .bind(meta.disc_number.map(i64::from))
        .bind(i64::try_from(meta.duration_ms)?)
        .bind(i64::from(meta.sample_rate))
        .bind(i64::from(meta.channels))
        .bind(meta.bit_depth.map(i64::from))
        .bind(meta.codec.clone())
        .bind(meta.year.map(i64::from)))
}
