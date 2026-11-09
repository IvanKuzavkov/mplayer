use super::*;

fn output_range(
    channels: u16,
    min: u32,
    max: u32,
    format: SampleFormat,
) -> cpal::SupportedStreamConfigRange {
    cpal::SupportedStreamConfigRange::new(
        channels,
        min,
        max,
        cpal::SupportedBufferSize::Unknown,
        format,
    )
}

#[test]
fn dsd_highest_output_prioritizes_rate_then_precision_without_forcing_i16() {
    let configs = output_configs(
        [
            output_range(2, 176_400, 176_400, SampleFormat::F64),
            output_range(2, 44_100, 384_000, SampleFormat::I16),
            output_range(2, 44_100, 192_000, SampleFormat::F64),
            output_range(2, 44_100, 384_000, SampleFormat::I32),
        ],
        176_400,
        2,
        PcmFormat::Float(64),
        RateSelection::Highest,
    );
    assert_eq!(
        configs
            .iter()
            .map(|config| (config.sample_rate(), config.sample_format()))
            .collect::<Vec<_>>(),
        [
            (384_000, SampleFormat::I32),
            (384_000, SampleFormat::I16),
            (192_000, SampleFormat::F64),
            (192_000, SampleFormat::I32),
            (192_000, SampleFormat::I16),
            (176_400, SampleFormat::F64),
            (176_400, SampleFormat::F64),
            (176_400, SampleFormat::I32),
            (176_400, SampleFormat::I16),
        ]
    );
}

#[test]
fn highest_output_prefers_f64_at_equal_highest_rate() {
    let configs = output_configs(
        [
            output_range(2, 44_100, 384_000, SampleFormat::I16),
            output_range(2, 44_100, 384_000, SampleFormat::I32),
            output_range(2, 44_100, 384_000, SampleFormat::F64),
        ],
        176_400,
        2,
        PcmFormat::Float(64),
        RateSelection::Highest,
    );
    assert_eq!(
        configs
            .iter()
            .map(|config| (config.sample_rate(), config.sample_format()))
            .collect::<Vec<_>>(),
        [
            (384_000, SampleFormat::F64),
            (384_000, SampleFormat::I32),
            (384_000, SampleFormat::I16),
        ]
    );
}

#[test]
fn highest_output_ranks_all_compatible_formats_at_each_advertised_rate() {
    let configs = output_configs(
        [
            output_range(2, 44_100, 384_000, SampleFormat::I32),
            output_range(2, 44_100, 768_000, SampleFormat::F64),
        ],
        176_400,
        2,
        PcmFormat::Float(64),
        RateSelection::Highest,
    );
    assert!(
        configs
            .iter()
            .map(|config| (config.sample_rate(), config.sample_format()))
            .eq([
                (768_000, SampleFormat::F64),
                (384_000, SampleFormat::F64),
                (384_000, SampleFormat::I32),
            ])
    );
}

#[test]
fn highest_output_skips_invalid_conversion_rates_and_keeps_best_fallback_format() {
    let configs = output_configs(
        [
            output_range(2, 44_100, 192_001, SampleFormat::F64),
            output_range(2, 44_100, 176_400, SampleFormat::I16),
            output_range(2, 44_100, 176_400, SampleFormat::I32),
        ],
        176_400,
        2,
        PcmFormat::Float(64),
        RateSelection::Highest,
    );
    assert!(
        configs
            .iter()
            .map(|config| (config.sample_rate(), config.sample_format()))
            .eq([
                (176_400, SampleFormat::F64),
                (176_400, SampleFormat::I32),
                (176_400, SampleFormat::I16),
            ])
    );
}

#[test]
fn highest_output_rejects_incompatible_channels_and_unrenderable_formats() {
    let configs = output_configs(
        [
            output_range(1, 44_100, 768_000, SampleFormat::F64),
            output_range(0, 44_100, 768_000, SampleFormat::I32),
            output_range(6, 44_100, 768_000, SampleFormat::I32),
            output_range(2, 44_100, 768_000, SampleFormat::DsdU8),
            output_range(2, 44_100, 768_000, SampleFormat::DsdU16),
            output_range(2, 44_100, 768_000, SampleFormat::DsdU32),
            output_range(2, 44_100, 768_000, SampleFormat::U24),
            output_range(2, 44_100, 384_000, SampleFormat::I32),
        ],
        176_400,
        2,
        PcmFormat::Float(64),
        RateSelection::Highest,
    );
    assert_eq!(configs.len(), 1);
    assert_eq!(configs[0].sample_rate(), 384_000);
    assert_eq!(configs[0].channels(), 2);
    assert_eq!(configs[0].sample_format(), SampleFormat::I32);
}

