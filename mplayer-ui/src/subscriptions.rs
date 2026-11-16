use eframe::egui;
use mplayer_core::{AppSnapshot, UiSubscriptions};

pub(super) fn changed(state: &UiSubscriptions) -> bool {
    state.player.has_changed().unwrap_or(true)
        || state.library.has_changed().unwrap_or(true)
        || state.artwork.has_changed().unwrap_or(true)
        || state.settings.has_changed().unwrap_or(true)
}

pub(super) fn snapshot(state: &mut UiSubscriptions) -> AppSnapshot {
    let player = state.player.borrow_and_update();
    let library = state.library.borrow_and_update();
    let artwork = state.artwork.borrow_and_update();
    state.settings.borrow_and_update();
    AppSnapshot {
        library: library.library.clone(),
        library_filter: library.library_filter.clone(),
        folders: library.folders.clone(),
        albums: library.albums.clone(),
        roots: library.roots.clone(),
        playlists: library.playlists.clone(),
        selected_playlist: library.selected_playlist,
        playlist_entries: library.playlist_entries.clone(),
        scan: library.scan.clone(),
        queued_imports: library.queued_imports,
        queue: player.queue.clone(),
        queue_index: player.queue_index,
        play_mode: player.play_mode,
        playback: player.playback.clone(),
        artwork: player.playback.track.as_ref().and_then(|_| {
            (player.artwork_revision == artwork.revision)
                .then(|| artwork.artwork.clone())
                .flatten()
        }),
        status: if library.status.is_empty() {
            player.status.clone()
        } else {
            library.status.clone()
        },
    }
}

/// Only the UI adapter knows how service updates wake the native window.
pub(super) async fn repaint(mut state: UiSubscriptions, context: egui::Context) {
    let (mut player, mut library, mut artwork, mut settings) = (true, true, true, true);
    loop {
        tokio::select! {
            changed = state.player.changed(), if player => player = changed.is_ok(),
            changed = state.library.changed(), if library => library = changed.is_ok(),
            changed = state.artwork.changed(), if artwork => artwork = changed.is_ok(),
            changed = state.settings.changed(), if settings => settings = changed.is_ok(),
            else => break,
        }
        context.request_repaint();
    }
}
