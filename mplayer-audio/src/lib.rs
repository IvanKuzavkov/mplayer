//! Local audio decoding and native output behind a bounded command interface.
mod decoder;
mod discovery;
mod dsd;
mod dsp;
mod output;
mod pcm;

use anyhow::{anyhow, bail};
pub use decoder::SymphoniaProbe;

use decoder::Decoder;
use dsp::Converter;
use mplayer_core::{AudioCommand, PlaybackSnapshot, PlaybackState, Result, Track, Wake};
use output::{Output, RateSelection};
use pcm::PcmFormat;
use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use tokio::sync::watch;
use tracing::Instrument;

pub struct AudioEngine {
    sender: SyncSender<AudioCommand>,
    snapshots: watch::Receiver<PlaybackSnapshot>,
    thread: Option<JoinHandle<()>>,
    discovery: tokio::task::JoinHandle<()>,
    stop: Arc<AtomicBool>,
}

impl AudioEngine {
    /// Requires an active Tokio runtime for background output-device discovery.
    #[tracing::instrument(skip_all, err)]
    pub fn start(wake: Wake) -> Result<Self> {
        let runtime = tokio::runtime::Handle::try_current()?;
        let (device_sender, device_updates) = watch::channel(Vec::new());
        let (sender, receiver) = mpsc::sync_channel(64);
        let (snapshots_tx, snapshots) = watch::channel(PlaybackSnapshot {
            volume: 1.0,
            ..Default::default()
        });
        let parent = tracing::Span::current();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let thread = thread::Builder::new()
            .name("mplayer-audio".into())
            .spawn(move || {
                parent.in_scope(|| {
                    Worker::new(snapshots_tx, wake, worker_stop, device_updates).run(receiver)
                });
            })?;
        let discovery = runtime.spawn(
            discovery::run(device_sender, output::devices, Duration::from_secs(3))
                .in_current_span(),
        );
        Ok(Self {
            sender,
            snapshots,
            thread: Some(thread),
            discovery,
            stop,
        })
    }
    pub fn sender(&self) -> SyncSender<AudioCommand> {
        self.sender.clone()
    }
    pub fn subscribe(&self) -> watch::Receiver<PlaybackSnapshot> {
        self.snapshots.clone()
    }
    #[tracing::instrument(skip_all, err)]
    pub fn shutdown(mut self) -> Result<()> {
        self.discovery.abort();
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.sender.try_send(AudioCommand::Shutdown);
        if let Some(thread) = self.thread.take() {
            let deadline = Instant::now() + Duration::from_secs(3);
            while !thread.is_finished() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(10));
            }
            if !thread.is_finished() {
                bail!("Audio worker shutdown timed out; detached a blocked file/device operation");
            }
            thread
                .join()
                .map_err(|_| anyhow!("Audio worker panicked"))?;
        }
        Ok(())
    }
}
impl Drop for AudioEngine {
    fn drop(&mut self) {
        self.discovery.abort();
        if let Some(thread) = self.thread.take() {
            self.stop.store(true, Ordering::Relaxed);
            let _ = self.sender.try_send(AudioCommand::Shutdown);
            if thread.is_finished() {
                let _ = thread.join();
            }
        }
    }
}

struct Prepared {
    track: Track,
    decoder: Decoder,
}
impl Prepared {
    fn open(mut track: Track) -> Result<Self> {
        let mut decoder = Decoder::open(&track.path)?;
        if let Some(segment) = track.segment {
            decoder.set_segment(segment)?;
            track.metadata.sample_rate = decoder.metadata.sample_rate;
            track.metadata.channels = decoder.metadata.channels;
            track.metadata.bit_depth = decoder.metadata.bit_depth;
            track.metadata.codec.clone_from(&decoder.metadata.codec);
        } else {
            track.metadata = decoder.metadata.clone();
        }
        track.missing = false;
        Ok(Self { track, decoder })
    }