#[test]
fn highest_output_allows_mono_to_stereo_but_prefers_native_channels_at_equal_rate() {
    let configs = output_configs(
        [
            output_range(2, 44_100, 384_000, SampleFormat::I32),
            output_range(1, 44_100, 384_000, SampleFormat::I32),
        ],
        176_400,
        1,
        PcmFormat::Float(64),
        RateSelection::Highest,
    );
    assert_eq!(
        configs
            .iter()
            .map(|config| (config.sample_rate(), config.channels()))
            .collect::<Vec<_>>(),
        [(384_000, 1), (384_000, 2)]
    );
}

#[test]
fn native_pcm_output_keeps_source_rate_when_higher_rates_are_available() {
    let configs = output_configs(
        [
            output_range(2, 44_100, 384_000, SampleFormat::I32),
            output_range(2, 384_000, 384_000, SampleFormat::F64),
            output_range(2, 44_100, 192_000, SampleFormat::F64),
        ],
        44_100,
        2,
        PcmFormat::Integer(16),
        RateSelection::Native,
    );
    assert_eq!(
        configs
            .iter()
            .map(|config| config.sample_rate())
            .collect::<Vec<_>>(),
        [44_100, 44_100]
    );
}

#[test]
fn native_output_without_a_matching_rate_leaves_fallback_to_the_caller() {
    assert!(
        output_configs(
            [output_range(2, 48_000, 384_000, SampleFormat::F64)],
            44_100,
            2,
            PcmFormat::Integer(16),
            RateSelection::Native,
        )
        .is_empty()
    );
}

#[test]
fn highest_output_caps_unbounded_rates_and_skips_invalid_ranges() {
    let configs = output_configs(
        [
            output_range(2, 44_100, u32::MAX, SampleFormat::I32),
            output_range(2, 768_001, u32::MAX, SampleFormat::F64),
            output_range(2, 384_000, 176_400, SampleFormat::F64),
            output_range(2, 0, 0, SampleFormat::F64),
            output_range(2, 1, 0, SampleFormat::F64),
        ],
        176_400,
        2,
        PcmFormat::Float(64),
        RateSelection::Highest,
    );
    assert_eq!(configs.len(), 1);
    assert_eq!(configs[0].sample_rate(), 768_000);
    assert_eq!(configs[0].sample_format(), SampleFormat::I32);
}

#[test]
fn native_output_rejects_zero_source_rate() {
    assert!(
        output_configs(
            [output_range(2, 0, 384_000, SampleFormat::F64)],
            0,
            2,
            PcmFormat::Integer(16),
            RateSelection::Native,
        )
        .is_empty()
    );
}

#[test]
fn output_config_matching_requires_a_positive_rate() {
    let config = output_range(2, 0, 384_000, SampleFormat::I32);
    assert!(!matches_output_config(&config, 0, 2));
    assert!(matches_output_config(&config, 1, 2));
}

#[test]
fn output_config_matching_requires_the_requested_rate() {
    for (min, max, expected) in [
        (176_400, 176_400, false),
        (44_100, 176_400, false),
        (384_001, 768_000, false),
        (384_000, 384_000, true),
        (384_000, 768_000, true),
        (44_100, 384_000, true),
        (44_100, 768_000, true),
    ] {
        let config = cpal::SupportedStreamConfigRange::new(
            2,
            min,
            max,
            cpal::SupportedBufferSize::Unknown,
            SampleFormat::I32,
        );
        assert_eq!(
            matches_output_config(&config, 384_000, 2),
            expected,
            "range: {min}..={max}"
        );
    }
}

#[test]
fn output_config_matching_preserves_mono_and_stereo_rules() {
    for (source_channels, output_channels, expected) in [
        (1, 1, true),
        (1, 2, true),
        (2, 2, true),
        (2, 1, false),
        (1, 0, false),
        (2, 0, false),
        (1, 6, false),
        (2, 6, false),
    ] {
        let config = cpal::SupportedStreamConfigRange::new(
            output_channels,
            384_000,
            384_000,
            cpal::SupportedBufferSize::Unknown,
            SampleFormat::I32,
        );
        assert_eq!(
            matches_output_config(&config, 384_000, source_channels),
            expected,
            "source channels: {source_channels}, output channels: {output_channels}"
        );
    }
}

