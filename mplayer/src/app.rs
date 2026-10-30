use anyhow::{Context, bail};
use mplayer_core::*;
use std::{
    collections::VecDeque,
    hash::{BuildHasher, RandomState},
    path::PathBuf,
    sync::{Arc, mpsc::SyncSender},
    time::{Duration, Instant},
};
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
};
use tracing_appender::non_blocking::ErrorCounter;

pub struct Application {
    store: Store,
    audio: SyncSender<AudioCommand>,
    pending_audio: VecDeque<AudioCommand>,
    pending_playback_update: bool,
    playback: watch::Receiver<PlaybackSnapshot>,
    commands: mpsc::Receiver<AppCommand>,
    output: watch::Sender<AppSnapshot>,
    state: AppSnapshot,
    wake: Wake,
    probe: Arc<dyn MetadataProbe>,
    scan_cancel: Option<ScanCancellationToken>,
    scan_task: Option<JoinHandle<Result<()>>>,
    scan_progress: Option<watch::Receiver<ScanProgress>>,
    pending_imports: Vec<PathBuf>,
    artwork_track: Option<PathBuf>,
    artwork_loaded: Option<PathBuf>,
    artwork_reading: Option<PathBuf>,
    artwork_task: Option<JoinHandle<(PathBuf, Result<Option<Artwork>>)>>,
    completed: u64,
    generation: u64,
    next_queue_index: Option<usize>,
    shuffle_random: RandomState,
    shuffle_counter: u64,
    volume_dirty: bool,
    requested_volume: f32,
    last_settings_write: Instant,
    dropped_logs: ErrorCounter,
}

