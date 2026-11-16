use crate::*;
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub enum PlaybackCommand {
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
    Seek { position_ms: u64, request_id: u64 },
    SetVolume(f32),
    SetDevice(Option<String>),
    PlayPlaylist(i64),
}

#[derive(Clone, Debug)]
pub enum LibraryCommand {
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
}

#[derive(Clone, Debug)]
pub enum AppCommand {
    Playback(PlaybackCommand),
    Library(LibraryCommand),
    Shutdown,
}
impl From<PlaybackCommand> for AppCommand {
    fn from(command: PlaybackCommand) -> Self {
        Self::Playback(command)
    }
}
impl From<LibraryCommand> for AppCommand {
    fn from(command: LibraryCommand) -> Self {
        Self::Library(command)
    }
}
