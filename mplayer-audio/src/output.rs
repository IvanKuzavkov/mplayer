use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
};
use std::time::{Duration, Instant};

use crate::{dsp::Converter, pcm::PcmFormat};
use cpal::{
    FromSample, SampleFormat, SizedSample, StreamConfig, SupportedStreamConfig,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use mplayer_core::{OutputDevice, OutputDeviceCapabilities, Result};
use ringbuf::{HeapCons, HeapProd, HeapRb, traits::*};

#[derive(Default)]
pub(crate) struct CallbackState {
    pub consumed: AtomicU64,
    pub underruns: AtomicU64,
    pub device_underruns: AtomicU64,
    pub callbacks: AtomicU64,
    pub max_callback_gap_ns: AtomicU64,
    pub max_callback_interval_ns: AtomicU64,
    pub min_queued_samples: AtomicU64,
    pub failed: AtomicBool,
    pub ending: AtomicBool,
    pub latency_ns: AtomicU64,
    pub paused: AtomicBool,
    pub volume: AtomicU32,
}

impl CallbackState {
    fn stream_error(&self, error: &cpal::Error) {
        match error.kind() {
            cpal::ErrorKind::Xrun => {
                self.device_underruns.fetch_add(1, Ordering::Relaxed);
            }
            cpal::ErrorKind::RealtimeDenied | cpal::ErrorKind::DeviceChanged => (),
            _ => self.failed.store(true, Ordering::Relaxed),
        }
    }

    fn callback_gap(&self, elapsed: Duration, expected: Duration) {
        self.max_callback_interval_ns
            .fetch_max(elapsed.as_nanos() as u64, Ordering::Relaxed);
        // Ignore ordinary scheduling jitter; retain only a clearly missed buffer.
        if elapsed > expected * 2 + Duration::from_millis(2) {
            self.max_callback_gap_ns
                .fetch_max(elapsed.as_nanos() as u64, Ordering::Relaxed);
        }
    }
}

/// All allocation and native stream lifecycle operations stay on the audio worker.
pub(crate) struct Output {
    _stream: Option<cpal::Stream>,
    producer: HeapProd<f64>,
    errors: HeapCons<cpal::Error>,
    pub state: Arc<CallbackState>,
    pub rate: u32,
    pub channels: usize,
    pub device: String,
    pub sample_format: SampleFormat,
}

pub(crate) fn devices() -> Vec<OutputDevice> {
    cpal::default_host()
        .output_devices()
        .map(|devices| {
            devices
                .map(|device| OutputDevice {
                    id: device_label(&device),
                    capabilities: device
                        .supported_output_configs()
                        .ok()
                        .and_then(capabilities),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn capabilities(
    configs: impl IntoIterator<Item = cpal::SupportedStreamConfigRange>,
) -> Option<OutputDeviceCapabilities> {
    configs
        .into_iter()
        .filter(|config| {
            supported_sample_format(config.sample_format())
                && config.channels() > 0
                && config.max_sample_rate() > 0
                && config.max_sample_rate() < u32::MAX
                && config.min_sample_rate() <= config.max_sample_rate()
        })
        .max_by_key(|config| {
            (
                config.max_sample_rate(),
                config.sample_format().bits_per_sample(),
                !config.sample_format().is_float(),
                config.channels(),
            )
        })
        .map(|config| OutputDeviceCapabilities {
            sample_rate: config.max_sample_rate(),
            bits_per_sample: config.sample_format().bits_per_sample() as u16,
            channels: config.channels(),
            floating_point: config.sample_format().is_float(),
        })
}

fn device_label(device: &cpal::Device) -> String {
    match device.id() {
        Ok(id) => format!("{device} [{id}]"),
        Err(_) => device.to_string(),
    }
}

fn supported_sample_format(format: SampleFormat) -> bool {
    matches!(
        format,
        SampleFormat::F32
            | SampleFormat::F64
            | SampleFormat::I8
            | SampleFormat::I16
            | SampleFormat::I24
            | SampleFormat::I32
            | SampleFormat::I64
            | SampleFormat::U8
            | SampleFormat::U16
            | SampleFormat::U32
            | SampleFormat::U64
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RateSelection {
    Native,
    Highest,
}

fn matches_output_config(
    config: &cpal::SupportedStreamConfigRange,
    rate: u32,
    source_channels: usize,
) -> bool {
    let channels = usize::from(config.channels());
    rate > 0
        && (channels == source_channels || (source_channels == 1 && channels == 2))
        && config.min_sample_rate() <= rate
        && config.max_sample_rate() >= rate
        && supported_sample_format(config.sample_format())
}

fn output_configs(
    configs: impl IntoIterator<Item = cpal::SupportedStreamConfigRange>,
    source_rate: u32,
    source_channels: usize,
    source_format: PcmFormat,
    selection: RateSelection,
) -> Vec<SupportedStreamConfig> {
    let target_rate = |config: &cpal::SupportedStreamConfigRange| match selection {
        RateSelection::Native => source_rate,
        // Match the playback converter's maximum supported rate.
        RateSelection::Highest => config.max_sample_rate().min(768_000),
    };
    let configs: Vec<_> = configs
        .into_iter()
        .filter(|config| matches_output_config(config, target_rate(config), source_channels))
        .collect();
    let mut rates: Vec<_> = configs.iter().map(target_rate).collect();
    rates.sort_unstable();
    rates.dedup();
    let mut candidates = Vec::new();
    for rate in rates {
        if Converter::validate_rates(source_rate, rate).is_err() {
            continue;
        }
        // A lower-rate retry must reconsider every format supporting that rate,
        // not just formats whose advertised maximum happens to equal it.
        candidates.extend(
            configs
                .iter()
                .filter(|config| matches_output_config(config, rate, source_channels))
                .map(|config| config.with_sample_rate(rate)),
        );
    }
    candidates.sort_by_key(|config| {
        (
            std::cmp::Reverse(config.sample_rate()),
            usize::from(config.channels()) != source_channels
                || !source_format.preserved_by(config.sample_format()),
            usize::from(config.channels()) != source_channels,
            source_format.preference(config.sample_format()),
        )
    });
    candidates
}

fn playback_buffer_size(rate: u32, supported: &cpal::SupportedBufferSize) -> cpal::BufferSize {
    match *supported {
        cpal::SupportedBufferSize::Range { min, max } if max > 0 && min <= max => {
            // Music playback does not need a low-latency monitoring period.
            // ALSA uses about two periods; 20 ms leaves more scheduling headroom
            // than the native device's commonly selected 5 ms default.
            cpal::BufferSize::Fixed(rate.div_ceil(50).clamp(min.max(1), max))
        }
        _ => cpal::BufferSize::Default,
    }
}

impl Output {
    #[tracing::instrument(skip_all, fields(source_rate, source_channels, ?rate_selection, selected))]
    pub fn open(
        source_rate: u32,
        source_channels: usize,
        source_format: PcmFormat,
        rate_selection: RateSelection,
        selected: Option<&str>,
        volume: f32,
    ) -> Result<Self> {
        let host = cpal::default_host();
        let device = if let Some(selected) = selected {
            host.output_devices()?
                .find(|device| device_label(device) == selected)
                .ok_or(anyhow::anyhow!(
                    "Selected output device is unavailable; select another device"
                ))?
        } else {
            host.default_output_device()
                .ok_or(anyhow::anyhow!("No output device is available"))?
        };
        let candidates = output_configs(
            device.supported_output_configs()?,
            source_rate,
            source_channels,
            source_format,
            rate_selection,
        );
        for config in candidates {
            let requested_rate = config.sample_rate();
            match Self::build(&device, config, volume) {
                Ok(output) => return Ok(output),
                Err(error) => {
                    tracing::debug!(%error, requested_rate, "Requested-rate output configuration rejected");
                }
            }
        }
        let default = device.default_output_config()?;
        Converter::validate_rates(source_rate, default.sample_rate())?;
        let channels = usize::from(default.channels());
        if channels > 2 || channels == 0 || (source_channels == 2 && channels == 1) {
            anyhow::bail!("Output device has no supported mono/stereo configuration");
        }
        tracing::info!(
            source_rate,
            output_rate = default.sample_rate(),
            "Using default output rate with resampling when needed"
        );
        Self::build(&device, default, volume)
    }

    fn build(device: &cpal::Device, supported: SupportedStreamConfig, volume: f32) -> Result<Self> {
        let config = supported.config();
        let sample_format = supported.sample_format();
        let buffer_size = playback_buffer_size(config.sample_rate, supported.buffer_size());
        if buffer_size != cpal::BufferSize::Default {
            match Self::build_config(
                device,
                StreamConfig {
                    buffer_size,
                    ..config
                },
                sample_format,
                volume,
            ) {
                Ok(output) => return Ok(output),
                Err(error) => tracing::warn!(
                    %error,
                    ?buffer_size,
                    "Playback buffer request rejected; retrying device default"
                ),
            }
        }
        Self::build_config(device, config, sample_format, volume)
    }

    fn build_config(
        device: &cpal::Device,
        config: StreamConfig,
        sample_format: SampleFormat,
        volume: f32,
    ) -> Result<Self> {
        let rate = config.sample_rate;
        let channels = usize::from(config.channels);
        let capacity = (rate as usize / 2).max(4096) * channels;
        let (producer, consumer) = HeapRb::<f64>::new(capacity).split();
        let state = Arc::new(CallbackState::default());
        state.paused.store(true, Ordering::Relaxed);
        state.volume.store(volume.to_bits(), Ordering::Relaxed);
        state.min_queued_samples.store(u64::MAX, Ordering::Relaxed);
        let (stream, errors) = match sample_format {
            SampleFormat::F32 => build_stream::<f32>(device, config, consumer, state.clone()),
            SampleFormat::F64 => build_stream::<f64>(device, config, consumer, state.clone()),
            SampleFormat::I8 => build_stream::<i8>(device, config, consumer, state.clone()),
            SampleFormat::I16 => build_stream::<i16>(device, config, consumer, state.clone()),
            SampleFormat::I24 => build_stream::<cpal::I24>(device, config, consumer, state.clone()),
            SampleFormat::I32 => build_stream::<i32>(device, config, consumer, state.clone()),
            SampleFormat::I64 => build_stream::<i64>(device, config, consumer, state.clone()),
            SampleFormat::U8 => build_stream::<u8>(device, config, consumer, state.clone()),
            SampleFormat::U16 => build_stream::<u16>(device, config, consumer, state.clone()),
            SampleFormat::U32 => build_stream::<u32>(device, config, consumer, state.clone()),
            SampleFormat::U64 => build_stream::<u64>(device, config, consumer, state.clone()),
            format => anyhow::bail!(format!("Unsupported output sample format: {format}")),
        }?;
        let period_frames = stream.buffer_size().ok();
        stream.play()?;
        tracing::info!(
            rate,
            channels,
            %sample_format,
            device = %device,
            requested_buffer = ?config.buffer_size,
            period_frames,
            period_ms = period_frames.map(|frames| f64::from(frames) * 1000.0 / f64::from(rate)),
            "Opened audio output"
        );
        Ok(Self {
            _stream: Some(stream),
            producer,
            errors,
            state,
            rate,
            channels,
            device: device_label(device),
            sample_format,
        })
    }

    pub fn write(&mut self, samples: &[f64]) -> usize {
        // Only whole frames are published so an underrun cannot swap stereo channels.
        let count = self.producer.vacant_len().min(samples.len()) / self.channels * self.channels;
        self.producer.push_slice(&samples[..count])
    }

    pub fn vacant(&self) -> usize {
        self.producer.vacant_len()
    }

    pub fn consumed(&self) -> u64 {
        self.state.consumed.load(Ordering::Relaxed)
    }

    pub fn log_errors(&mut self) {
        // Error callbacks may run on the real-time thread. Drain their bounded,
        // lock-free queue here so logging and formatting stay on the worker.
        while let Some(error) = self.errors.try_pop() {
            if error.kind() == cpal::ErrorKind::DeviceChanged {
                tracing::info!(%error, device = %self.device, "Audio output rerouted by the system");
            } else {
                tracing::warn!(%error, kind = ?error.kind(), device = %self.device, rate = self.rate, "Audio output stream event");
            }
        }
    }

    pub fn pause(&self, paused: bool) {
        self.state.paused.store(paused, Ordering::Relaxed);
    }

    pub fn set_volume(&self, volume: f32) {
        self.state.volume.store(volume.to_bits(), Ordering::Relaxed);
    }

    #[cfg(test)]
    pub fn fake(rate: u32, channels: usize) -> (Self, HeapCons<f64>) {
        let (producer, consumer) = HeapRb::new(rate as usize / 2 * channels).split();
        let (_, errors) = HeapRb::new(16).split();
        let state = Arc::new(CallbackState::default());
        state.volume.store(1.0_f32.to_bits(), Ordering::Relaxed);
        state.min_queued_samples.store(u64::MAX, Ordering::Relaxed);
        (
            Self {
                _stream: None,
                producer,
                errors,
                state,
                rate,
                channels,
                device: "Test sink".into(),
                sample_format: SampleFormat::F64,
            },
            consumer,
        )
    }
}

#[cfg(test)]
pub(crate) fn test_consume<T: SizedSample + FromSample<f64>>(
    consumer: &mut HeapCons<f64>,
    output: &mut [T],
    channels: usize,
    state: &CallbackState,
) {
    render(consumer, output, channels, state);
}

fn build_stream<T: SizedSample + FromSample<f64>>(
    device: &cpal::Device,
    config: StreamConfig,
    mut consumer: HeapCons<f64>,
    state: Arc<CallbackState>,
) -> Result<(cpal::Stream, HeapCons<cpal::Error>)> {
    let (mut errors, error_receiver) = HeapRb::new(16).split();
    let failed_state = state.clone();
    let channels = usize::from(config.channels);
    let mut previous_callback = None;
    let stream = device.build_output_stream(
        config,
        move |data: &mut [T], info| {
            let timestamp = info.timestamp();
            let queued = timestamp
                .playback
                .saturating_duration_since(timestamp.callback);
            let callback_ns =
                data.len() as u64 * 1_000_000_000 / u64::from(config.sample_rate) / channels as u64;
            if state.paused.load(Ordering::Relaxed) {
                previous_callback = None;
            } else {
                let now = Instant::now();
                if let Some((previous, expected)) = previous_callback {
                    state.callback_gap(now.saturating_duration_since(previous), expected);
                }
                previous_callback = Some((now, Duration::from_nanos(callback_ns)));
                state.callbacks.fetch_add(1, Ordering::Relaxed);
            }
            state.latency_ns.store(
                (queued.as_nanos() as u64).saturating_add(callback_ns),
                Ordering::Relaxed,
            );
            render(&mut consumer, data, channels, &state);
        },
        move |error| {
            failed_state.stream_error(&error);
            let _ = errors.try_push(error);
        },
        None,
    )?;
    Ok((stream, error_receiver))
}

/// Real-time callback: no allocation, locks, logging, I/O, or channel sends.
fn render<T: SizedSample + FromSample<f64>>(
    consumer: &mut HeapCons<f64>,
    output: &mut [T],
    channels: usize,
    state: &CallbackState,
) {
    if state.paused.load(Ordering::Relaxed) {
        output.fill(T::from_sample(0.0));
        return;
    }
    let queued = consumer.occupied_len();
    state
        .min_queued_samples
        .fetch_min(queued as u64, Ordering::Relaxed);
    let available = queued.min(output.len()) / channels * channels;
    let volume = f32::from_bits(state.volume.load(Ordering::Relaxed)) as f64;
    let float_output = T::FORMAT.is_float();
    let maximum = if float_output {
        1.0
    } else {
        // I24 conversion constructs a packed integer without saturation. Never
        // pass +1.0 (or NaN) to it; +1 is outside normalized integer PCM.
        let bits = T::FORMAT.bits_per_sample() as i32;
        (1.0 - 2.0_f64.powi(1 - bits)).min(1.0_f64.next_down())
    };
    let (first, second) = consumer.as_slices();
    let mut sound_output = first.iter().chain(second).map(|v| {
        let v = if !v.is_finite() {
            0.0
        } else if volume == 1.0 && float_output {
            *v
        } else {
            (if volume == 1.0 { *v } else { *v * volume }).clamp(-1.0, maximum)
        };
        T::from_sample(v)
    });
    output[..available]
        .iter_mut()
        .for_each(|v| *v = sound_output.next().unwrap_or_else(|| T::from_sample(0.0)));

    consumer.skip(available);
    output[available..].fill(T::from_sample(0.0));
    state
        .consumed
        .fetch_add(available as u64, Ordering::Relaxed);
    if available < output.len() && !state.ending.load(Ordering::Relaxed) {
        state.underruns.fetch_add(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
#[path = "../tests/unit/output.rs"]
mod tests;
