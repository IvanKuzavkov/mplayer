use anyhow::{Context, bail};
use mplayer_core::*;
use std::{
    collections::VecDeque,
    hash::{BuildHasher, RandomState},
    sync::{Arc, mpsc::SyncSender},
    time::Duration,
};
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
};
use tracing::Instrument;

pub(super) struct PlaybackService {
    pub(super) audio: SyncSender<CommandEnvelope<AudioCommand>>,
    pub(super) pending_audio: VecDeque<CommandEnvelope<AudioCommand>>,
    pending_playback_update: bool,
    playback: watch::Receiver<PlaybackSnapshot>,
    commands: mpsc::Receiver<CommandEnvelope<PlaybackCommand>>,
    output: watch::Sender<PlayerSnapshot>,
    pub(super) state: PlayerSnapshot,
    library: LibraryClient,
    desired: watch::Sender<SettingsUpdate>,
    pub(super) completed: u64,
    pub(super) generation: u64,
    pub(super) next_queue_index: Option<usize>,
    shuffle_random: RandomState,
    shuffle_counter: u64,
    pub(super) requested_volume: f32,
    trace: TraceContext,
    pending: VecDeque<CommandEnvelope<PlaybackCommand>>,
    lookup: Option<JoinHandle<Result<Vec<Track>>>>,
}
impl PlaybackService {
    pub(super) fn new(
        audio: SyncSender<CommandEnvelope<AudioCommand>>,
        playback: watch::Receiver<PlaybackSnapshot>,
        commands: mpsc::Receiver<CommandEnvelope<PlaybackCommand>>,
        output: watch::Sender<PlayerSnapshot>,
        library: LibraryClient,
        desired: watch::Sender<SettingsUpdate>,
    ) -> Self {
        let volume = desired.borrow().volume.payload;
        Self {
            audio,
            playback,
            commands,
            output,
            library,
            desired,
            pending_audio: VecDeque::new(),
            pending_playback_update: false,
            state: PlayerSnapshot {
                playback: PlaybackSnapshot {
                    volume,
                    ..Default::default()
                },
                ..Default::default()
            },
            completed: 0,
            generation: 0,
            next_queue_index: None,
            shuffle_random: RandomState::new(),
            shuffle_counter: 0,
            requested_volume: volume,
            trace: TraceContext::new(&"playback_start"),
            pending: VecDeque::new(),
            lookup: None,
        }
    }
    pub(super) async fn run(mut self, mut shutdown: watch::Receiver<bool>) -> Result<()> {
        let settings = self.desired.borrow().clone();
        self.send_audio(AudioCommand::SetVolume(settings.volume.payload))?;
        self.send_audio(AudioCommand::SetDevice(settings.device.payload))?;
        self.publish();
        loop {
            tokio::select! {
                biased;
                _ = shutdown_requested(&mut shutdown) => break,
                changed = self.playback.changed() => {
                    if changed.is_err() {
                        // Preserve the final value and report only this service as unavailable.
                        let _ = self.update_playback();
                        self.state.status = "Audio engine has stopped".into();
                        self.state.playback.error = Some(self.state.status.clone());
                        self.publish();
                        break;
                    }
                    if let Err(error) = self.update_playback() {
                        self.pending_playback_update = true;
                        self.fail(error);
                    }
                    self.publish();
                }
                command = self.commands.recv() => {
                    let Some(command) = command else { break };
                    self.accept(command);
                    self.publish();
                }
                result = async { match self.lookup.as_mut() { Some(task) => Some(task.await), None => std::future::pending().await } } => {
                    self.lookup = None;
                    let command = self.pending.pop_front().expect("lookup has an intent");
                    self.trace = command.trace;
                    let result = result.unwrap().map_err(anyhow::Error::from).and_then(|result| result);
                    let result = result.and_then(|tracks| {
                        if matches!(command.payload, PlaybackCommand::EnqueueAlbum(_)) { self.enqueue(tracks) }
                        else { self.load_queue(tracks.into(), 0) }
                    });
                    self.finish(result);
                    self.drain_pending();
                    self.publish();
                }
                _ = tokio::time::sleep(Duration::from_millis(30)), if !self.pending_audio.is_empty() || self.pending_playback_update => {
                    if let Err(error) = self.flush_audio() { self.fail(error); }
                    if self.pending_playback_update {
                        self.pending_playback_update = self.update_playback().is_err();
                    }
                    self.publish();
                }
            }
        }
        self.commands.close();
        // Closing a window may race the last slider/device command. Persist
        // accepted preferences without starting new playback during shutdown.
        while let Ok(command) = self.commands.try_recv() {
            match command.payload {
                PlaybackCommand::SetVolume(volume) if volume.is_finite() => {
                    self.desired.send_modify(|settings| {
                        settings.volume = command.trace.wrap(volume.clamp(0.0, 1.0))
                    })
                }
                PlaybackCommand::SetDevice(device) => self
                    .desired
                    .send_modify(|settings| settings.device = command.trace.wrap(device)),
                _ => command.trace.event("playback", "cancelled"),
            }
        }
        self.cancel_pending(false, None);
        // The audio owner also has an out-of-band stop flag for a saturated inbox.
        let _ = self.send_audio(AudioCommand::Stop {
            generation: self.generation.wrapping_add(1),
        });
        Ok(())
    }
    fn publish(&self) {
        publish_changed(&self.output, &self.state);
    }
    fn fail(&mut self, error: anyhow::Error) {
        self.trace.failed("playback", &error);
        self.state.status = error.to_string();
        self.state.playback.error = Some(error.to_string());
    }
    fn finish(&mut self, result: Result<()>) {
        match result {
            Ok(()) => self.trace.event("playback", "handled"),
            Err(error) => self.fail(error),
        }
    }
    fn cancel_pending(&mut self, starts_only: bool, replacement: Option<&TraceContext>) {
        let is_start = |command: &PlaybackCommand| {
            matches!(
                command,
                PlaybackCommand::Play(_)
                    | PlaybackCommand::PlayAlbum(_)
                    | PlaybackCommand::PlayPlaylist(_)
            )
        };
        if self
            .pending
            .front()
            .is_some_and(|command| !starts_only || is_start(&command.payload))
            && let Some(task) = self.lookup.take()
        {
            task.abort();
        }
        self.pending.retain(|command| {
            if !starts_only || is_start(&command.payload) {
                if let Some(next) = replacement {
                    command.trace.superseded_by(next, "playback");
                } else {
                    command.trace.event("playback", "cancelled");
                }
                false
            } else {
                true
            }
        });
    }
    pub(super) fn accept(&mut self, command: CommandEnvelope<PlaybackCommand>) {
        use PlaybackCommand::*;
        match &command.payload {
            Play(_) | PlayAlbum(_) | PlayPlaylist(_) | ClearQueue => {
                self.cancel_pending(false, Some(&command.trace))
            }
            Stop | Next | Previous | PlayQueue(_) => {
                self.cancel_pending(true, Some(&command.trace))
            }
            _ => (),
        }
        let queued = matches!(
            &command.payload,
            PlayAlbum(_) | PlayPlaylist(_) | EnqueueAlbum(_)
        ) || (!self.pending.is_empty()
            && matches!(&command.payload, Enqueue(_) | RemoveQueue(_)));
        if queued {
            if self.pending.len() >= INBOX_CAPACITY {
                self.trace = command.trace;
                self.fail(anyhow::anyhow!("Playback queue is busy; please retry"));
                return;
            }
            command.trace.event("playback", "queued_lookup");
            self.pending.push_back(command);
        } else {
            self.trace = command.trace;
            let span = self.trace.span.clone();
            let result = span.in_scope(|| self.handle(command.payload));
            self.finish(result);
        }
        self.drain_pending();
    }
    fn drain_pending(&mut self) {
        while self.lookup.is_none() && !self.pending.is_empty() {
            let command = self.pending.front().unwrap();
            let source = match &command.payload {
                PlaybackCommand::PlayAlbum(key) | PlaybackCommand::EnqueueAlbum(key) => {
                    Some(TrackSource::Album(key.clone()))
                }
                PlaybackCommand::PlayPlaylist(id) => Some(TrackSource::Playlist(*id)),
                _ => None,
            };
            if let Some(source) = source {
                let library = self.library.clone();
                let trace = command.trace.clone();
                let span = trace.span.clone();
                self.lookup = Some(tokio::spawn(
                    async move { library.tracks(source, &trace).await }.instrument(span),
                ));
            } else {
                let command = self.pending.pop_front().unwrap();
                self.trace = command.trace;
                let span = self.trace.span.clone();
                let result = span.in_scope(|| self.handle(command.payload));
                self.finish(result);
            }
        }
    }
    fn enqueue(&mut self, tracks: Vec<Track>) -> Result<()> {
        let mut queue = Arc::clone(&self.state.queue);
        Arc::make_mut(&mut queue).extend(tracks);
        self.sync_next(&queue, self.state.queue_index)?;
        self.state.queue = queue;
        Ok(())
    }
    pub(super) fn send_audio(&mut self, command: AudioCommand) -> Result<()> {
        self.flush_audio()?;
        let command = self.trace.wrap(command);
        if let Some(previous) = self.pending_audio.back_mut()
            && matches!(
                (&previous.payload, &command.payload),
                (AudioCommand::Seek { .. }, AudioCommand::Seek { .. })
                    | (AudioCommand::SetVolume(_), AudioCommand::SetVolume(_))
                    | (AudioCommand::SetNext(_), AudioCommand::SetNext(_))
            )
        {
            previous.trace.superseded_by(&command.trace, "audio");
            *previous = command;
        } else {
            if self.pending_audio.len() >= 64 {
                bail!("Audio engine busy; please retry");
            }
            self.pending_audio.push_back(command);
        }
        self.flush_audio()
    }