    fn rate_selection(&self) -> RateSelection {
        if self.decoder.is_dsd() {
            RateSelection::Highest
        } else {
            RateSelection::Native
        }
    }
}
struct Session {
    prepared: Prepared,
    converter: Converter,
    decoded: Vec<f64>,
    decoded_offset: usize,
    pending: Vec<f64>,
    pending_offset: usize,
    eof: bool,
    flushed: bool,
    converter_origin: u64,
}
impl Session {
    fn new(prepared: Prepared, rate: u32, channels: usize) -> Result<Self> {
        let spec = prepared.decoder.pcm_spec();
        let converter = Converter::new(spec.rate, rate, usize::from(spec.channels), channels)?;
        Ok(Self {
            prepared,
            converter,
            decoded: Vec::new(),
            decoded_offset: 0,
            pending: Vec::new(),
            pending_offset: 0,
            eof: false,
            flushed: false,
            converter_origin: 0,
        })
    }
    fn fill(&mut self) -> Result<()> {
        self.pending_offset = 0;
        self.pending.clear();
        while self.pending.is_empty() && !self.eof {
            if self.decoded_offset < self.decoded.len() {
                let channels = usize::from(self.prepared.track.metadata.channels);
                let end = (self.decoded_offset + 1024 * channels).min(self.decoded.len());
                self.converter.convert(
                    &self.decoded[self.decoded_offset..end],
                    false,
                    &mut self.pending,
                )?;
                self.decoded_offset = end;
            } else if self.prepared.decoder.next(&mut self.decoded)? {
                self.decoded_offset = 0;
            } else {
                self.eof = true;
            }
        }
        Ok(())
    }

    fn continue_with(mut self, prepared: Prepared) -> Self {
        self.prepared = prepared;
        self.decoded.clear();
        self.decoded_offset = 0;
        self.pending.clear();
        self.pending_offset = 0;
        self.eof = false;
        self.flushed = false;
        self
    }
}
struct Segment {
    track: Track,
    start_sample: u64,
    end_sample: Option<u64>,
    base_ms: u64,
    pcm_rate: u32,
    rate_selection: RateSelection,
}

#[derive(Clone)]
struct Recovery {
    track: Track,
    next: Option<Track>,
    position_ms: u64,
    paused: bool,
}

#[cfg(test)]
type OutputFactory = fn(u32, usize, PcmFormat, RateSelection, Option<&str>, f32) -> Result<Output>;