#[test]
fn output_config_matching_accepts_only_renderable_sample_formats() {
    for (format, expected) in [
        (SampleFormat::I8, true),
        (SampleFormat::I16, true),
        (SampleFormat::I24, true),
        (SampleFormat::I32, true),
        (SampleFormat::I64, true),
        (SampleFormat::U8, true),
        (SampleFormat::U16, true),
        (SampleFormat::U24, false),
        (SampleFormat::U32, true),
        (SampleFormat::U64, true),
        (SampleFormat::F32, true),
        (SampleFormat::F64, true),
        (SampleFormat::DsdU8, false),
        (SampleFormat::DsdU16, false),
        (SampleFormat::DsdU32, false),
    ] {
        let config = cpal::SupportedStreamConfigRange::new(
            2,
            384_000,
            384_000,
            cpal::SupportedBufferSize::Unknown,
            format,
        );
        assert_eq!(
            matches_output_config(&config, 384_000, 2),
            expected,
            "format: {format:?}"
        );
    }
}

#[test]
fn playback_buffer_targets_twenty_milliseconds_at_each_rate() {
    let supported = cpal::SupportedBufferSize::Range {
        min: 1,
        max: u32::MAX,
    };
    for (rate, frames) in [
        (44_100, 882),
        (48_000, 960),
        (176_400, 3_528),
        (192_000, 3_840),
        (44_101, 883),
        (48_001, 961),
        (u32::MAX, 85_899_346),
    ] {
        assert_eq!(
            playback_buffer_size(rate, &supported),
            cpal::BufferSize::Fixed(frames),
            "rate: {rate}"
        );
    }
}

#[test]
fn playback_buffer_clamps_to_valid_nonzero_bounds() {
    for (rate, min, max, frames) in [
        (48_000, 1_024, 4_096, 1_024),
        (192_000, 128, 2_048, 2_048),
        (48_000, 960, 2_048, 960),
        (48_000, 128, 960, 960),
        (48_000, 512, 512, 512),
        (48_000, 0, 4_096, 960),
        (0, 0, 4_096, 1),
        (1, 0, 4_096, 1),
        (0, 128, 4_096, 128),
        (48_000, 0, 1, 1),
    ] {
        let supported = cpal::SupportedBufferSize::Range { min, max };
        assert_eq!(
            playback_buffer_size(rate, &supported),
            cpal::BufferSize::Fixed(frames),
            "rate: {rate}, supported: {supported:?}"
        );
    }
}

#[test]
fn playback_buffer_uses_default_for_unknown_or_invalid_bounds() {
    for supported in [
        cpal::SupportedBufferSize::Unknown,
        cpal::SupportedBufferSize::Range { min: 0, max: 0 },
        cpal::SupportedBufferSize::Range { min: 1, max: 0 },
        cpal::SupportedBufferSize::Range {
            min: 1_024,
            max: 512,
        },
    ] {
        for rate in [0, 44_100, 48_000, 176_400, 192_000, u32::MAX] {
            assert_eq!(
                playback_buffer_size(rate, &supported),
                cpal::BufferSize::Default,
                "rate: {rate}, supported: {supported:?}"
            );
        }
    }
}

#[test]
fn recoverable_device_events_keep_the_stream_active() {
    let state = CallbackState::default();
    for kind in [
        cpal::ErrorKind::Xrun,
        cpal::ErrorKind::RealtimeDenied,
        cpal::ErrorKind::DeviceChanged,
    ] {
        state.stream_error(&kind.into());
        assert!(!state.failed.load(Ordering::Relaxed), "{kind:?}");
    }
    assert_eq!(state.device_underruns.load(Ordering::Relaxed), 1);
    assert_eq!(state.underruns.load(Ordering::Relaxed), 0);
    for kind in [
        cpal::ErrorKind::DeviceNotAvailable,
        cpal::ErrorKind::StreamInvalidated,
        cpal::ErrorKind::BackendError,
    ] {
        let state = CallbackState::default();
        state.stream_error(&kind.into());
        assert!(state.failed.load(Ordering::Relaxed), "{kind:?}");
    }
}

#[test]
fn callback_diagnostics_ignore_jitter_and_retain_the_longest_gap() {
    let state = CallbackState::default();
    let buffer = Duration::from_millis(3);
    state.callback_gap(Duration::from_millis(4), buffer);
    assert_eq!(state.max_callback_gap_ns.load(Ordering::Relaxed), 0);
    state.callback_gap(Duration::from_millis(100), buffer);
    state.callback_gap(Duration::from_millis(10), buffer);
    assert_eq!(
        state.max_callback_gap_ns.load(Ordering::Relaxed),
        100_000_000
    );
    // A legitimately larger callback buffer is not a missed deadline.
    let state = CallbackState::default();
    state.callback_gap(Duration::from_millis(25), Duration::from_millis(24));
    assert_eq!(state.max_callback_gap_ns.load(Ordering::Relaxed), 0);
}