impl Application {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        store: Store,
        audio: SyncSender<AudioCommand>,
        playback: watch::Receiver<PlaybackSnapshot>,
        commands: mpsc::Receiver<AppCommand>,
        output: watch::Sender<AppSnapshot>,
        wake: Wake,
        probe: Arc<dyn MetadataProbe>,
        dropped_logs: ErrorCounter,
    ) -> Self {
        Self {
            store,
            audio,
            pending_audio: VecDeque::new(),
            pending_playback_update: false,
            playback,
            commands,
            output,
            state: AppSnapshot::default(),
            wake,
            probe,
            scan_cancel: None,
            scan_task: None,
            scan_progress: None,
            pending_imports: Vec::new(),
            artwork_track: None,
            artwork_loaded: None,
            artwork_reading: None,
            artwork_task: None,
            completed: 0,
            generation: 0,
            next_queue_index: None,
            shuffle_random: RandomState::new(),
            shuffle_counter: 0,
            volume_dirty: false,
            requested_volume: 1.0,
            last_settings_write: Instant::now(),
            dropped_logs,
        }
    }

    #[tracing::instrument(skip_all, err)]
    pub async fn run(mut self) -> Result<()> {
        if let Err(error) = async {
            self.restore().await?;
            self.refresh().await
        }
        .await
        {
            self.state.status = format!("Initialization failed: {error}");
            self.state.playback.state = PlaybackState::Error;
            self.state.playback.error = Some(self.state.status.clone());
            self.publish();
            return Err(error);
        }
        self.publish();
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut last_dropped = 0;
        loop {
            tokio::select! {
                command = self.commands.recv() => {
                    match command {
                        Some(AppCommand::Shutdown) | None => break,
                        Some(command) => {
                            if let Err(error) = self.handle(command).await {
                                tracing::warn!(%error, "application command failed");
                                self.state.status = error.to_string();
                            }
                            self.publish();
                        }
                    }
                }
                changed = self.playback.changed() => {
                    if changed.is_err() { break; }
                    if let Err(error) = self.update_playback() {
                        self.pending_playback_update = true;
                        tracing::warn!(%error, "cannot advance playback");
                        self.state.status = error.to_string();
                    }
                    self.publish();
                }
                _ = tick.tick() => {
                    let mut changed = false;
                    if self.artwork_task.as_ref().is_some_and(JoinHandle::is_finished) {
                        self.finish_artwork().await;
                        changed = true;
                    }
                    if let Err(error) = self.flush_audio() {
                        self.state.status = error.to_string();
                        changed = true;
                    }
                    if self.pending_playback_update {
                        self.pending_playback_update = self.update_playback().is_err();
                        changed = true;
                    }
                    if let Some(progress) = &mut self.scan_progress && progress.has_changed().unwrap_or(false) {
                        let previous = self.state.scan.folder_revision;
                        self.state.scan = progress.borrow_and_update().clone();
                        if previous != self.state.scan.folder_revision {
                            self.state.folders = self.store.folders(&self.state.folders.query, self.state.folders.offset).await?;
                        }
                        changed = true;
                    }
                    if self.scan_task.as_ref().is_some_and(JoinHandle::is_finished) {
                        let result = self.scan_task.take().expect("finished scan exists").await;
                        if let Some(progress) = &self.scan_progress {
                            self.state.scan = progress.borrow().clone();
                        }
                        self.state.scan.running = false;
                        self.scan_progress = None;
                        self.state.status = match result {
                            Ok(Ok(())) => format!("Scan complete: {} files, {} errors", self.state.scan.processed, self.state.scan.errors),
                            Ok(Err(_)) if self.scan_cancel.is_none() => "Scan cancelled".into(),
                            Ok(Err(error)) => format!("Scan failed: {error}"),
                            Err(error) => format!("Scan task failed: {error}"),
                        };
                        self.scan_cancel = None;
                        if let Err(error) = self.refresh().await {
                            self.state.status = error.to_string(); tracing::error!(%error, "refresh after scan failed");
                        }
                        if !self.pending_imports.is_empty() {
                            let paths = std::mem::take(&mut self.pending_imports);
                            self.state.queued_imports = 0;
                            if let Err(error) = self.start_scan(paths).await {
                                self.state.status = error.to_string();
                                tracing::warn!(%error, "could not start queued imports");
                            }
                        }
                        changed = true;
                    }
                    if self.volume_dirty && self.last_settings_write.elapsed() >= Duration::from_millis(500) {
                        if let Err(error) = self.persist_volume().await {
                            tracing::warn!(%error, "could not save volume");
                        }
                        self.last_settings_write = Instant::now();
                    }
                    let dropped = self.dropped_logs.dropped_lines();
                    if dropped != last_dropped {
                        tracing::warn!(dropped, "log buffer overflow; some records were dropped"); last_dropped = dropped;
                    }
                    if changed { self.publish(); }
                }
            }
        }
        if let Some(cancel) = self.scan_cancel.take() {
            let _ = cancel.cancel();
        }
        if let Some(task) = self.artwork_task.take() {
            task.abort();
        }
        if let Some(scan) = self.scan_task.take() {
            match tokio::time::timeout(Duration::from_secs(3), scan).await {
                Ok(Ok(Ok(()))) => (),
                Ok(Ok(Err(error))) => tracing::warn!(%error, "scan shutdown failed"),
                Ok(Err(error)) => tracing::warn!(%error, "scan task shutdown failed"),
                Err(_) => {
                    tracing::warn!("Scan cancellation timed out; finishing application shutdown")
                }
            }
        }
        self.persist_volume().await?;
        tracing::info!("application services stopped");
        Ok(())
    }

    #[tracing::instrument(skip_all, err)]
    async fn restore(&mut self) -> Result<()> {
        let volume = self
            .store
            .get_setting("volume")
            .await?
            .and_then(|value| value.parse::<f32>().ok())
            .filter(|value| value.is_finite())
            .unwrap_or(1.0)
            .clamp(0.0, 1.0);
        self.state.playback.volume = volume;
        self.requested_volume = volume;
        self.send_audio(AudioCommand::SetVolume(volume))?;
        let device = self
            .store
            .get_setting("output_device")
            .await?
            .filter(|value| !value.is_empty());
        self.send_audio(AudioCommand::SetDevice(device))?;
        Ok(())
    }

    #[tracing::instrument(level = "debug", skip_all, err)]
    async fn refresh(&mut self) -> Result<()> {
        let (library, folders, albums) = tokio::try_join!(
            self.store.library_filtered(
                &self.state.library.query,
                self.state.library.offset,
                &self.state.library_filter
            ),
            self.store
                .folders(&self.state.folders.query, self.state.folders.offset),
            self.store
                .albums(&self.state.albums.query, self.state.albums.offset),
        )?;
        self.state.library = library;
        self.state.folders = folders;
        self.state.albums = albums;
        self.state.roots = self.store.roots().await?;
        self.state.playlists = self.store.playlists().await?;
        if let Some(id) = self.state.selected_playlist {
            self.state.playlist_entries = self.store.playlist_entries(id).await?.into();
        }
        Ok(())
    }

    fn publish(&self) {
        self.output.send_replace(self.state.clone());
        (self.wake)();
    }

    fn send_audio(&mut self, command: AudioCommand) -> Result<()> {
        self.flush_audio()?;
        if let Some(previous) = self.pending_audio.back_mut()
            && matches!(
                (&*previous, &command),
                (AudioCommand::Seek { .. }, AudioCommand::Seek { .. })
                    | (AudioCommand::SetVolume(_), AudioCommand::SetVolume(_))
                    | (AudioCommand::SetNext(_), AudioCommand::SetNext(_))
            )
        {
            *previous = command;
        } else {
            if self.pending_audio.len() >= 64 {
                bail!("Audio engine busy; please retry");
            }
            self.pending_audio.push_back(command);
        }
        self.flush_audio()
    }

    fn flush_audio(&mut self) -> Result<()> {
        while let Some(command) = self.pending_audio.pop_front() {
            match self.audio.try_send(command) {
                Ok(()) => (),
                Err(std::sync::mpsc::TrySendError::Full(command)) => {
                    self.pending_audio.push_front(command);
                    break;
                }
                Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                    self.pending_audio.clear();
                    bail!("Audio engine has stopped");
                }
            }
        }
        Ok(())
    }

    #[tracing::instrument(level = "debug", skip_all, err)]
    async fn handle(&mut self, command: AppCommand) -> Result<()> {
        match command {
            AppCommand::Import(paths) => self.import(paths).await?,
            AppCommand::RemoveScanFolder(path) => {
                if self.scan_task.is_some() || !self.pending_imports.is_empty() {
                    bail!("Wait for scanning to finish before removing a music folder");
                }
                self.store.remove_scan_folder(&path).await?;
                self.refresh().await?;
                self.state.status = "Music folder removed; files kept on disk".into();
            }
            AppCommand::Rescan => self.start_scan_request(ScanRequest::Library).await?,
            AppCommand::RescanFolder(id) => {
                self.start_scan_request(ScanRequest::Folder(id)).await?
            }
            AppCommand::CancelScan => {
                if let Some(cancel) = self.scan_cancel.take() {
                    let _ = cancel.cancel();
                }
                self.pending_imports.clear();
                self.state.queued_imports = 0;
                self.state.status = "Cancelling scans…".into();
            }
            AppCommand::Search { query, offset } => {
                self.state.library = self
                    .store
                    .library_filtered(&query, offset, &self.state.library_filter)
                    .await?;
            }
            AppCommand::BrowseLibrary {
                filter,
                query,
                offset,
            } => {
                self.state.library = self.store.library_filtered(&query, offset, &filter).await?;
                self.state.library_filter = filter;
            }
            AppCommand::ListFolders { query, offset } => {
                self.state.folders = self.store.folders(&query, offset).await?;
            }
            AppCommand::ListAlbums { query, offset } => {
                self.state.albums = self.store.albums(&query, offset).await?;
            }
            AppCommand::Play(track) => {
                self.load_queue(Arc::new(vec![track]), 0)?;
                self.state.status = "Starting playback…".into();
            }
            AppCommand::Enqueue(track) => {
                let mut queue = Arc::clone(&self.state.queue);
                Arc::make_mut(&mut queue).push(track);
                self.sync_next(&queue, self.state.queue_index)?;
                self.state.queue = queue;
            }
            AppCommand::EnqueueAlbum(key) => {
                let tracks = self.store.album_tracks(&key).await?;
                if tracks.is_empty() {
                    bail!("Album has no available tracks");
                }
                let mut queue = Arc::clone(&self.state.queue);
                Arc::make_mut(&mut queue).extend(tracks);
                self.sync_next(&queue, self.state.queue_index)?;
                self.state.queue = queue;
            }
            AppCommand::PlayAlbum(key) => {
                let tracks = self.store.album_tracks(&key).await?;
                if tracks.is_empty() {
                    bail!("Album has no available tracks");
                }
                self.load_queue(Arc::new(tracks), 0)?;
                self.state.status = "Starting playback…".into();
            }
            AppCommand::RemoveQueue(index) => {
                if index < self.state.queue.len() {
                    let was_current = self.state.queue_index == Some(index);
                    let mut queue = Arc::clone(&self.state.queue);
                    Arc::make_mut(&mut queue).remove(index);
                    let current = index_after_removal(self.state.queue_index, index, queue.len());
                    if was_current {
                        if let Some(index) = current {
                            self.load_queue(Arc::clone(&queue), index)?;
                        } else {
                            self.stop()?;
                        }
                    } else {
                        self.sync_next(&queue, current)?;
                    }
                    self.state.queue = queue;
                    self.state.queue_index = current;
                }
            }
            AppCommand::ClearQueue => {
                self.stop()?;
                self.state.queue = Arc::default();
                self.state.queue_index = None;
            }
            AppCommand::PlayQueue(index) => self.play_index(index)?,
            AppCommand::PauseResume => match self.state.playback.state {
                PlaybackState::Playing | PlaybackState::Buffering => {
                    self.send_audio(AudioCommand::Pause)?
                }
                PlaybackState::Paused => self.send_audio(AudioCommand::Resume)?,
                _ => {
                    if !self.state.queue.is_empty() {
                        self.play_index(self.state.queue_index.unwrap_or(0))?;
                    }
                }
            },
            AppCommand::Stop => self.stop()?,
            AppCommand::Next => {
                let next = self.next_queue_index.or_else(|| {
                    self.state.queue_index.and_then(|index| {
                        self.choose_next(self.state.play_mode, index, self.state.queue.len())
                    })
                });
                if let Some(index) = next {
                    self.play_index(index)?;
                }
            }
            AppCommand::SetPlayMode(mode) => {
                let queue = Arc::clone(&self.state.queue);
                let next = self
                    .state
                    .queue_index
                    .and_then(|index| self.choose_next(mode, index, queue.len()));
                self.send_audio(AudioCommand::SetNext(
                    next.map(|index| queue[index].clone()),
                ))?;
                self.next_queue_index = next;
                self.state.play_mode = mode;
            }
            AppCommand::Previous => {
                if self.state.playback.position_ms > 3000 {
                    self.send_audio(AudioCommand::Seek {
                        position_ms: 0,
                        request_id: 0,
                    })?;
                } else if let Some(index) = self.state.queue_index {
                    self.play_index(index.saturating_sub(1))?;
                }
            }
            AppCommand::Seek {
                position_ms,
                request_id,
            } => self.send_audio(AudioCommand::Seek {
                position_ms,
                request_id,
            })?,
            AppCommand::SetVolume(volume) => {
                if volume.is_finite() {
                    let volume = volume.clamp(0.0, 1.0);
                    self.send_audio(AudioCommand::SetVolume(volume))?;
                    self.requested_volume = volume;
                    self.state.playback.volume = volume;
                    self.volume_dirty = true;
                }
            }
            AppCommand::SetDevice(device) => {
                self.send_audio(AudioCommand::SetDevice(device.clone()))?;
                let queue = Arc::clone(&self.state.queue);
                self.sync_next(&queue, self.state.queue_index)?;
                self.store
                    .set_setting("output_device", device.as_deref().unwrap_or(""))
                    .await?;
            }
            AppCommand::CreatePlaylist(name) => {
                self.state.selected_playlist = Some(self.store.create_playlist(&name).await?);
                self.refresh().await?;
            }
            AppCommand::DeletePlaylist(id) => {
                self.store.delete_playlist(id).await?;
                if self.state.selected_playlist == Some(id) {
                    self.state.selected_playlist = None;
                    self.state.playlist_entries = Arc::default();
                }
                self.refresh().await?;
            }
            AppCommand::SelectPlaylist(id) => {
                self.state.playlist_entries = match id {
                    Some(id) => self.store.playlist_entries(id).await?.into(),
                    None => Arc::default(),
                };
                self.state.selected_playlist = id;
            }
            AppCommand::AddToPlaylist {
                playlist_id,
                track_id,
            } => {
                self.store.add_playlist_entry(playlist_id, track_id).await?;
                self.refresh().await?;
            }
            AppCommand::RemovePlaylistEntry(id) => {
                self.store.remove_playlist_entry(id).await?;
                self.refresh().await?;
            }
            AppCommand::MovePlaylistEntry { entry_id, position } => {
                self.store.move_playlist_entry(entry_id, position).await?;
                self.refresh().await?;
            }
            AppCommand::PlayPlaylist(id) => {
                let entries = self.store.playlist_entries(id).await?;
                if entries.is_empty() {
                    bail!("Playlist is empty");
                }
                self.load_queue(
                    Arc::new(entries.into_iter().map(|entry| entry.track).collect()),
                    0,
                )?;
            }
            AppCommand::Shutdown => (),
        }
        Ok(())
    }

    #[tracing::instrument(skip_all, fields(input_count = paths.len()), err)]
    async fn import(&mut self, paths: Vec<PathBuf>) -> Result<()> {
        if paths.is_empty() {
            bail!("Choose a music folder or local file first");
        }
        if self.scan_task.is_none() {
            return self.start_scan(paths).await;
        }
        let mut pending = self.pending_imports.clone();
        for path in paths {
            if !pending.contains(&path) {
                if pending.len() >= 64 {
                    bail!(
                        "The scan queue is full; wait for it to finish before adding more folders"
                    );
                }
                pending.push(path);
            }
        }
        self.state.queued_imports = pending.len();
        self.state.status = format!("{} path(s) queued for scanning", pending.len());
        self.pending_imports = pending;
        Ok(())
    }

    #[tracing::instrument(skip_all, fields(input_count = paths.len()), err)]
    async fn start_scan(&mut self, paths: Vec<PathBuf>) -> Result<()> {
        if paths.is_empty() {
            bail!("Add a music folder or local file first");
        }
        self.start_scan_request(ScanRequest::Roots(paths)).await
    }

    #[tracing::instrument(skip_all, fields(?request), err)]
    async fn start_scan_request(&mut self, request: ScanRequest) -> Result<()> {
        if self.scan_task.is_some() {
            bail!("A scan is already running; cancel it or wait for completion");
        }
        let progress = ScanProgress {
            running: true,
            ..Default::default()
        };
        let (sender, receiver) = watch::channel(progress);
        let store = self.store.clone();
        let probe = Arc::clone(&self.probe);
        let (cancel, task) = scan_task(store, request, probe, sender).await?;
        self.scan_cancel = Some(cancel);
        self.scan_task = Some(task);
        self.state.scan = receiver.borrow().clone();
        self.scan_progress = Some(receiver);
        self.state.status = "Scanning music…".into();
        Ok(())
    }

    #[tracing::instrument(skip_all, fields(index), err)]
    fn play_index(&mut self, index: usize) -> Result<()> {
        self.load_queue(Arc::clone(&self.state.queue), index)
    }

    fn load_queue(&mut self, queue: Arc<Vec<Track>>, index: usize) -> Result<()> {
        let track = queue
            .get(index)
            .with_context(|| "Queue entry no longer exists")?
            .clone();
        let next_index = self.choose_next(self.state.play_mode, index, queue.len());
        let next = next_index.map(|index| queue[index].clone());
        let generation = self.generation.wrapping_add(1);
        self.send_audio(AudioCommand::Load {
            track,
            next,
            generation,
        })?;
        self.generation = generation;
        self.completed = 0;
        self.next_queue_index = next_index;
        self.state.queue = queue;
        self.state.queue_index = Some(index);
        Ok(())
    }

    fn sync_next(&mut self, queue: &[Track], current: Option<usize>) -> Result<()> {
        let next =
            current.and_then(|index| self.choose_next(self.state.play_mode, index, queue.len()));
        self.send_audio(AudioCommand::SetNext(
            next.map(|index| queue[index].clone()),
        ))?;
        self.next_queue_index = next;
        Ok(())
    }

    fn choose_next(&mut self, mode: PlayMode, current: usize, len: usize) -> Option<usize> {
        if len == 0 || current >= len {
            return None;
        }
        match mode {
            PlayMode::Sequential => (current + 1 < len).then_some(current + 1),
            PlayMode::Repeat => Some((current + 1) % len),
            PlayMode::Shuffle if len == 1 => Some(0),
            PlayMode::Shuffle => {
                self.shuffle_counter = self.shuffle_counter.wrapping_add(1);
                let index = (self.shuffle_random.hash_one(self.shuffle_counter) % (len - 1) as u64)
                    as usize;
                Some(if index >= current { index + 1 } else { index })
            }
        }
    }

    fn stop(&mut self) -> Result<()> {
        let generation = self.generation.wrapping_add(1);
        self.send_audio(AudioCommand::Stop { generation })?;
        self.generation = generation;
        self.completed = 0;
        self.next_queue_index = None;
        self.state.playback = PlaybackSnapshot {
            generation,
            state: PlaybackState::Stopped,
            volume: self.requested_volume,
            selected_device: self.state.playback.selected_device.clone(),
            devices: self.state.playback.devices.clone(),
            underruns: self.state.playback.underruns,
            ..Default::default()
        };
        self.state.artwork = None;
        self.refresh_artwork();
        self.state.status = "Ready".into();
        Ok(())
    }

    fn update_playback(&mut self) -> Result<()> {
        let playback = self.playback.borrow_and_update().clone();
        if playback.generation != self.generation {
            return Ok(());
        }
        let completions = playback.completed.saturating_sub(self.completed);
        let previous_notice =
            self.state.playback.error.is_some() || self.state.playback.notification.is_some();
        self.state.playback = playback;
        self.state.playback.volume = self.requested_volume;
        if previous_notice
            && self.state.playback.error.is_none()
            && self.state.playback.notification.is_none()
        {
            self.state.status = "Ready".into();
        }
        self.refresh_artwork();
        if completions > 0 {
            let mut next_index = self.next_queue_index;
            let mut last_index = self.state.queue_index;
            for _ in 1..completions {
                last_index = next_index.or(last_index);
                next_index = next_index.and_then(|index| {
                    self.choose_next(self.state.play_mode, index, self.state.queue.len())
                });
            }
            if let Some(next_index) = next_index {
                let already_playing = self
                    .state
                    .playback
                    .track
                    .as_ref()
                    .is_some_and(|track| track.id == self.state.queue[next_index].id)
                    && self.state.playback.state != PlaybackState::Stopped;
                if already_playing {
                    let queue = Arc::clone(&self.state.queue);
                    self.sync_next(&queue, Some(next_index))?;
                    self.state.queue_index = Some(next_index);
                } else {
                    self.play_index(next_index)?;
                    return Ok(());
                }
            } else {
                self.state.queue_index = last_index;
                self.next_queue_index = None;
            }
        }
        self.completed = self.state.playback.completed;
        if let Some(message) = self.state.playback.error.as_ref().or(self
            .state
            .playback
            .notification
            .as_ref())
        {
            self.state.status = message.clone();
        }
        Ok(())
    }

    async fn persist_volume(&mut self) -> Result<()> {
        if self.volume_dirty {
            self.store
                .set_setting("volume", &self.requested_volume.to_string())
                .await?;
            self.volume_dirty = false;
        }
        Ok(())
    }

    fn refresh_artwork(&mut self) {
        let path = self
            .state
            .playback
            .track
            .as_ref()
            .map(|track| track.path.clone());
        if path == self.artwork_track {
            return;
        }
        self.artwork_track = path.clone();
        self.artwork_loaded = None;
        if path.is_none() {
            self.state.artwork = None;
        }
        self.start_artwork();
    }

    fn start_artwork(&mut self) {
        if self.artwork_task.is_some() || self.artwork_track == self.artwork_loaded {
            return;
        }
        if let Some(path) = self.artwork_track.clone() {
            self.artwork_reading = Some(path.clone());
            let probe = Arc::clone(&self.probe);
            let span = tracing::info_span!("load_artwork", path = %path.display());
            self.artwork_task = Some(tokio::task::spawn_blocking(move || {
                let result = span.in_scope(|| probe.artwork(&path));
                (path, result)
            }));
        }
    }

    async fn finish_artwork(&mut self) {
        let Some(task) = self.artwork_task.take() else {
            return;
        };
        let reading = self.artwork_reading.take();
        match task.await {
            Ok((path, result)) if self.artwork_track.as_ref() == Some(&path) => {
                self.artwork_loaded = Some(path);
                match result {
                    Ok(artwork) => self.state.artwork = artwork,
                    Err(error) => {
                        self.state.artwork = None;
                        tracing::warn!(%error, "could not read album artwork");
                    }
                }
            }
            Ok(_) => (),
            Err(error) => {
                if self.artwork_track == reading {
                    self.state.artwork = None;
                }
                self.artwork_loaded = reading;
                tracing::warn!(%error, "artwork task failed");
            }
        }
        self.start_artwork();
    }
}

fn index_after_removal(current: Option<usize>, removed: usize, remaining: usize) -> Option<usize> {
    current.and_then(|index| {
        if remaining == 0 {
            None
        } else if removed < index {
            Some(index - 1)
        } else {
            Some(index.min(remaining - 1))
        }
    })
}

#[cfg(test)]
#[path = "../tests/unit/app.rs"]
mod tests;