struct Worker {
    sender: watch::Sender<PlaybackSnapshot>,
    snapshot: PlaybackSnapshot,
    wake: Wake,
    selected_device: Option<String>,
    device_updates: watch::Receiver<Vec<String>>,
    recovery: Option<Recovery>,
    #[cfg(test)]
    output_factory: Option<OutputFactory>,
    output: Option<Output>,
    session: Option<Session>,
    next: Option<Track>,
    prepared: Option<Prepared>,
    segments: VecDeque<Segment>,
    written: u64,
    drain_at: Option<Instant>,
    last_publish: Instant,
    last_diagnostics: Instant,
    total_underruns: u64,
    stop: Arc<AtomicBool>,
}
impl Worker {
    fn new(
        sender: watch::Sender<PlaybackSnapshot>,
        wake: Wake,
        stop: Arc<AtomicBool>,
        mut device_updates: watch::Receiver<Vec<String>>,
    ) -> Self {
        let devices = device_updates.borrow_and_update().clone();
        Self {
            sender,
            snapshot: PlaybackSnapshot {
                volume: 1.0,
                devices,
                ..Default::default()
            },
            wake,
            selected_device: None,
            device_updates,
            recovery: None,
            #[cfg(test)]
            output_factory: None,
            output: None,
            session: None,
            next: None,
            prepared: None,
            segments: VecDeque::new(),
            written: 0,
            drain_at: None,
            last_publish: Instant::now(),
            last_diagnostics: Instant::now(),
            total_underruns: 0,
            stop,
        }
    }
    #[cfg(test)]
    fn with_devices(
        sender: watch::Sender<PlaybackSnapshot>,
        wake: Wake,
        stop: Arc<AtomicBool>,
        devices: Vec<String>,
    ) -> Self {
        Self::new(sender, wake, stop, watch::channel(devices).1)
    }
    #[tracing::instrument(skip_all)]
    fn run(mut self, receiver: Receiver<AudioCommand>) {
        tracing::info!("Audio worker started");
        self.publish();
        while !self.stop.load(Ordering::Relaxed) {
            let timeout = if self.output.is_some() {
                Duration::from_millis(5)
            } else {
                Duration::from_millis(100)
            };
            match receiver.recv_timeout(timeout) {
                Ok(AudioCommand::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Ok(command) => {
                    if let Err(error) = self.command(command) {
                        self.fail(error);
                    }
                    self.publish();
                }
                Err(mpsc::RecvTimeoutError::Timeout) => (),
            }
            if let Err(error) = self.tick() {
                self.fail(error);
                self.publish();
            }
            if self.last_publish.elapsed() >= Duration::from_millis(100) && self.output.is_some() {
                self.publish();
            }
            self.refresh_devices();
        }
        self.reset();
        tracing::info!("Audio worker stopped");
    }
    fn refresh_devices(&mut self) {
        let devices = self.device_updates.borrow_and_update();
        if self.snapshot.devices == *devices {
            return;
        }
        self.snapshot.devices.clone_from(&devices);
        drop(devices);
        self.publish();
    }
    #[tracing::instrument(skip_all, fields(command = command_name(&command)), err)]
    fn command(&mut self, command: AudioCommand) -> Result<()> {
        match command {
            AudioCommand::Load {
                track,
                next,
                generation,
            } => {
                self.reset();
                self.snapshot.generation = generation;
                self.snapshot.completed = 0;
                self.snapshot.state = PlaybackState::Buffering;
                self.snapshot.track = Some(track.clone());
                self.snapshot.position_ms = 0;
                self.snapshot.error = None;
                self.snapshot.notification = None;
                self.recovery = Some(Recovery {
                    track: track.clone(),
                    next: next.clone(),
                    position_ms: 0,
                    paused: false,
                });
                self.publish();
                self.load(Prepared::open(track)?, next, 0, false)?;
            }
            AudioCommand::SetNext(next) => {
                // Invalidate already-buffered lookahead if the application changes the queue.
                if self.segments.len() > 1 {
                    if self.segments.get(1).map(|segment| segment.track.id)
                        == next.as_ref().map(|track| track.id)
                    {
                        return Ok(());
                    }
                    let track = self.segments.front().map(|segment| segment.track.clone());
                    let position = self.position();
                    let paused = self.snapshot.state == PlaybackState::Paused;
                    if let Some(track) = track {
                        self.reset();
                        let mut prepared = Prepared::open(track)?;
                        prepared.decoder.seek(position)?;
                        self.load(prepared, next, position, paused)?;
                    }
                } else {
                    if self.next.as_ref().map(|track| track.id)
                        != next.as_ref().map(|track| track.id)
                    {
                        self.prepared = None;
                    }
                    self.next = next;
                }
            }
            AudioCommand::Pause => {
                if let Some(output) = &self.output {
                    output.pause(true);
                    self.snapshot.state = PlaybackState::Paused;
                }
            }
            AudioCommand::Resume => {
                if let Some(output) = &self.output {
                    output.pause(false);
                    self.snapshot.state = PlaybackState::Playing;
                } else if let Some(mut recovery) = self.recovery.clone() {
                    recovery.paused = false;
                    self.restore(recovery)?;
                }
            }
            AudioCommand::Stop { generation } => {
                self.reset();
                self.snapshot = PlaybackSnapshot {
                    generation,
                    state: PlaybackState::Stopped,
                    volume: self.snapshot.volume,
                    selected_device: self.snapshot.selected_device.clone(),
                    devices: self.snapshot.devices.clone(),
                    underruns: self.snapshot.underruns,
                    ..Default::default()
                };
            }
            AudioCommand::Seek {
                position_ms: position,
                request_id,
            } => {
                if let Some(track) = self.snapshot.track.clone() {
                    let position = if track.metadata.duration_ms > 0 {
                        position.min(track.metadata.duration_ms.saturating_sub(1))
                    } else {
                        position
                    };
                    let next = self.future_track();
                    let paused = self.snapshot.state == PlaybackState::Paused;
                    self.reset();
                    let mut prepared = Prepared::open(track)?;
                    prepared.decoder.seek(position)?;
                    self.load(prepared, next, position, paused)?;
                    self.snapshot.seek_request_id = request_id;
                }
            }
            AudioCommand::SetVolume(volume) => {
                if !volume.is_finite() {
                    bail!("Volume must be a finite number");
                }
                self.snapshot.volume = volume.clamp(0.0, 1.0);
                tracing::info!(volume = self.snapshot.volume, "Software volume changed");
                if let Some(output) = &self.output {
                    output.set_volume(self.snapshot.volume);
                }
            }
            AudioCommand::SetDevice(device) => {
                self.refresh_devices();
                if self.selected_device == device {
                    return Ok(());
                }
                self.selected_device = device.clone();
                self.snapshot.selected_device = device;
                let recovery = if self.output.is_some() {
                    self.current_recovery()
                } else if matches!(
                    self.snapshot.state,
                    PlaybackState::Paused | PlaybackState::Error
                ) {
                    self.recovery.clone().or_else(|| self.current_recovery())
                } else {
                    None
                };
                self.reset();
                self.snapshot.state = PlaybackState::Stopped;
                self.snapshot.error = None;
                self.snapshot.notification = None;
                if let Some(recovery) = recovery {
                    self.restore(recovery)?;
                }
            }
            AudioCommand::Shutdown => (),
        }
        Ok(())
    }

    #[tracing::instrument(skip_all, err)]
    fn restore(&mut self, mut recovery: Recovery) -> Result<()> {
        if recovery.track.metadata.duration_ms > 0 {
            recovery.position_ms = recovery
                .position_ms
                .min(recovery.track.metadata.duration_ms.saturating_sub(1));
        }
        self.recovery = Some(recovery.clone());
        let mut prepared = Prepared::open(recovery.track)?;
        if recovery.position_ms > 0 {
            prepared.decoder.seek(recovery.position_ms)?;
        }
        self.load(
            prepared,
            recovery.next,
            recovery.position_ms,
            recovery.paused,
        )
    }

    fn future_track(&self) -> Option<Track> {
        self.segments
            .get(1)
            .map(|segment| segment.track.clone())
            .or_else(|| self.next.clone())
    }

    fn current_recovery(&self) -> Option<Recovery> {
        self.snapshot.track.clone().map(|track| Recovery {
            track,
            next: self.future_track(),
            position_ms: self.position(),
            paused: self.snapshot.state == PlaybackState::Paused,
        })
    }

    fn open_output(
        &mut self,
        rate: u32,
        channels: usize,
        format: PcmFormat,
        rate_selection: RateSelection,
    ) -> Result<Output> {
        #[cfg(test)]
        if let Some(factory) = self.output_factory {
            return factory(
                rate,
                channels,
                format,
                rate_selection,
                self.selected_device.as_deref(),
                self.snapshot.volume,
            );
        }
        Output::open(
            rate,
            channels,
            format,
            rate_selection,
            self.selected_device.as_deref(),
            self.snapshot.volume,
        )
    }
    fn load(
        &mut self,
        prepared: Prepared,
        next: Option<Track>,
        position: u64,
        paused: bool,
    ) -> Result<()> {
        self.snapshot.track = Some(prepared.track.clone());
        self.snapshot.source_rate = prepared.track.metadata.sample_rate;
        self.snapshot.duration_ms = prepared.track.metadata.duration_ms;
        let output = self.open_output(
            prepared.decoder.pcm_spec().rate,
            usize::from(prepared.decoder.pcm_spec().channels),
            prepared.decoder.pcm_format(),
            prepared.rate_selection(),
        )?;
        let session = Session::new(prepared, output.rate, output.channels)?;
        self.snapshot.track = Some(session.prepared.track.clone());
        self.snapshot.source_rate = session.prepared.track.metadata.sample_rate;
        self.snapshot.duration_ms = session.prepared.track.metadata.duration_ms;
        self.snapshot.position_ms = position;
        self.snapshot.output_rate = output.rate;
        self.snapshot.device = output.device.clone();
        Self::log_signal_path(&session.prepared, &output, self.snapshot.volume);
        self.snapshot.error = None;
        self.snapshot.notification = None;
        self.snapshot.state = if paused {
            PlaybackState::Paused
        } else {
            PlaybackState::Buffering
        };
        self.segments.push_back(Segment {
            track: session.prepared.track.clone(),
            start_sample: 0,
            end_sample: None,
            base_ms: position,
            pcm_rate: session.prepared.decoder.pcm_spec().rate,
            rate_selection: session.prepared.rate_selection(),
        });
        self.session = Some(session);
        self.output = Some(output);
        self.last_diagnostics = Instant::now();
        self.next = next;
        self.written = 0;
        self.drain_at = None;
        self.produce()?;
        if !paused {
            if let Some(output) = &self.output {
                output.pause(false);
            }
            self.snapshot.state = PlaybackState::Playing;
        }
        self.recovery = None;
        Ok(())
    }
    fn prepare_next(&mut self) {
        if self.prepared.is_some() {
            return;
        }
        if let Some(output) = &self.output {
            let queued_frames =
                self.written.saturating_sub(output.consumed()) / output.channels as u64;
            if self.session.as_ref().is_some_and(|session| !session.eof)
                && queued_frames < u64::from(output.rate) / 4
            {
                return;
            }
        }
        if let Some(track) = self.next.clone() {
            match Prepared::open(track) {
                Ok(prepared) => self.prepared = Some(prepared),
                Err(error) => {
                    tracing::warn!(%error, "Could not preload next track");
                    self.snapshot.error = Some(format!("Could not preload next track: {error}"));
                    self.next = None;
                }
            }
        }
    }
    fn compatible(&self, prepared: &Prepared) -> bool {
        let Some(output) = &self.output else {
            return false;
        };
        let spec = prepared.decoder.pcm_spec();
        self.segments.back().is_some_and(|segment| {
            segment.pcm_rate == spec.rate && segment.rate_selection == prepared.rate_selection()
        }) && (usize::from(spec.channels) == output.channels
            || (spec.channels == 1 && output.channels == 2))
            && spec.format.preserved_by(output.sample_format)
    }

    fn log_signal_path(prepared: &Prepared, output: &Output, volume: f32) {
        let metadata = &prepared.track.metadata;
        let spec = prepared.decoder.pcm_spec();
        tracing::info!(
            track_id = prepared.track.id,
            source_codec = metadata.codec,
            source_bits = metadata.bit_depth,
            decoded_format = ?spec.format,
            decoded_rate = spec.rate,
            dsd_to_pcm = prepared.decoder.is_dsd(),
            output_format = %output.sample_format,
            source_rate = metadata.sample_rate,
            output_rate = output.rate,
            source_channels = metadata.channels,
            output_channels = output.channels,
            resampling = spec.rate != output.rate,
            channel_conversion = usize::from(spec.channels) != output.channels,
            precision_reduction = !spec.format.preserved_by(output.sample_format),
            software_volume = volume != 1.0,
            "Configured application PCM path; OS/DAC processing is unverified"
        );
    }
    fn append_compatible_prepared(&mut self) -> Result<bool> {
        if self.session.is_some()
            || self.segments.is_empty()
            || !self
                .prepared
                .as_ref()
                .is_some_and(|prepared| self.compatible(prepared))
        {
            return Ok(false);
        }
        let prepared = self.prepared.take().expect("prepared checked above");
        self.next = None;
        let output = self.output.as_ref().expect("output active");
        let mut session = Session::new(prepared, output.rate, output.channels)?;
        Self::log_signal_path(&session.prepared, output, self.snapshot.volume);
        session.converter_origin = self.written;
        self.segments.push_back(Segment {
            track: session.prepared.track.clone(),
            start_sample: self.written,
            end_sample: None,
            base_ms: 0,
            pcm_rate: session.prepared.decoder.pcm_spec().rate,
            rate_selection: session.prepared.rate_selection(),
        });
        output.state.ending.store(false, Ordering::Relaxed);
        self.drain_at = None;
        self.session = Some(session);
        Ok(true)
    }

    fn continue_converter_into_next(&mut self) -> Result<bool> {
        let Some(session) = self.session.as_ref() else {
            return Ok(false);
        };
        let Some(prepared) = self.prepared.as_ref() else {
            return Ok(false);
        };
        if !session.eof
            || session.flushed
            || session.pending_offset != session.pending.len()
            || !self.compatible(prepared)
            || prepared.track.metadata.channels != session.prepared.track.metadata.channels
        {
            return Ok(false);
        }
        let output = self.output.as_ref().expect("session has output");
        let boundary = session
            .converter_origin
            .checked_add(
                session
                    .converter
                    .target_frames()?
                    .checked_mul(output.channels as u64)
                    .ok_or(anyhow!("Audio boundary overflow"))?,
            )
            .ok_or(anyhow!("Audio boundary overflow"))?;
        if let Some(segment) = self.segments.back_mut() {
            segment.end_sample = Some(boundary);
        }
        let previous = self.session.take().expect("session checked above");
        let prepared = self.prepared.take().expect("prepared checked above");
        Self::log_signal_path(&prepared, output, self.snapshot.volume);
        let session = previous.continue_with(prepared);
        self.segments.push_back(Segment {
            track: session.prepared.track.clone(),
            start_sample: boundary,
            end_sample: None,
            base_ms: 0,
            pcm_rate: session.prepared.decoder.pcm_spec().rate,
            rate_selection: session.prepared.rate_selection(),
        });
        self.next = None;
        self.session = Some(session);
        output.state.ending.store(false, Ordering::Relaxed);
        self.drain_at = None;
        Ok(true)
    }
    fn produce(&mut self) -> Result<()> {
        let started = Instant::now();
        for _ in 0..32 {
            if self.stop.load(Ordering::Relaxed) {
                break;
            }
            if self.session.is_none() {
                self.prepare_next();
                if !self.append_compatible_prepared()? {
                    break;
                }
            }
            let Some(output) = &self.output else {
                break;
            };
            if output.vacant() < output.channels {
                break;
            }
            if let Some(session) = &mut self.session
                && session.pending_offset == session.pending.len()
                && !session.eof
            {
                session.fill()?;
            }
            if self.session.as_ref().is_some_and(|session| {
                session.eof && session.pending_offset == session.pending.len()
            }) {
                self.prepare_next();
                if self.continue_converter_into_next()? {
                    continue;
                }
                let session = self.session.as_mut().expect("session active");
                if !session.flushed {
                    session.converter.convert(&[], true, &mut session.pending)?;
                    session.pending_offset = 0;
                    session.flushed = true;
                }
                if session.pending.is_empty() || session.pending_offset == session.pending.len() {
                    let channels =
                        self.output.as_ref().expect("session has output").channels as u64;
                    let boundary =
                        session.converter_origin + session.converter.target_frames()? * channels;
                    if boundary != self.written {
                        bail!("Resampler final frame count did not match track boundary");
                    }
                    if let Some(segment) = self.segments.back_mut() {
                        segment.end_sample = Some(boundary);
                    }
                    self.session = None;
                    if self.append_compatible_prepared()? {
                        continue;
                    }
                    if let Some(output) = &self.output {
                        output.state.ending.store(true, Ordering::Relaxed);
                    }
                    break;
                }
            }
            let output = self.output.as_mut().expect("output active");
            let session = self.session.as_mut().expect("session active");
            let count = output.write(&session.pending[session.pending_offset..]);
            self.written += count as u64;
            session.pending_offset += count;
            if count == 0 && !session.pending.is_empty() {
                break;
            }
        }
        if started.elapsed() >= Duration::from_millis(50) {
            tracing::warn!(
                elapsed_ms = started.elapsed().as_millis(),
                queued_ms = self.queued_ms(),
                "Audio producer stalled"
            );
        }
        Ok(())
    }
    fn tick(&mut self) -> Result<()> {
        if let Some(output) = &mut self.output {
            output.log_errors();
        }
        if self
            .output
            .as_ref()
            .is_some_and(|output| output.state.failed.load(Ordering::Relaxed))
        {
            let mut recovery = self.current_recovery();
            if let Some(recovery) = &mut recovery {
                self.snapshot.position_ms = recovery.position_ms;
                recovery.paused = true;
            }
            self.reset();
            self.recovery = recovery;
            self.snapshot.state = PlaybackState::Paused;
            self.snapshot.error = None;
            self.snapshot.notification =
                Some("Audio output device changed. Playback paused.".into());
            tracing::warn!("Audio output became unavailable; playback paused");
            self.publish();
            return Ok(());
        }
        self.produce()?;
        self.prepare_next();
        if self.snapshot.state == PlaybackState::Paused {
            return Ok(());
        }
        let consumed = self.output.as_ref().map_or(0, Output::consumed);
        loop {
            if !self
                .segments
                .front()
                .and_then(|segment| segment.end_sample)
                .is_some_and(|end| consumed >= end)
            {
                break;
            }
            if self.segments.len() == 1 {
                let output = self.output.as_ref().expect("segment has output");
                let latency = Duration::from_nanos(output.state.latency_ns.load(Ordering::Relaxed));
                let drain_at = self
                    .drain_at
                    .get_or_insert_with(|| Instant::now() + latency);
                if Instant::now() < *drain_at {
                    break;
                }
            }
            self.segments.pop_front();
            self.snapshot.completed = self.snapshot.completed.saturating_add(1);
            if let Some(segment) = self.segments.front() {
                self.snapshot.track = Some(segment.track.clone());
                self.snapshot.source_rate = segment.track.metadata.sample_rate;
                self.snapshot.duration_ms = segment.track.metadata.duration_ms;
                self.snapshot.position_ms = segment.base_ms;
                tracing::info!(track_id = segment.track.id, "Advanced to prefetched track");
            } else if let Some(prepared) = self.prepared.take() {
                self.reset_output();
                self.next = None;
                self.load(prepared, None, 0, false)?;
                self.publish();
                return Ok(());
            } else {
                self.snapshot.position_ms = self.snapshot.duration_ms;
                self.snapshot.state = PlaybackState::Stopped;
                self.reset_output();
            }
            self.publish();
        }
        self.snapshot.position_ms = self.position();
        Ok(())
    }
    fn position(&self) -> u64 {
        match (self.segments.front(), self.output.as_ref()) {
            (Some(segment), Some(output)) => {
                let elapsed = output.consumed().saturating_sub(segment.start_sample) * 1000
                    / output.channels as u64
                    / u64::from(output.rate);
                let position = segment.base_ms.saturating_add(elapsed);
                if segment.track.metadata.duration_ms > 0 {
                    position.min(segment.track.metadata.duration_ms)
                } else {
                    position
                }
            }
            _ => self.snapshot.position_ms,
        }
    }
    fn queued_ms(&self) -> u64 {
        self.output.as_ref().map_or(0, |output| {
            self.written.saturating_sub(output.consumed()) * 1000
                / output.channels as u64
                / u64::from(output.rate)
        })
    }
    fn reset_output(&mut self) {
        if let Some(output) = self.output.take() {
            self.total_underruns += output.state.underruns.load(Ordering::Relaxed)
                + output.state.device_underruns.load(Ordering::Relaxed);
        }
        self.written = 0;
        self.drain_at = None;
        self.snapshot.output_rate = 0;
        self.snapshot.device.clear();
    }
    fn reset(&mut self) {
        self.reset_output();
        self.session = None;
        self.next = None;
        self.prepared = None;
        self.segments.clear();
        self.recovery = None;
    }
    fn fail(&mut self, error: anyhow::Error) {
        tracing::error!(%error, "Audio operation failed");
        let recovery = if self.output.is_some() {
            self.current_recovery()
        } else {
            self.recovery.clone().or_else(|| self.current_recovery())
        };
        if let Some(recovery) = &recovery {
            self.snapshot.position_ms = recovery.position_ms;
        }
        self.reset();
        self.recovery = recovery;
        self.snapshot.state = PlaybackState::Error;
        self.snapshot.error = Some(error.to_string());
        self.snapshot.notification = None;
    }
    fn publish(&mut self) {
        self.snapshot.position_ms = self.position();
        if let Some(output) = &self.output {
            let gap = output.state.max_callback_gap_ns.swap(0, Ordering::Relaxed);
            if gap > 0 {
                tracing::warn!(
                    device = %output.device,
                    rate = output.rate,
                    callback_gap_ms = gap as f64 / 1_000_000.0,
                    position_ms = self.snapshot.position_ms,
                    queued_ms = self.queued_ms(),
                    "Audio device callback delayed"
                );
            }
            if self.snapshot.state == PlaybackState::Playing
                && self.last_diagnostics.elapsed() >= Duration::from_secs(5)
            {
                let minimum = output
                    .state
                    .min_queued_samples
                    .swap(u64::MAX, Ordering::Relaxed);
                let max_interval = output
                    .state
                    .max_callback_interval_ns
                    .swap(0, Ordering::Relaxed);
                tracing::info!(
                    position_ms = self.snapshot.position_ms,
                    queued_ms = self.queued_ms(),
                    min_queued_ms = (minimum != u64::MAX).then(|| {
                        minimum as f64 * 1000.0 / output.channels as f64 / f64::from(output.rate)
                    }),
                    max_callback_interval_ms = max_interval as f64 / 1_000_000.0,
                    callbacks = output.state.callbacks.load(Ordering::Relaxed),
                    buffer_underruns = output.state.underruns.load(Ordering::Relaxed),
                    device_underruns = output.state.device_underruns.load(Ordering::Relaxed),
                    device = %output.device,
                    rate = output.rate,
                    "Audio playback diagnostics"
                );
                self.last_diagnostics = Instant::now();
            }
        }
        let underruns = self.total_underruns
            + self.output.as_ref().map_or(0, |output| {
                output.state.underruns.load(Ordering::Relaxed)
                    + output.state.device_underruns.load(Ordering::Relaxed)
            });
        if underruns != self.snapshot.underruns {
            tracing::warn!(
                underruns,
                position_ms = self.snapshot.position_ms,
                buffer_underruns = self.output.as_ref().map_or(0, |output| output.state.underruns.load(Ordering::Relaxed)),
                device_underruns = self.output.as_ref().map_or(0, |output| output.state.device_underruns.load(Ordering::Relaxed)),
                queued_ms = self.queued_ms(),
                device = %self.snapshot.device,
                rate = self.snapshot.output_rate,
                "Audio underrun counter changed"
            );
            self.snapshot.underruns = underruns;
        }
        self.sender.send_replace(self.snapshot.clone());
        (self.wake)();
        self.last_publish = Instant::now();
    }
}
fn command_name(command: &AudioCommand) -> &'static str {
    match command {
        AudioCommand::Load { .. } => "load",
        AudioCommand::SetNext(_) => "set_next",
        AudioCommand::Pause => "pause",
        AudioCommand::Resume => "resume",
        AudioCommand::Stop { .. } => "stop",
        AudioCommand::Seek { .. } => "seek",
        AudioCommand::SetVolume(_) => "volume",
        AudioCommand::SetDevice(_) => "device",
        AudioCommand::Shutdown => "shutdown",
    }
}

#[cfg(test)]
#[path = "../tests/unit/playback.rs"]
mod tests;