#[test]
fn raw_callback_intervals_retain_jitter_below_the_warning_threshold() {
    let state = CallbackState::default();
    let buffer = Duration::from_millis(3);
    for (elapsed_ns, maximum_ns) in [
        (4_000_000, 4_000_000),
        (7_250_001, 7_250_001),
        (5_000_000, 7_250_001),
        (8_000_000, 8_000_000),
    ] {
        state.callback_gap(Duration::from_nanos(elapsed_ns), buffer);
        assert_eq!(
            state.max_callback_interval_ns.load(Ordering::Relaxed),
            maximum_ns
        );
        assert_eq!(state.max_callback_gap_ns.load(Ordering::Relaxed), 0);
    }
    state.callback_gap(Duration::from_millis(100), buffer);
    state.callback_gap(Duration::from_millis(10), buffer);
    assert_eq!(
        state.max_callback_interval_ns.load(Ordering::Relaxed),
        100_000_000
    );
    assert_eq!(
        state.max_callback_gap_ns.load(Ordering::Relaxed),
        100_000_000
    );
}

fn rendered<T: SizedSample + FromSample<f64>>(samples: &[f64]) -> Vec<T> {
    let (mut producer, mut consumer) = HeapRb::<f64>::new(samples.len()).split();
    producer.push_slice(samples);
    let state = CallbackState::default();
    state.volume.store(1.0_f32.to_bits(), Ordering::Relaxed);
    let mut output = vec![T::from_sample(0.0); samples.len()];
    render(&mut consumer, &mut output, 1, &state);
    output
}

#[test]
fn unity_volume_preserves_integer_and_float_precision() {
    let pcm16: Vec<_> = (i16::MIN..=i16::MAX).collect();
    let samples16: Vec<_> = pcm16.iter().map(|&v| f64::from(v) / 32_768.0).collect();
    assert_eq!(rendered::<i16>(&samples16), pcm16);
    let pcm24 = [-8_388_608, -8_388_607, -1, 0, 1, 8_388_606, 8_388_607];
    let samples24: Vec<_> = pcm24.iter().map(|&v| f64::from(v) / 8_388_608.0).collect();
    assert_eq!(
        rendered::<cpal::I24>(&samples24)
            .iter()
            .map(|v| v.inner())
            .collect::<Vec<_>>(),
        pcm24
    );
    for samples in [&samples16, &samples24] {
        assert_eq!(
            rendered::<f32>(samples)
                .into_iter()
                .map(f64::from)
                .collect::<Vec<_>>(),
            *samples
        );
    }
    let floats = [-2.0, -0.0, 0.0, f64::MIN_POSITIVE, 1.0 + f64::EPSILON, 2.0];
    assert_eq!(
        rendered::<f64>(&floats)
            .iter()
            .map(|v| v.to_bits())
            .collect::<Vec<_>>(),
        floats.map(f64::to_bits)
    );
}

#[test]
fn packed_integer_output_saturates_and_sanitizes_invalid_samples() {
    let samples = [-2.0, -1.0, 0.0, 1.0, 2.0, f64::NAN, f64::INFINITY];
    let output = rendered::<cpal::I24>(&samples);
    assert_eq!(
        output.iter().map(|v| v.inner()).collect::<Vec<_>>(),
        [-8_388_608, -8_388_608, 0, 8_388_607, 8_388_607, 0, 0]
    );
    assert_eq!(
        rendered::<u32>(&samples),
        [0, 0, 1 << 31, u32::MAX, u32::MAX, 1 << 31, 1 << 31]
    );
}

#[test]
fn callback_preserves_frames_and_fills_underruns_with_silence() {
    let (mut producer, mut consumer) = HeapRb::<f64>::new(16).split();
    producer.push_slice(&[0.25, -0.5, 1.0, -1.0]);
    let state = CallbackState::default();
    state.volume.store(0.5_f32.to_bits(), Ordering::Relaxed);
    let mut output = [99.0_f64; 8];
    render(&mut consumer, &mut output, 2, &state);
    assert_eq!(output, [0.125, -0.25, 0.5, -0.5, 0.0, 0.0, 0.0, 0.0]);
    assert_eq!(state.consumed.load(Ordering::Relaxed), 4);
    assert_eq!(state.underruns.load(Ordering::Relaxed), 1);
}

#[test]
fn paused_callback_does_not_consume_samples() {
    let (mut producer, mut consumer) = HeapRb::<f64>::new(16).split();
    producer.push_slice(&[0.25, -0.5]);
    let state = CallbackState::default();
    state.paused.store(true, Ordering::Relaxed);
    let mut output = [1.0_f64; 2];
    render(&mut consumer, &mut output, 2, &state);
    assert_eq!(output, [0.0; 2]);
    assert_eq!(consumer.occupied_len(), 2);
    assert_eq!(state.consumed.load(Ordering::Relaxed), 0);
}

