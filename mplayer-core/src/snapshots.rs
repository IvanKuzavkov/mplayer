use crate::*;
use std::{path::PathBuf, sync::Arc};
use tokio::sync::watch;

#[derive(Clone, Debug, PartialEq)]
pub struct PlayerSnapshot {
    pub queue: Arc<Vec<Track>>,
    pub queue_index: Option<usize>,
    pub play_mode: PlayMode,
    pub playback: PlaybackSnapshot,
    pub status: String,
    pub origin: Option<TraceContext>,
    /// Retains a cover reset even when subscribers miss an intermediate Stop.
    pub artwork_revision: u64,
}
impl Default for PlayerSnapshot {
    fn default() -> Self {
        Self {
            queue: Arc::default(),
            queue_index: None,
            play_mode: PlayMode::default(),
            playback: PlaybackSnapshot {
                volume: 1.0,
                ..Default::default()
            },
            status: "Ready".into(),
            origin: None,
            artwork_revision: 0,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct LibrarySnapshot {
    pub library: LibraryPage,
    pub library_filter: LibraryFilter,
    pub folders: FolderPage,
    pub albums: AlbumPage,
    pub roots: Vec<PathBuf>,
    pub playlists: Vec<Playlist>,
    pub selected_playlist: Option<i64>,
    pub playlist_entries: Arc<Vec<PlaylistEntry>>,
    pub scan: ScanProgress,
    pub queued_imports: usize,
    pub status: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ArtworkSnapshot {
    pub revision: u64,
    pub path: Option<PathBuf>,
    pub artwork: Option<Artwork>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SettingsSnapshot {
    pub volume: f32,
    pub device: Option<String>,
    pub error: Option<String>,
}
impl Default for SettingsSnapshot {
    fn default() -> Self {
        Self {
            volume: 1.0,
            device: None,
            error: None,
        }
    }
}

#[derive(Clone)]
pub struct SettingsUpdate {
    pub volume: CommandEnvelope<f32>,
    pub device: CommandEnvelope<Option<String>>,
}

#[derive(Clone)]
pub struct UiSubscriptions {
    pub player: watch::Receiver<PlayerSnapshot>,
    pub library: watch::Receiver<LibrarySnapshot>,
    pub artwork: watch::Receiver<ArtworkSnapshot>,
    pub settings: watch::Receiver<SettingsSnapshot>,
}