    pub(super) fn flush_audio(&mut self) -> Result<()> {
        while let Some(command) = self.pending_audio.pop_front() {
            let trace = command.trace.clone();
            match self.audio.try_send(command) {
                Ok(()) => trace.event("audio", "dispatched"),
                Err(std::sync::mpsc::TrySendError::Full(command)) => {
                    trace.event("audio", "inbox_full");
                    self.pending_audio.push_front(command);
                    break;
                }
                Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                    trace.event("audio", "rejected_closed");
                    for command in self.pending_audio.drain(..) {
                        command.trace.event("audio", "rejected_closed");
                    }
                    bail!("Audio engine has stopped");
                }
            }
        }
        Ok(())
    }

    pub(super) fn handle(&mut self, command: PlaybackCommand) -> Result<()> {
        match command {
            PlaybackCommand::Play(track) => {
                self.load_queue(Arc::new(vec![track]), 0)?;
                self.state.status = "Starting playback…".into();
            }
            PlaybackCommand::Enqueue(track) => {
                let mut queue = Arc::clone(&self.state.queue);
                Arc::make_mut(&mut queue).push(track);
                self.sync_next(&queue, self.state.queue_index)?;
                self.state.queue = queue;
            }
            PlaybackCommand::RemoveQueue(index) => {
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
            PlaybackCommand::ClearQueue => {
                self.stop()?;
                self.state.queue = Arc::default();
                self.state.queue_index = None;
            }
            PlaybackCommand::PlayQueue(index) => self.play_index(index)?,
            PlaybackCommand::PauseResume => match self.state.playback.state {
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
            PlaybackCommand::Stop => self.stop()?,
            PlaybackCommand::Next => {
                let next = self.next_queue_index.or_else(|| {
                    self.state.queue_index.and_then(|index| {
                        self.choose_next(self.state.play_mode, index, self.state.queue.len())
                    })
                });
                if let Some(index) = next {
                    self.play_index(index)?;
                }
            }
            PlaybackCommand::SetPlayMode(mode) => {
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
            PlaybackCommand::Previous => {
                if self.state.playback.position_ms > 3000 {
                    self.send_audio(AudioCommand::Seek {
                        position_ms: 0,
                        request_id: 0,
                    })?;
                } else if let Some(index) = self.state.queue_index {
                    self.play_index(index.saturating_sub(1))?;
                }
            }
            PlaybackCommand::Seek {
                position_ms,
                request_id,
            } => self.send_audio(AudioCommand::Seek {
                position_ms,
                request_id,
            })?,
            PlaybackCommand::SetVolume(volume) => {
                if volume.is_finite() {
                    let volume = volume.clamp(0.0, 1.0);
                    self.send_audio(AudioCommand::SetVolume(volume))?;
                    self.requested_volume = volume;
                    self.state.playback.volume = volume;
                    self.desired.send_modify(|settings| {
                        settings.volume.trace.superseded_by(&self.trace, "settings");
                        settings.volume = self.trace.wrap(volume);
                    });
                }
            }
            PlaybackCommand::SetDevice(device) => {
                self.send_audio(AudioCommand::SetDevice(device.clone()))?;
                let queue = Arc::clone(&self.state.queue);
                self.sync_next(&queue, self.state.queue_index)?;
                self.desired.send_modify(|settings| {
                    settings.device.trace.superseded_by(&self.trace, "settings");
                    settings.device = self.trace.wrap(device);
                });
            }
            PlaybackCommand::PlayAlbum(_)
            | PlaybackCommand::EnqueueAlbum(_)
            | PlaybackCommand::PlayPlaylist(_) => {
                unreachable!("lookups are handled asynchronously")
            }
        }
        Ok(())
    }
    #[tracing::instrument(skip_all, fields(index), err)]
    pub(super) fn play_index(&mut self, index: usize) -> Result<()> {
        self.load_queue(Arc::clone(&self.state.queue), index)
    }

    pub(super) fn load_queue(&mut self, queue: Arc<Vec<Track>>, index: usize) -> Result<()> {
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
        self.state.origin = Some(self.trace.clone());
        self.generation = generation;
        self.completed = 0;
        self.next_queue_index = next_index;
        self.state.queue = queue;
        self.state.queue_index = Some(index);
        Ok(())
    }

    pub(super) fn sync_next(&mut self, queue: &[Track], current: Option<usize>) -> Result<()> {
        let next =
            current.and_then(|index| self.choose_next(self.state.play_mode, index, queue.len()));
        self.send_audio(AudioCommand::SetNext(
            next.map(|index| queue[index].clone()),
        ))?;
        self.next_queue_index = next;
        Ok(())
    }

    pub(super) fn choose_next(
        &mut self,
        mode: PlayMode,
        current: usize,
        len: usize,
    ) -> Option<usize> {
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

    pub(super) fn stop(&mut self) -> Result<()> {
        let generation = self.generation.wrapping_add(1);
        self.send_audio(AudioCommand::Stop { generation })?;
        self.state.artwork_revision = self.state.artwork_revision.wrapping_add(1);
        self.state.origin = Some(self.trace.clone());
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
        self.state.status = "Ready".into();
        Ok(())
    }

    pub(super) fn update_playback(&mut self) -> Result<()> {
        let playback = self.playback.borrow_and_update().clone();
        if playback.generation != self.generation {
            return Ok(());
        }
        if let Some(origin) = &self.state.origin {
            self.trace = origin.clone();
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
}

pub(super) fn index_after_removal(
    current: Option<usize>,
    removed: usize,
    remaining: usize,
) -> Option<usize> {
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
