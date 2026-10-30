use super::*;

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
