CREATE TABLE scan_folders (
    id INTEGER PRIMARY KEY,
    path BLOB NOT NULL UNIQUE,
    generation INTEGER NOT NULL DEFAULT 0,
    status TEXT NOT NULL DEFAULT 'pending' CHECK(status IN ('pending','scanning','ready','error','cancelled','missing')),
    error TEXT
);
CREATE TABLE folders (
    id INTEGER PRIMARY KEY,
    scan_folder_id INTEGER NOT NULL REFERENCES scan_folders(id),
    path BLOB NOT NULL UNIQUE,
    display_path TEXT NOT NULL,
    generation INTEGER NOT NULL DEFAULT 0,
    status TEXT NOT NULL DEFAULT 'pending' CHECK(status IN ('pending','scanning','ready','error','cancelled','missing')),
    error TEXT
);
CREATE INDEX folders_root ON folders(scan_folder_id);
CREATE TABLE media_tracks (
    id INTEGER PRIMARY KEY,
    folder_id INTEGER NOT NULL REFERENCES folders(id),
    path BLOB NOT NULL UNIQUE,
    modified_secs INTEGER NOT NULL,
    file_size INTEGER NOT NULL CHECK(file_size >= 0),
    metadata_version INTEGER NOT NULL DEFAULT 1,
    title TEXT NOT NULL,
    artist TEXT NOT NULL,
    album TEXT NOT NULL,
    album_artist TEXT NOT NULL DEFAULT '',
    track_number INTEGER,
    disc_number INTEGER,
    duration_ms INTEGER NOT NULL CHECK(duration_ms >= 0),
    sample_rate INTEGER NOT NULL,
    channels INTEGER NOT NULL,
    bit_depth INTEGER,
    codec TEXT NOT NULL,
    missing INTEGER NOT NULL DEFAULT 0 CHECK(missing IN (0,1)),
    year INTEGER CHECK(year BETWEEN 1 AND 9999)
);
CREATE INDEX media_folder ON media_tracks(folder_id);
CREATE TABLE tracks (
    id INTEGER PRIMARY KEY,
    media_id INTEGER NOT NULL REFERENCES media_tracks(id),
    logical_index INTEGER NOT NULL CHECK(logical_index BETWEEN 0 AND 99),
    cue_path BLOB,
    start_frame INTEGER CHECK(start_frame >= 0),
    end_frame INTEGER CHECK(end_frame > start_frame),
    title TEXT NOT NULL,
    artist TEXT NOT NULL,
    album TEXT NOT NULL,
    album_artist TEXT NOT NULL DEFAULT '',
    track_number INTEGER,
    disc_number INTEGER,
    duration_ms INTEGER NOT NULL CHECK(duration_ms >= 0),
    sample_rate INTEGER NOT NULL,
    channels INTEGER NOT NULL,
    bit_depth INTEGER,
    codec TEXT NOT NULL,
    missing INTEGER NOT NULL DEFAULT 0 CHECK(missing IN (0,1)),
    year INTEGER CHECK(year BETWEEN 1 AND 9999),
    UNIQUE(media_id, logical_index),
    CHECK((logical_index = 0 AND cue_path IS NULL AND start_frame IS NULL AND end_frame IS NULL)
       OR (logical_index > 0 AND cue_path IS NOT NULL AND start_frame IS NOT NULL))
);
CREATE INDEX tracks_title ON tracks(title COLLATE NOCASE, id);
CREATE INDEX tracks_album ON tracks(album, COALESCE(NULLIF(album_artist, ''), artist), disc_number, track_number, title COLLATE NOCASE, id);

CREATE TABLE settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE playlists (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL CHECK(length(trim(name)) > 0)
);

CREATE TABLE playlist_entries (
    id INTEGER PRIMARY KEY,
    playlist_id INTEGER NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
    track_id INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
    position INTEGER NOT NULL CHECK(position >= 0)
);
CREATE INDEX playlist_order ON playlist_entries(playlist_id, position, id);
