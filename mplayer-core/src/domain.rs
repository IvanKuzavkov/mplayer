use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

pub type Result<T> = anyhow::Result<T>;
pub type Wake = Arc<dyn Fn() + Send + Sync>;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TrackMetadata {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_artist: String,
    pub year: Option<u32>,
    pub track_number: Option<u32>,
    pub disc_number: Option<u32>,
    pub duration_ms: u64,
    pub sample_rate: u32,
    pub channels: u16,
    pub bit_depth: Option<u32>,
    pub codec: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    pub id: i64,
    pub path: PathBuf,
    pub metadata: TrackMetadata,
    pub missing: bool,
    pub segment: Option<TrackSegment>,
}

/// Bounds in source sample frames; the end is exclusive, or physical EOF.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrackSegment {
    pub start_frame: u64,
    pub end_frame: Option<u64>,
}

pub trait MetadataProbe: Send + Sync {
    fn probe(&self, path: &Path) -> Result<TrackMetadata>;

    fn artwork(&self, _path: &Path) -> Result<Option<Artwork>> {
        Ok(None)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Artwork {
    pub data: Arc<[u8]>,
    pub mime_type: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PlaybackState {
    #[default]
    Stopped,
    Buffering,
    Playing,
    Paused,
    Error,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PlayMode {
    #[default]
    Sequential,
    Repeat,
    Shuffle,
}

/// An output's stable selection label and its highest OS-reported PCM configuration.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OutputDevice {
    pub id: String,
    pub capabilities: Option<OutputDeviceCapabilities>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutputDeviceCapabilities {
    pub sample_rate: u32,
    pub bits_per_sample: u16,
    pub channels: u16,
    pub floating_point: bool,
}

impl From<String> for OutputDevice {
    fn from(id: String) -> Self {
        Self {
            id,
            capabilities: None,
        }
    }
}

impl From<&str> for OutputDevice {
    fn from(id: &str) -> Self {
        id.to_owned().into()
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlaybackSnapshot {
    /// Identifies the most recent explicit Load, excluding automatic next-track transitions.
    pub generation: u64,
    pub state: PlaybackState,
    pub track: Option<Track>,
    pub position_ms: u64,
    /// UI request acknowledged after a successful seek; zero is reserved for internal seeks.
    pub seek_request_id: u64,
    pub duration_ms: u64,
    pub source_rate: u32,
    pub output_rate: u32,
    pub device: String,
    pub selected_device: Option<String>,
    pub devices: Vec<OutputDevice>,
    pub volume: f32,
    pub underruns: u64,
    pub error: Option<String>,
    pub notification: Option<String>,
    /// Monotonic within a generation; increments after a track's final PCM frame is consumed.
    pub completed: u64,
}

#[derive(Clone, Debug)]
#[allow(
    clippy::large_enum_variant,
    reason = "bounded control messages stay inline to avoid an allocation for every load"
)]
pub enum AudioCommand {
    Load {
        track: Track,
        next: Option<Track>,
        generation: u64,
    },
    SetNext(Option<Track>),
    Pause,
    Resume,
    Stop {
        generation: u64,
    },
    Seek {
        position_ms: u64,
        request_id: u64,
    },
    SetVolume(f32),
    SetDevice(Option<String>),
    Shutdown,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct LibraryPage {
    pub tracks: Arc<Vec<Track>>,
    pub total: u64,
    pub offset: u64,
    pub query: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AlbumKey {
    pub title: String,
    pub artist: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AlbumSummary {
    pub key: AlbumKey,
    pub year: Option<u32>,
    pub track_count: u64,
    pub duration_ms: u64,
    pub artwork_path: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderSummary {
    pub id: i64,
    pub generation: i64,
    pub status: ScanStatus,
    pub error: Option<String>,
    pub path: PathBuf,
    pub track_count: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, sqlx::Type)]
#[sqlx(rename_all = "lowercase")]
pub enum ScanStatus {
    Pending,
    Scanning,
    Ready,
    Error,
    Cancelled,
    Missing,
}

impl std::fmt::Display for ScanStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Pending => "Pending",
            Self::Scanning => "Scanning",
            Self::Ready => "Ready",
            Self::Error => "Error",
            Self::Cancelled => "Cancelled",
            Self::Missing => "Missing",
        })
    }
}

#[derive(Clone, Debug)]
pub enum ScanRequest {
    Library,
    Roots(Vec<PathBuf>),
    Folder(i64),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum LibraryFilter {
    #[default]
    All,
    Folder(PathBuf),
    Album(AlbumKey),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FolderPage {
    pub folders: Arc<Vec<FolderSummary>>,
    pub total: u64,
    pub offset: u64,
    pub query: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AlbumPage {
    pub albums: Arc<Vec<AlbumSummary>>,
    pub total: u64,
    pub offset: u64,
    pub query: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Playlist {
    pub id: i64,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlaylistEntry {
    pub id: i64,
    pub track: Track,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScanProgress {
    pub running: bool,
    pub discovered: u64,
    pub processed: u64,
    pub errors: u64,
    /// Changes at folder status boundaries, even when watch coalesces snapshots.
    pub folder_revision: u64,
}

#[derive(Clone, Debug)]
pub struct AppSnapshot {
    pub library: LibraryPage,
    pub library_filter: LibraryFilter,
    pub folders: FolderPage,
    pub albums: AlbumPage,
    pub roots: Vec<PathBuf>,
    pub playlists: Vec<Playlist>,
    pub selected_playlist: Option<i64>,
    pub playlist_entries: Arc<Vec<PlaylistEntry>>,
    pub queue: Arc<Vec<Track>>,
    pub queue_index: Option<usize>,
    pub play_mode: PlayMode,
    pub playback: PlaybackSnapshot,
    pub scan: ScanProgress,
    pub queued_imports: usize,
    pub artwork: Option<Artwork>,
    pub status: String,
}

impl Default for AppSnapshot {
    fn default() -> Self {
        Self {
            library: LibraryPage::default(),
            library_filter: LibraryFilter::default(),
            folders: FolderPage::default(),
            albums: AlbumPage::default(),
            roots: Vec::new(),
            playlists: Vec::new(),
            selected_playlist: None,
            playlist_entries: Arc::default(),
            queue: Arc::default(),
            queue_index: None,
            play_mode: PlayMode::default(),
            playback: PlaybackSnapshot {
                volume: 1.0,
                ..Default::default()
            },
            scan: ScanProgress::default(),
            queued_imports: 0,
            artwork: None,
            status: "Ready".into(),
        }
    }
}

#[derive(Clone, Debug)]
pub enum AppCommand {
    Import(Vec<PathBuf>),
    RemoveScanFolder(PathBuf),
    Rescan,
    RescanFolder(i64),
    CancelScan,
    Search {
        query: String,
        offset: u64,
    },
    BrowseLibrary {
        filter: LibraryFilter,
        query: String,
        offset: u64,
    },
    ListFolders {
        query: String,
        offset: u64,
    },
    ListAlbums {
        query: String,
        offset: u64,
    },
    Play(Track),
    Enqueue(Track),
    EnqueueAlbum(AlbumKey),
    PlayAlbum(AlbumKey),
    RemoveQueue(usize),
    ClearQueue,
    PlayQueue(usize),
    PauseResume,
    Stop,
    Previous,
    Next,
    SetPlayMode(PlayMode),
    Seek {
        position_ms: u64,
        request_id: u64,
    },
    SetVolume(f32),
    SetDevice(Option<String>),
    CreatePlaylist(String),
    DeletePlaylist(i64),
    SelectPlaylist(Option<i64>),
    AddToPlaylist {
        playlist_id: i64,
        track_id: i64,
    },
    RemovePlaylistEntry(i64),
    MovePlaylistEntry {
        entry_id: i64,
        position: usize,
    },
    PlayPlaylist(i64),
    Shutdown,
}

pub const PAGE_SIZE: u64 = 256;