#[test]
fn queue_low_watermark_records_samples_before_consumption_and_only_decreases() {
    let (mut output, mut consumer) = Output::fake(8_000, 2);
    assert_eq!(
        output.state.min_queued_samples.load(Ordering::Relaxed),
        u64::MAX
    );
    assert_eq!(output.write(&[0.25; 12]), 12);
    let mut samples = [0.0_f64; 4];
    render(&mut consumer, &mut samples, 2, &output.state);
    assert_eq!(consumer.occupied_len(), 8);
    assert_eq!(output.state.min_queued_samples.load(Ordering::Relaxed), 12);
    render(&mut consumer, &mut samples, 2, &output.state);
    assert_eq!(consumer.occupied_len(), 4);
    assert_eq!(output.state.min_queued_samples.load(Ordering::Relaxed), 8);
    assert_eq!(output.write(&[0.5; 8]), 8);
    render(&mut consumer, &mut samples, 2, &output.state);
    assert_eq!(consumer.occupied_len(), 8);
    assert_eq!(output.state.min_queued_samples.load(Ordering::Relaxed), 8);
    render(&mut consumer, &mut [0.0_f64; 8], 2, &output.state);
    assert_eq!(consumer.occupied_len(), 0);
    assert_eq!(output.state.min_queued_samples.load(Ordering::Relaxed), 8);
    render(&mut consumer, &mut samples, 2, &output.state);
    assert_eq!(output.state.min_queued_samples.load(Ordering::Relaxed), 0);
}

#[test]
fn queue_low_watermark_swap_starts_a_new_measurement_window() {
    let (mut output, mut consumer) = Output::fake(8_000, 2);
    assert_eq!(output.write(&[0.25; 8]), 8);
    let mut samples = [0.0_f64; 4];
    render(&mut consumer, &mut samples, 2, &output.state);
    render(&mut consumer, &mut samples, 2, &output.state);
    assert_eq!(
        output
            .state
            .min_queued_samples
            .swap(u64::MAX, Ordering::Relaxed),
        4
    );
    assert_eq!(
        output.state.min_queued_samples.load(Ordering::Relaxed),
        u64::MAX
    );
    assert_eq!(output.write(&[0.5; 12]), 12);
    render(&mut consumer, &mut samples, 2, &output.state);
    assert_eq!(output.state.min_queued_samples.load(Ordering::Relaxed), 12);
    render(&mut consumer, &mut samples, 2, &output.state);
    assert_eq!(
        output
            .state
            .min_queued_samples
            .swap(u64::MAX, Ordering::Relaxed),
        8
    );
}

#[test]
fn paused_callbacks_preserve_queue_low_watermark() {
    let (mut output, mut consumer) = Output::fake(8_000, 2);
    output.pause(true);
    for marker in [u64::MAX, 6] {
        output
            .state
            .min_queued_samples
            .store(marker, Ordering::Relaxed);
        for queued in [0, 2] {
            if queued > 0 {
                assert_eq!(output.write(&[0.25; 2]), 2);
            }
            let mut samples = [1.0_f64; 2];
            render(&mut consumer, &mut samples, 2, &output.state);
            assert_eq!(samples, [0.0; 2]);
            assert_eq!(consumer.occupied_len(), queued);
            assert_eq!(
                output.state.min_queued_samples.load(Ordering::Relaxed),
                marker
            );
            assert_eq!(output.consumed(), 0);
        }
        consumer.skip(2);
    }
}

#[test]
fn native_stereo_samples_survive_ring_wrap_and_variable_callback_sizes() {
    let (mut output, mut consumer) = Output::fake(8_000, 2);
    let samples: Vec<f64> = (0..32_000)
        .map(|index| (index % 2048) as f64 / 1024.0 - 1.0)
        .collect();
    let mut written = 0;
    let mut consumed = 0;
    for frames in [127, 1, 512, 73].into_iter().cycle() {
        written += output.write(&samples[written..]);
        let count = (frames * 2).min(written - consumed);
        let mut rendered = vec![0.0; count];
        render(&mut consumer, &mut rendered, 2, &output.state);
        assert_eq!(rendered, samples[consumed..consumed + count]);
        consumed += count;
        if consumed == samples.len() {
            break;
        }
    }
    assert_eq!(output.consumed(), samples.len() as u64);
    assert_eq!(output.state.underruns.load(Ordering::Relaxed), 0);
}
