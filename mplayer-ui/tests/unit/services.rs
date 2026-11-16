use super::*;
use mplayer_core::{
    ArtworkRequest, ArtworkSnapshot, LibraryClient, LibraryRequest, LibrarySnapshot,
    PlayerSnapshot, SettingsSnapshot,
};
use tokio::sync::watch;

pub(super) fn command_channel(capacity: usize) -> (CommandRouter, Commands) {
    let (playback, player) = mpsc::channel(capacity);
    let (library, catalog) = mpsc::channel(capacity);
    let (shutdown, _) = watch::channel(false);
    (
        CommandRouter::new(playback, LibraryClient(library), shutdown),
        Commands { player, catalog },
    )
}
pub(super) struct Commands {
    player: mpsc::Receiver<CommandEnvelope<PlaybackCommand>>,
    catalog: mpsc::Receiver<CommandEnvelope<LibraryRequest>>,
}
impl Commands {
    pub(super) fn try_recv(&mut self) -> Result<AppCommand, mpsc::error::TryRecvError> {
        if let Ok(command) = self.player.try_recv() {
            return Ok(AppCommand::Playback(command.payload));
        }
        self.catalog.try_recv().map(|command| {
            let LibraryRequest::Command(command) = command.payload else {
                panic!("unexpected lookup")
            };
            AppCommand::Library(command)
        })
    }
}

pub(super) struct Snapshots {
    value: std::cell::RefCell<AppSnapshot>,
    player: watch::Sender<PlayerSnapshot>,
    library: watch::Sender<LibrarySnapshot>,
    artwork: watch::Sender<ArtworkSnapshot>,
    _settings: watch::Sender<SettingsSnapshot>,
}
pub(super) fn snapshot_channel(value: AppSnapshot) -> (Snapshots, UiSubscriptions) {
    let (player, p) = watch::channel(PlayerSnapshot::default());
    let (library, l) = watch::channel(LibrarySnapshot::default());
    let (artwork, a) = watch::channel(ArtworkSnapshot::default());
    let (settings, s) = watch::channel(SettingsSnapshot::default());
    let sender = Snapshots {
        value: std::cell::RefCell::new(AppSnapshot::default()),
        player,
        library,
        artwork,
        _settings: settings,
    };
    sender.send_replace(value);
    (
        sender,
        UiSubscriptions {
            player: p,
            library: l,
            artwork: a,
            settings: s,
        },
    )
}
impl Snapshots {
    pub(super) fn send_replace(&self, value: AppSnapshot) {
        self.player.send_replace(PlayerSnapshot {
            queue: value.queue.clone(),
            queue_index: value.queue_index,
            play_mode: value.play_mode,
            playback: value.playback.clone(),
            status: value.status.clone(),
            origin: None,
            artwork_revision: 0,
        });
        self.library.send_replace(LibrarySnapshot {
            library: value.library.clone(),
            library_filter: value.library_filter.clone(),
            folders: value.folders.clone(),
            albums: value.albums.clone(),
            roots: value.roots.clone(),
            playlists: value.playlists.clone(),
            selected_playlist: value.selected_playlist,
            playlist_entries: value.playlist_entries.clone(),
            scan: value.scan.clone(),
            queued_imports: value.queued_imports,
            status: value.status.clone(),
        });
        self.artwork.send_replace(ArtworkSnapshot {
            revision: 0,
            path: value
                .playback
                .track
                .as_ref()
                .map(|track| track.path.clone()),
            artwork: value.artwork.clone(),
        });
        *self.value.borrow_mut() = value;
    }
    pub(super) fn send_modify(&self, modify: impl FnOnce(&mut AppSnapshot)) {
        let mut value = self.value.borrow().clone();
        modify(&mut value);
        self.send_replace(value);
    }
}

pub(super) fn artwork_client(probe: Arc<dyn mplayer_core::MetadataProbe>) -> ArtworkClient {
    let (sender, mut requests) = mpsc::channel::<CommandEnvelope<ArtworkRequest>>(64);
    tokio::spawn(async move {
        while let Some(request) = requests.recv().await {
            let probe = probe.clone();
            tokio::task::spawn_blocking(move || {
                let result = probe.artwork(&request.payload.path);
                let _ = request.payload.reply.send(result);
            });
        }
    });
    ArtworkClient(sender)
}

#[test]
fn library_backpressure_does_not_block_playback_or_change_retry_identity() {
    let (router, mut commands) = command_channel(1);
    router
        .try_send(router.prepare(LibraryCommand::Rescan))
        .unwrap();
    let (_state, subscriptions) = snapshot_channel(populated());
    let mut app = PlayerApp::new(router, subscriptions);
    assert!(app.submit(LibraryCommand::CreatePlaylist("queued".into())));
    let id = app.library_outbox.pending.front().unwrap().trace.id;
    app.library_outbox.flush(&app.commands);
    assert_eq!(app.library_outbox.pending.front().unwrap().trace.id, id);
    assert!(app.submit(PlaybackCommand::PauseResume));
    assert!(matches!(
        commands.player.try_recv().unwrap().payload,
        PlaybackCommand::PauseResume
    ));
    commands.catalog.try_recv().unwrap();
    app.library_outbox.flush(&app.commands);
    assert_eq!(commands.catalog.try_recv().unwrap().trace.id, id);
}

#[test]
fn a_closed_library_disables_only_library_controls() {
    let (router, commands) = command_channel(1);
    let (state, subscriptions) = snapshot_channel(populated());
    let mut app = PlayerApp::new(router, subscriptions);
    drop(commands.catalog);
    drop(state.library);
    app.refresh(&egui::Context::default());
    assert!(!app.available());
    assert!(app.playback_available());
    assert!(app.submit(PlaybackCommand::PauseResume));
}

#[test]
fn clear_queue_cannot_restore_a_cover_from_an_older_subscription() {
    let (router, _commands) = command_channel(1);
    let (state, subscriptions) = snapshot_channel(populated());
    state.artwork.send_modify(|state| {
        state.artwork = Some(mplayer_core::Artwork {
            data: vec![1].into(),
            mime_type: "test".into(),
        })
    });
    let mut app = PlayerApp::new(router, subscriptions);
    assert!(app.snapshot.artwork.is_some());
    state.player.send_modify(|state| {
        state.playback.track = None;
        state.artwork_revision += 1;
    });
    app.refresh(&egui::Context::default());
    assert!(app.snapshot.artwork.is_none());
    state
        .player
        .send_modify(|state| state.playback.track = Some(track(2)));
    app.refresh(&egui::Context::default());
    assert!(app.snapshot.artwork.is_none());
    state.artwork.send_modify(|state| {
        state.revision = 1;
        state.artwork.as_mut().unwrap().data = vec![2].into();
    });
    app.refresh(&egui::Context::default());
    assert_eq!(app.snapshot.artwork.as_ref().unwrap().data.as_ref(), &[2]);
}
