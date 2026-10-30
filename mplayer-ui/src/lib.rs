//! Desktop presentation with a persistent now-playing panel and a paged library.
mod artwork;
mod folders;
mod icon_buttons;
mod library;
mod list_rows;
mod navigation;
mod player;
mod playlists;
mod theme;
mod thumbnails;

use std::{collections::VecDeque, sync::Arc, time::Duration};

pub use eframe;
use eframe::egui::{self, RichText};
use mplayer_core::{AppCommand, AppSnapshot, LibraryFilter, PlayMode, PlaybackState, Track};
use tokio::sync::{mpsc, watch};

const MAX_PENDING: usize = 64;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum View {
    #[default]
    Library,
    ScanFolders,
    Queue,
    Playlists,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
enum LibraryTab {
    #[default]
    Tracks,
    Folders,
    Albums,
}

#[derive(Default)]
struct Outbox {
    pending: VecDeque<AppCommand>,
    disconnected: bool,
}
impl Outbox {
    fn push(&mut self, command: AppCommand) -> bool {
        if self.disconnected {
            return false;
        }
        if let Some(previous) = self.pending.back_mut()
            && coalesces(previous, &command)
        {
            *previous = command;
            return true;
        }
        if self.pending.len() >= MAX_PENDING {
            return false;
        }
        self.pending.push_back(command);
        true
    }
    fn flush(&mut self, sender: &mpsc::Sender<AppCommand>) {
        while let Some(command) = self.pending.pop_front() {
            match sender.try_send(command) {
                Ok(()) => (),
                Err(mpsc::error::TrySendError::Full(command)) => {
                    self.pending.push_front(command);
                    break;
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    self.disconnected = true;
                    self.pending.clear();
                    break;
                }
            }
        }
    }
}
fn coalesces(previous: &AppCommand, next: &AppCommand) -> bool {
    if let (
        AppCommand::BrowseLibrary {
            filter: previous, ..
        },
        AppCommand::BrowseLibrary { filter: next, .. },
    ) = (previous, next)
    {
        return previous == next;
    }
    matches!(
        (previous, next),
        (AppCommand::Seek { .. }, AppCommand::Seek { .. })
            | (AppCommand::SetVolume(_), AppCommand::SetVolume(_))
            | (AppCommand::Search { .. }, AppCommand::Search { .. })
            | (
                AppCommand::ListFolders { .. },
                AppCommand::ListFolders { .. }
            )
            | (AppCommand::ListAlbums { .. }, AppCommand::ListAlbums { .. })
    )
}

pub struct PlayerApp {
    commands: mpsc::Sender<AppCommand>,
    state: watch::Receiver<AppSnapshot>,
    snapshot: Arc<AppSnapshot>,
    outbox: Outbox,
    view: View,
    queue_return_view: View,
    tab: LibraryTab,
    group: Option<LibraryFilter>,
    search: String,
    playlist_name: String,
    rejected_playlist_name: Option<String>,
    message: Option<String>,
    seek_draft: Option<f64>,
    pending_seek: Option<(u64, u64)>,
    seek_request_id: u64,
    initialized: bool,
    folders: folders::FolderPicker,
    cover: artwork::Cover,
    thumbnails: thumbnails::Thumbnails,
    #[cfg(test)]
    library_scroll: Option<egui::Rect>,
    #[cfg(test)]
    output_button: Option<egui::Rect>,
    #[cfg(test)]
    seek_slider: Option<egui::Rect>,
    #[cfg(test)]
    volume_slider: Option<egui::Rect>,
    #[cfg(test)]
    volume_icon: Option<egui::Rect>,
    #[cfg(test)]
    transport_rect: Option<egui::Rect>,
    #[cfg(test)]
    queue_buttons: Vec<egui::Rect>,
    #[cfg(test)]
    scan_actions: [Option<egui::Rect>; 2],
    #[cfg(test)]
    queue_button: Option<egui::Rect>,
    #[cfg(test)]
    mode_button: Option<egui::Rect>,
    #[cfg(test)]
    back_button: Option<egui::Rect>,
    #[cfg(test)]
    clear_button: Option<egui::Rect>,
    #[cfg(test)]
    album_cover: Option<egui::Rect>,
    #[cfg(test)]
    folder_actions: Vec<[egui::Rect; 2]>,
    #[cfg(test)]
    playlist_create: Option<egui::Rect>,
    #[cfg(test)]
    playlist_play: Option<egui::Rect>,
    #[cfg(test)]
    playlist_remove: Vec<egui::Rect>,
    #[cfg(test)]
    playlist_rows: Vec<egui::Rect>,
    #[cfg(test)]
    scan_remove: Vec<egui::Rect>,
    #[cfg(test)]
    album_artworks: Vec<egui::Rect>,
    #[cfg(test)]
    album_action_buttons: Vec<[egui::Rect; 2]>,
    #[cfg(test)]
    transport_buttons: [Option<egui::Rect>; 3],
}
impl PlayerApp {
    pub fn new(commands: mpsc::Sender<AppCommand>, state: watch::Receiver<AppSnapshot>) -> Self {
        let snapshot = Arc::new(state.borrow().clone());
        Self {
            search: snapshot.library.query.clone(),
            seek_request_id: snapshot.playback.seek_request_id,
            snapshot,
            commands,
            state,
            outbox: Outbox::default(),
            view: View::Library,
            queue_return_view: View::Library,
            tab: LibraryTab::Tracks,
            group: None,
            playlist_name: String::new(),
            rejected_playlist_name: None,
            message: None,
            seek_draft: None,
            pending_seek: None,
            initialized: false,
            folders: folders::FolderPicker::default(),
            cover: artwork::Cover::default(),
            thumbnails: thumbnails::Thumbnails::default(),
            #[cfg(test)]
            library_scroll: None,
            #[cfg(test)]
            output_button: None,
            #[cfg(test)]
            seek_slider: None,
            #[cfg(test)]
            volume_slider: None,
            #[cfg(test)]
            volume_icon: None,
            #[cfg(test)]
            transport_rect: None,
            #[cfg(test)]
            queue_buttons: Vec::new(),
            #[cfg(test)]
            scan_actions: [None; 2],
            #[cfg(test)]
            queue_button: None,
            #[cfg(test)]
            mode_button: None,
            #[cfg(test)]
            back_button: None,
            #[cfg(test)]
            clear_button: None,
            #[cfg(test)]
            album_cover: None,
            #[cfg(test)]
            folder_actions: Vec::new(),
            #[cfg(test)]
            playlist_create: None,
            #[cfg(test)]
            playlist_play: None,
            #[cfg(test)]
            playlist_remove: Vec::new(),
            #[cfg(test)]
            playlist_rows: Vec::new(),
            #[cfg(test)]
            scan_remove: Vec::new(),
            #[cfg(test)]
            album_artworks: Vec::new(),
            #[cfg(test)]
            album_action_buttons: Vec::new(),
            #[cfg(test)]
            transport_buttons: [None; 3],
        }
    }
    pub fn with_artwork_probe(mut self, probe: Arc<dyn mplayer_core::MetadataProbe>) -> Self {
        self.thumbnails.probe = Some(probe);
        self
    }
    fn available(&self) -> bool {
        !self.outbox.disconnected && self.outbox.pending.len() < MAX_PENDING
    }

    fn toggle_queue(&mut self) {
        if self.view == View::Queue {
            self.view = self.queue_return_view;
        } else {
            self.queue_return_view = self.view;
            self.view = View::Queue;
        }
    }

    #[tracing::instrument(skip_all, level="debug", fields(command = ?command))]
    fn submit(&mut self, command: AppCommand) -> bool {
        let changes_track = matches!(
            &command,
            AppCommand::Play(_)
                | AppCommand::PlayQueue(_)
                | AppCommand::PlayPlaylist(_)
                | AppCommand::Previous
                | AppCommand::Next
                | AppCommand::Stop
                | AppCommand::ClearQueue
        );
        if !self.outbox.push(command) {
            self.message = Some(
                if self.outbox.disconnected {
                    "The player service has stopped. Restart the player."
                } else {
                    "The player is busy. Please retry shortly."
                }
                .into(),
            );
            return false;
        }
        if changes_track {
            self.seek_draft = None;
            self.pending_seek = None;
        }
        self.outbox.flush(&self.commands);
        true
    }
    fn refresh(&mut self, context: &egui::Context) {
        let changed = self.state.has_changed();
        let closed = changed.is_err();
        if changed.unwrap_or(true) {
            let next = self.state.borrow_and_update().clone();
            if !next.scan.running
                && (self.snapshot.scan.running
                    || self.snapshot.scan.folder_revision != next.scan.folder_revision)
            {
                self.thumbnails.invalidate();
            }
            let previous = &self.snapshot.playback;
            let playback = &next.playback;
            let changed_track = previous.generation != playback.generation
                || previous.completed != playback.completed
                || previous.track.as_ref().map(|track| track.id)
                    != playback.track.as_ref().map(|track| track.id);
            let stopped = matches!(
                playback.state,
                PlaybackState::Stopped | PlaybackState::Error
            );
            if changed_track || stopped {
                self.seek_draft = None;
            }
            if changed_track
                || stopped
                || self
                    .pending_seek
                    .is_some_and(|(request_id, _)| request_id == playback.seek_request_id)
            {
                self.pending_seek = None;
            }
            self.snapshot = Arc::new(next);
        }
        if closed || self.commands.is_closed() {
            self.outbox.disconnected = true;
            self.outbox.pending.clear();
            self.seek_draft = None;
            self.pending_seek = None;
        }
        self.outbox.flush(&self.commands);
        if !self.outbox.pending.is_empty() {
            context.request_repaint_after(Duration::from_millis(30));
        }
        self.cover.update(self.snapshot.artwork.as_ref(), context);
    }
    fn show(&mut self, ui: &mut egui::Ui) {
        self.thumbnails.begin_frame(ui.ctx());
        #[cfg(test)]
        {
            self.queue_buttons.clear();
            self.back_button = None;
            self.clear_button = None;
            self.album_cover = None;
            self.folder_actions.clear();
            self.playlist_create = None;
            self.playlist_play = None;
            self.playlist_remove.clear();
            self.playlist_rows.clear();
            self.scan_remove.clear();
            self.album_artworks.clear();
            self.album_action_buttons.clear();
        }
        if !self.initialized {
            theme::apply(ui.ctx());
            ui.set_style(ui.ctx().style_of(ui.ctx().theme()));
            self.initialized = true;
        }
        self.refresh(ui.ctx());
        let dropped: Vec<_> = ui.input(|input| {
            input
                .raw
                .dropped_files
                .iter()
                .map(|file| file.path().to_owned())
                .collect()
        });
        if !dropped.is_empty() {
            self.submit(AppCommand::Import(dropped));
        }
        if !self.folders.is_open()
            && !ui.ctx().egui_wants_keyboard_input()
            && ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Space))
        {
            self.submit(AppCommand::PauseResume);
        }
        let state = Arc::clone(&self.snapshot);
        egui::Panel::left("navigation")
            .exact_size(205.0)
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(ui.visuals().extreme_bg_color)
                    .inner_margin(16),
            )
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("sidebar")
                    .auto_shrink([false, false])
                    .show(ui, |ui| self.sidebar(ui));
            });
        egui::Panel::right("now_playing")
            .exact_size(318.0)
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(ui.visuals().extreme_bg_color)
                    .inner_margin(20),
            )
            .show(ui, |ui| {
                self.output_controls(ui, &state);
                egui::Panel::bottom("playback_controls")
                    .exact_size(140.0)
                    .show_separator_line(false)
                    .frame(egui::Frame::NONE)
                    .show(ui, |ui| self.playback_controls(ui, &state));
                // Keep messages above the controls without shifting the artwork or metadata.
                egui::Panel::bottom("playback_message")
                    .exact_size(2.0 * ui.text_style_height(&egui::TextStyle::Small) + 24.0)
                    .show_separator_line(false)
                    .frame(egui::Frame::NONE)
                    .show(ui, |ui| self.playback_message(ui, &state));
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE)
                    .show(ui, |ui| {
                        let available_height = ui.available_height();
                        self.player(ui, &state, available_height);
                    });
            });
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(ui.visuals().panel_fill)
                    .inner_margin(20),
            )
            .show(ui, |ui| match self.view {
                View::Library => self.library(ui, &state),
                View::ScanFolders => self.scan_folders(ui, &state),
                View::Queue => self.queue(ui, &state),
                View::Playlists => self.playlists(ui, &state),
            });
        if let Some(paths) = self.folders.show(ui.ctx(), self.available())
            && self.submit(AppCommand::Import(paths))
        {
            self.folders.close();
        }
        if let Some(error) = self.folders.take_error() {
            self.message = Some(error);
        }
        self.thumbnails.finish_frame(ui.ctx());
    }
}
impl eframe::App for PlayerApp {
    fn logic(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        self.refresh(context);
    }
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.show(ui);
        self.folders.launch(frame, ui.ctx());
    }
}
fn track_title(track: &Track) -> String {
    if track.metadata.title.trim().is_empty() {
        track
            .path
            .file_stem()
            .unwrap_or(track.path.as_os_str())
            .to_string_lossy()
            .into_owned()
    } else {
        track.metadata.title.clone()
    }
}

#[cfg(test)]
#[path = "../tests/unit/ui.rs"]
mod tests;
