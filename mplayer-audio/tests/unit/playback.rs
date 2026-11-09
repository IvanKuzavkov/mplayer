use super::*;
use mplayer_core::TrackMetadata;
use std::sync::atomic::AtomicU64;

#[test]
#[ignore = "manual release-mode decode/convert/callback throughput measurement"]
fn playback_throughput() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tone24.flac");
    for rate in [44_100, 48_000] {
        let started = Instant::now();
        let mut frames = 0;
        for _ in 0..1000 {
            let mut decoder = Decoder::open(&path).unwrap();
            let mut converter = Converter::new(44_100, rate, 2, 2).unwrap();
            let (mut output, mut consumer) = Output::fake(rate, 2);
            output.pause(false);
            let mut decoded = Vec::new();
            let mut converted = Vec::new();
            loop {
                let more = decoder.next(&mut decoded).unwrap();
                for chunk in decoded.chunks(2048).chain((!more).then_some(&[][..])) {
                    converter.convert(chunk, !more, &mut converted).unwrap();
                    assert_eq!(output.write(&converted), converted.len());
                    let mut rendered = vec![0.0_f32; converted.len()];
                    output::test_consume(&mut consumer, &mut rendered, 2, &output.state);
                    frames += rendered.len() / 2;
                    std::hint::black_box(rendered);
                }
                if !more {
                    break;
                }
            }
        }
        let seconds = started.elapsed().as_secs_f64();
        eprintln!(
            "{rate} Hz: {}x real time ({seconds:.6}s for 100s stereo PCM)",
            100.0 / seconds
        );
        assert_eq!(frames, rate as usize * 100);
    }
}

static NEXT_FILE: AtomicU64 = AtomicU64::new(0);

#[tokio::test]
#[ignore = "manual muted hardware playback; set MPLAYER_DAC_BENCH_FILE and optionally MPLAYER_DAC_BENCH_DEVICE"]
async fn dac_playback_diagnostics() {
    let Some(path) = std::env::var_os("MPLAYER_DAC_BENCH_FILE") else {
        eprintln!("Set MPLAYER_DAC_BENCH_FILE to run muted hardware playback");
        return;
    };
    let seconds: u64 = std::env::var("MPLAYER_DAC_BENCH_SECONDS")
        .map_or(15, |value| value.parse().expect("invalid duration"));
    let (sender, _) = watch::channel(PlaybackSnapshot::default());
    let (device_sender, mut device_updates) = watch::channel(Vec::new());
    let discovery = tokio::spawn(crate::discovery::run(
        device_sender,
        output::devices,
        Duration::from_secs(3),
    ));
    tokio::time::timeout(Duration::from_secs(10), device_updates.changed())
        .await
        .expect("No output devices discovered within 10 seconds")
        .unwrap();
    let devices = device_updates.borrow_and_update().clone();
    let selected = std::env::var("MPLAYER_DAC_BENCH_DEVICE").ok().map(|name| {
        let matching: Vec<_> = devices
            .iter()
            .filter(|device| device.id.contains(&name))
            .collect();
        assert_eq!(matching.len(), 1, "Choose one device from {devices:?}");
        matching[0].id.clone()
    });
    let mut worker = Worker::with_devices(
        sender,
        Arc::new(|| {}),
        Arc::new(AtomicBool::new(false)),
        devices,
    );
    worker.device_updates = device_updates;
    worker.selected_device = selected;
    worker.snapshot.volume = 0.0;
    let track = Track {
        id: 1,
        path: path.into(),
        metadata: TrackMetadata::default(),
        missing: false,
        segment: None,
    };
    worker
        .load(Prepared::open(track).unwrap(), None, 0, false)
        .unwrap();
    let output = worker.output.as_ref().unwrap();
    let state = output.state.clone();
    eprintln!(
        "DAC_TEST device={} rate={} channels={} volume=0",
        output.device, output.rate, output.channels
    );
    let started = Instant::now();
    let mut max_gap = 0;
    let mut last_report = Instant::now();
    while started.elapsed() < Duration::from_secs(seconds) {
        tokio::time::sleep(Duration::from_millis(5)).await;
        worker.tick().unwrap();
        worker.refresh_devices();
        max_gap = max_gap.max(state.max_callback_gap_ns.load(Ordering::Relaxed));
        if last_report.elapsed() >= Duration::from_secs(1) {
            eprintln!(
                "DAC_PROGRESS wall_s={:.3} position_ms={} queued_ms={} callbacks={} buffer_underruns={} device_underruns={} max_gap_ms={:.3} state={:?}",
                started.elapsed().as_secs_f64(),
                worker.position(),
                worker.queued_ms(),
                state.callbacks.load(Ordering::Relaxed),
                state.underruns.load(Ordering::Relaxed),
                state.device_underruns.load(Ordering::Relaxed),
                max_gap as f64 / 1_000_000.0,
                worker.snapshot.state
            );
            last_report = Instant::now();
        }
        if worker.last_publish.elapsed() >= Duration::from_millis(100) {
            worker.publish();
        }
    }
    discovery.abort();
    let _ = discovery.await;
    assert!(
        state.callbacks.load(Ordering::Relaxed) > 0,
        "No device callbacks"
    );
    assert!(
        !state.failed.load(Ordering::Relaxed),
        "Device stream failed"
    );
    eprintln!(
        "DAC_RESULT seconds={seconds} position_ms={} buffer_underruns={} device_underruns={} max_gap_ms={:.3}",
        worker.position(),
        state.underruns.load(Ordering::Relaxed),
        state.device_underruns.load(Ordering::Relaxed),
        max_gap as f64 / 1_000_000.0
    );
}

struct Fixture(std::path::PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn fixture(id: i64, sample: i16) -> (Fixture, Track) {
    fixture_length(id, sample, 4000)
}
fn fixture_length(id: i64, sample: i16, frames: u32) -> (Fixture, Track) {
    fixture_pcm(id, i32::from(sample), 16, frames)
}

fn fixture_pcm(id: i64, sample: i32, bits: u16, frames: u32) -> (Fixture, Track) {
    let path = std::env::temp_dir().join(format!(
        "mplayer-worker-{}-{}.wav",
        std::process::id(),
        NEXT_FILE.fetch_add(1, Ordering::Relaxed)
    ));
    let mut bytes = Vec::new();
    let sample_bytes = bits / 8;
    let data_size = frames * u32::from(sample_bytes);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(data_size + 36).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16_u32.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&8000_u32.to_le_bytes());
    bytes.extend_from_slice(&(8000 * u32::from(sample_bytes)).to_le_bytes());
    bytes.extend_from_slice(&sample_bytes.to_le_bytes());
    bytes.extend_from_slice(&bits.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_size.to_le_bytes());
    for _ in 0..frames {
        bytes.extend_from_slice(&sample.to_le_bytes()[..usize::from(sample_bytes)]);
    }
    std::fs::write(&path, bytes).unwrap();
    let track = Track {
        id,
        path: path.clone(),
        metadata: TrackMetadata::default(),
        missing: false,
        segment: None,
    };
    (Fixture(path), track)
}

fn worker(
    track: Track,
    next: Option<Track>,
) -> (Worker, ringbuf::HeapCons<f64>, Arc<output::CallbackState>) {
    worker_at_rate(track, next, 8000, 1)
}

fn worker_at_rate(
    track: Track,
    next: Option<Track>,
    rate: u32,
    channels: usize,
) -> (Worker, ringbuf::HeapCons<f64>, Arc<output::CallbackState>) {
    let (sender, _) = watch::channel(PlaybackSnapshot::default());
    let mut worker = Worker::with_devices(
        sender,
        Arc::new(|| {}),
        Arc::new(AtomicBool::new(false)),
        Vec::new(),
    );
    let (output, consumer) = Output::fake(rate, channels);
    let state = output.state.clone();
    let session = Session::new(Prepared::open(track).unwrap(), rate, channels).unwrap();
    worker.snapshot.state = PlaybackState::Playing;
    worker.snapshot.track = Some(session.prepared.track.clone());
    worker.snapshot.duration_ms = session.prepared.track.metadata.duration_ms;
    worker.segments.push_back(Segment {
        track: session.prepared.track.clone(),
        start_sample: 0,
        end_sample: None,
        base_ms: 0,
        pcm_rate: session.prepared.decoder.pcm_spec().rate,
        rate_selection: session.prepared.rate_selection(),
    });
    worker.session = Some(session);
    worker.next = next;
    worker.output = Some(output);
    worker.produce().unwrap();
    (worker, consumer, state)
}

#[test]
#[ignore = "manual offline DSD callback correctness; set MPLAYER_DSD_CHECK_FILE"]
fn dsd_file_callback_correctness() {
    use cpal::Sample;

    let Some(path) = std::env::var_os("MPLAYER_DSD_CHECK_FILE") else {
        eprintln!("Set MPLAYER_DSD_CHECK_FILE for offline playback-queue verification");
        return;
    };
    let starts: Vec<u64> = std::env::var("MPLAYER_DSD_CHECK_STARTS")
        .unwrap_or_else(|_| "105,116".into())
        .split(',')
        .map(|value| value.parse().expect("invalid window start"))
        .collect();
    let seconds: u64 = std::env::var("MPLAYER_DSD_CHECK_SECONDS")
        .map_or(10, |value| value.parse().expect("invalid window length"));
    let end_seconds = starts.iter().max().unwrap() + seconds;
    assert!(end_seconds > 0 && end_seconds <= 3600);
    for period in [883, 3528] {
        let track = Track {
            id: 1,
            path: path.clone().into(),
            metadata: TrackMetadata::default(),
            missing: false,
            segment: None,
        };
        let mut reference = Decoder::open(&track.path).unwrap();
        let spec = reference.pcm_spec();
        assert_eq!(spec.rate, crate::dsd::PCM_RATE);
        let channels = usize::from(spec.channels);
        let end = end_seconds as usize * spec.rate as usize * channels;
        let (mut worker, mut consumer, callback) = worker_at_rate(track, None, spec.rate, channels);
        worker.output.as_mut().unwrap().sample_format = cpal::SampleFormat::I32;
        let mut decoded = Vec::new();
        let mut decoded_offset = 0;
        let mut rendered = vec![0_i32; period * channels];
        let mut checked = 0;
        while checked < end {
            let count = rendered.len().min(end - checked);
            output::test_consume(&mut consumer, &mut rendered[..count], channels, &callback);
            assert_eq!(callback.underruns.load(Ordering::Relaxed), 0);
            for &actual in &rendered[..count] {
                if decoded_offset == decoded.len() {
                    assert!(reference.next(&mut decoded).unwrap());
                    decoded_offset = 0;
                }
                let sample = decoded[decoded_offset];
                assert!(sample.is_finite());
                let expected = i32::from_sample(sample.clamp(-1.0, 1.0 - 2.0_f64.powi(-31)));
                assert_eq!(
                    actual,
                    expected,
                    "period={period} frame={} channel={}",
                    checked / channels,
                    checked % channels
                );
                checked += 1;
                decoded_offset += 1;
            }
            worker.tick().unwrap();
        }
        assert_eq!(callback.consumed.load(Ordering::Relaxed), end as u64);
        eprintln!(
            "DSD_CALLBACK_CORRECTNESS period={period} rate={} channels={channels} seconds={end_seconds} matching_i32_samples={checked}",
            spec.rate
        );
    }
}

#[test]
fn periodic_diagnostics_reset_measurements_without_changing_playback() {
    let (_fixture, track) = fixture(1, 8192);
    let (mut worker, mut consumer, callback) = worker(track, None);
    let mut samples = [0.0_f64; 800];
    output::test_consume(&mut consumer, &mut samples, 1, &callback);
    callback
        .max_callback_interval_ns
        .store(7_000_000, Ordering::Relaxed);
    assert!(callback.min_queued_samples.load(Ordering::Relaxed) < u64::MAX);
    assert_eq!(
        callback.max_callback_interval_ns.load(Ordering::Relaxed),
        7_000_000
    );
    worker.last_diagnostics = Instant::now() - Duration::from_secs(5);
    let queued_ms = worker.queued_ms();
    let written = worker.written;

    worker.publish();

    assert_eq!(
        callback.min_queued_samples.load(Ordering::Relaxed),
        u64::MAX
    );
    assert_eq!(callback.max_callback_interval_ns.load(Ordering::Relaxed), 0);
    assert!(worker.last_diagnostics.elapsed() < Duration::from_secs(5));
    assert_eq!(worker.snapshot.state, PlaybackState::Playing);
    assert_eq!(worker.snapshot.position_ms, 100);
    assert_eq!(worker.queued_ms(), queued_ms);
    assert_eq!(worker.written, written);
    assert!(Arc::ptr_eq(
        &worker.output.as_ref().unwrap().state,
        &callback
    ));
}

#[test]
fn device_updates_publish_only_the_latest_list_and_wake() {
    let (sender, mut snapshots) = watch::channel(PlaybackSnapshot::default());
    let wakes = Arc::new(AtomicU64::new(0));
    let wake_count = wakes.clone();
    let mut worker = Worker::with_devices(
        sender,
        Arc::new(move || {
            wake_count.fetch_add(1, Ordering::Relaxed);
        }),
        Arc::new(AtomicBool::new(false)),
        Vec::new(),
    );
    let (devices, updates) = watch::channel(Vec::new());
    worker.device_updates = updates;
    devices.send(vec!["Old output".into()]).unwrap();
    let latest = vec!["Headphones".into(), "Replacement output".into()];
    devices.send(latest.clone()).unwrap();

    worker.refresh_devices();

    assert_eq!(worker.snapshot.devices, latest);
    assert!(snapshots.has_changed().unwrap());
    assert_eq!(snapshots.borrow_and_update().devices, latest);
    assert!(!worker.device_updates.has_changed().unwrap());
    assert_eq!(wakes.load(Ordering::Relaxed), 1);
}

#[test]
fn unchanged_device_updates_do_not_publish_or_wake() {
    let initial = vec!["Headphones".into()];
    let (sender, mut snapshots) = watch::channel(PlaybackSnapshot::default());
    let wakes = Arc::new(AtomicU64::new(0));
    let wake_count = wakes.clone();
    let mut worker = Worker::with_devices(
        sender,
        Arc::new(move || {
            wake_count.fetch_add(1, Ordering::Relaxed);
        }),
        Arc::new(AtomicBool::new(false)),
        initial.clone(),
    );
    let (devices, updates) = watch::channel(initial.clone());
    worker.device_updates = updates;
    worker.publish();
    snapshots.borrow_and_update();
    let last_publish = worker.last_publish;
    let wake_count = wakes.load(Ordering::Relaxed);

    worker.refresh_devices();
    for _ in 0..2 {
        devices.send(initial.clone()).unwrap();
        worker.refresh_devices();
        assert!(!worker.device_updates.has_changed().unwrap());
    }
    worker.refresh_devices();

    assert_eq!(worker.snapshot.devices, initial);
    assert!(!snapshots.has_changed().unwrap());
    assert_eq!(worker.last_publish, last_publish);
    assert_eq!(wakes.load(Ordering::Relaxed), wake_count);
}

#[test]
fn closed_device_updates_publish_the_latest_list() {
    let (sender, mut snapshots) = watch::channel(PlaybackSnapshot::default());
    let mut worker = Worker::with_devices(
        sender,
        Arc::new(|| {}),
        Arc::new(AtomicBool::new(false)),
        Vec::new(),
    );
    let latest = vec!["Replacement output".into()];

    update_devices(&mut worker, latest.clone());

    assert_eq!(worker.snapshot.devices, latest);
    assert!(snapshots.has_changed().unwrap());
    assert_eq!(snapshots.borrow_and_update().devices, latest);
    worker.refresh_devices();
    assert!(!snapshots.has_changed().unwrap());
}

#[test]
fn device_discovery_preserves_selection_stream_and_position_while_playing() {
    let (_first, first) = fixture(1, 8192);
    let (_second, second) = fixture(2, -16384);
    let (mut worker, mut consumer, callback) = worker(first, Some(second));
    let selected = Some("Headphones".to_string());
    worker.selected_device = selected.clone();
    worker.snapshot.selected_device = selected.clone();
    worker.output.as_mut().unwrap().device = "Headphones".into();
    worker.snapshot.device = "Headphones".into();
    let (devices, updates) = watch::channel(vec!["Headphones".into()]);
    worker.device_updates = updates;
    let mut rendered = [0.0_f64; 800];
    output::test_consume(&mut consumer, &mut rendered, 1, &callback);
    worker.publish();
    let written = worker.written;
    let queued_ms = worker.queued_ms();

    for latest in [vec!["Replacement output".into()], Vec::new()] {
        devices.send(latest.clone()).unwrap();
        if latest.is_empty() {
            worker
                .command(AudioCommand::SetDevice(selected.clone()))
                .unwrap();
        } else {
            worker.refresh_devices();
        }

        assert_eq!(worker.snapshot.devices, latest);
        assert_eq!(worker.selected_device, selected);
        assert_eq!(worker.snapshot.selected_device, selected);
        assert_eq!(worker.snapshot.device, "Headphones");
        assert_eq!(worker.snapshot.state, PlaybackState::Playing);
        assert_eq!(worker.position(), 100);
        assert_eq!(worker.snapshot.position_ms, 100);
        assert!(Arc::ptr_eq(
            &worker.output.as_ref().unwrap().state,
            &callback
        ));
        assert!(!callback.paused.load(Ordering::Relaxed));
        assert_eq!(worker.written, written);
        assert_eq!(worker.queued_ms(), queued_ms);
        assert_eq!(worker.snapshot.track.as_ref().unwrap().id, 1);
        assert_eq!(worker.future_track().unwrap().id, 2);
        assert!(worker.snapshot.error.is_none());
        assert!(worker.snapshot.notification.is_none());
    }
    assert_eq!(rendered, [0.25; 800]);
}

#[test]
fn eof_is_announced_after_consumption_and_compatible_tracks_are_contiguous() {
    let (_first, first) = fixture(1, 8192);
    let (_second, second) = fixture(2, -16384);
    let (mut worker, mut consumer, callback) = worker(first, Some(second));
    assert_eq!(worker.snapshot.completed, 0);
    let mut samples = Vec::new();
    let mut transitioned = false;
    for _ in 0..32 {
        let mut buffer = [0.0; 250];
        output::test_consume(&mut consumer, &mut buffer, 1, &callback);
        samples.extend(buffer);
        worker.tick().unwrap();
        if worker.snapshot.completed == 1 {
            transitioned = true;
            assert_eq!(worker.snapshot.track.as_ref().unwrap().id, 2);
        }
    }
    worker.tick().unwrap();
    assert!(transitioned);
    assert_eq!(worker.snapshot.completed, 2);
    assert_eq!(worker.snapshot.state, PlaybackState::Stopped);
    assert!(samples[..4000].iter().all(|sample| *sample == 0.25));
    assert!(samples[4000..].iter().all(|sample| *sample == -0.5));
    assert_eq!(callback.underruns.load(Ordering::Relaxed), 0);
}

#[test]
fn pause_preserves_pcm_and_stop_invalidates_completion_generation() {
    let (_fixture, track) = fixture(1, 8192);
    let (mut worker, mut consumer, callback) = worker(track, None);
    worker.command(AudioCommand::Pause).unwrap();
    let mut buffer = [1.0; 250];
    output::test_consume(&mut consumer, &mut buffer, 1, &callback);
    worker.tick().unwrap();
    assert_eq!(buffer, [0.0; 250]);
    assert_eq!(worker.position(), 0);
    worker.command(AudioCommand::Resume).unwrap();
    output::test_consume(&mut consumer, &mut buffer, 1, &callback);
    assert_eq!(buffer, [0.25; 250]);
    worker
        .command(AudioCommand::Stop { generation: 17 })
        .unwrap();
    assert_eq!(worker.snapshot.completed, 0);
    assert_eq!(worker.snapshot.generation, 17);
    assert_eq!(worker.snapshot.state, PlaybackState::Stopped);
    assert!(worker.snapshot.track.is_none());
    assert_eq!(worker.snapshot.position_ms, 0);
    assert_eq!(worker.snapshot.duration_ms, 0);
    assert_eq!(worker.snapshot.source_rate, 0);
    assert_eq!(worker.snapshot.output_rate, 0);
    assert!(worker.snapshot.device.is_empty());
    assert!(worker.segments.is_empty());
    assert!(worker.output.is_none());
    worker
        .command(AudioCommand::Seek {
            position_ms: 100,
            request_id: 8,
        })
        .unwrap();
    assert!(worker.session.is_none());
    assert!(worker.output.is_none());
}

#[test]
fn hardware_underruns_are_counted_without_pausing_or_losing_track_position() {
    let (_fixture, track) = fixture(1, 8192);
    let (mut worker, mut consumer, state) = worker(track, None);
    let mut rendered = [0.0_f64; 250];
    output::test_consume(&mut consumer, &mut rendered, 1, &state);
    state.device_underruns.fetch_add(1, Ordering::Relaxed);
    worker.tick().unwrap();
    worker.publish();
    assert_eq!(worker.snapshot.state, PlaybackState::Playing);
    assert_eq!(worker.snapshot.underruns, 1);
    assert_eq!(worker.position(), 31);
    assert!(worker.snapshot.notification.is_none());
    assert!(worker.snapshot.error.is_none());
    assert_eq!(rendered, [0.25; 250]);
    worker
        .command(AudioCommand::Stop { generation: 2 })
        .unwrap();
    worker.publish();
    assert_eq!(worker.snapshot.underruns, 1);
}

#[test]
fn dsd_queue_uses_pcm_rates_and_preserves_source_metadata_and_controls() {
    let mut fixtures = Vec::new();
    let mut tracks = Vec::new();
    let mut expected = Vec::new();
    for (id, rate) in [(1, 2_822_400_u32), (2, 11_289_600)] {
        let samples = u64::from(rate / 75);
        let raw = [0xdf, 0x05].repeat(samples as usize / 8);
        let file =
            dsd::tests::Fixture::new(&dsd::tests::dsf(rate, 2, samples, true, &raw, &[]), "dsf");
        let mut decoder = Decoder::open(&file.0).unwrap();
        let mut buffer = Vec::new();
        while decoder.next(&mut buffer).unwrap() {
            expected.extend_from_slice(&buffer);
        }
        tracks.push(Track {
            id,
            path: file.0.clone(),
            metadata: TrackMetadata::default(),
            missing: false,
            segment: None,
        });
        fixtures.push(file);
    }
    let second = tracks.pop().unwrap();
    let first = tracks.pop().unwrap();
    let (mut worker, mut consumer, callback) =
        worker_at_rate(first, Some(second), dsd::PCM_RATE, 2);
    assert_eq!(
        worker.segments.len(),
        2,
        "Different DSD rates share the same decoded PCM rate"
    );
    assert_eq!(
        worker.snapshot.track.as_ref().unwrap().metadata.sample_rate,
        2_822_400
    );
    worker.command(AudioCommand::Pause).unwrap();
    let mut paused = [1.0_f64; 294];
    output::test_consume(&mut consumer, &mut paused, 2, &callback);
    assert!(paused.iter().all(|&v| v == 0.0));
    assert_eq!(callback.consumed.load(Ordering::Relaxed), 0);
    worker.command(AudioCommand::SetVolume(0.5)).unwrap();
    worker.command(AudioCommand::Resume).unwrap();
    let mut actual = Vec::new();
    for _ in 0..32 {
        let mut buffer = [0.0_f64; 294];
        output::test_consume(&mut consumer, &mut buffer, 2, &callback);
        actual.extend_from_slice(&buffer);
        worker.tick().unwrap();
        if worker.snapshot.completed == 1 {
            assert_eq!(worker.snapshot.source_rate, 11_289_600);
            assert_eq!(
                worker.snapshot.track.as_ref().unwrap().metadata.bit_depth,
                Some(1)
            );
        }
    }
    assert_eq!(
        actual,
        expected.iter().map(|value| value * 0.5).collect::<Vec<_>>()
    );
    assert_eq!(worker.snapshot.completed, 2);
    assert_eq!(worker.snapshot.state, PlaybackState::Stopped);
    assert_eq!(callback.underruns.load(Ordering::Relaxed), 0);
}

#[test]
fn dsd_load_and_seek_negotiate_pcm_and_resample_for_device_fallback() {
    let samples = 2_822_400_u64 / 10;
    let raw = [0xdf, 0x05].repeat(samples as usize / 8);
    let file = dsd::tests::Fixture::new(
        &dsd::tests::dsf(2_822_400, 2, samples, true, &raw, &[]),
        "dsf",
    );
    let track = Track {
        id: 1,
        path: file.0.clone(),
        metadata: TrackMetadata::default(),
        missing: false,
        segment: None,
    };
    let (sender, _) = watch::channel(PlaybackSnapshot::default());
    let mut worker = Worker::with_devices(
        sender,
        Arc::new(|| {}),
        Arc::new(AtomicBool::new(false)),
        Vec::new(),
    );
    worker.output_factory = Some(|rate, channels, format, rate_selection, selected, volume| {
        assert_eq!(rate, dsd::PCM_RATE);
        assert_eq!(channels, 2);
        assert_eq!(format, PcmFormat::Float(64));
        assert_eq!(rate_selection, RateSelection::Highest);
        let mut output =
            recovery_output(48_000, channels, format, rate_selection, selected, volume)?;
        output.sample_format = cpal::SampleFormat::F32;
        Ok(output)
    });
    worker
        .load(Prepared::open(track).unwrap(), None, 0, false)
        .unwrap();
    assert_eq!(worker.snapshot.source_rate, 2_822_400);
    assert_eq!(worker.snapshot.output_rate, 48_000);
    assert_eq!(worker.written, 4800 * 2);
    worker.command(AudioCommand::Pause).unwrap();
    worker
        .command(AudioCommand::Seek {
            position_ms: 50,
            request_id: 19,
        })
        .unwrap();
    assert_eq!(worker.snapshot.state, PlaybackState::Paused);
    assert_eq!(worker.snapshot.position_ms, 50);
    assert_eq!(worker.snapshot.seek_request_id, 19);
    assert_eq!(worker.written, 2400 * 2);
    assert!(worker.snapshot.error.is_none());
    worker.command(AudioCommand::Resume).unwrap();
    assert_eq!(worker.snapshot.state, PlaybackState::Playing);
}

#[test]
fn dsd_load_and_paused_seek_resample_to_384000_with_i32_output() {
    let samples = 2_822_400_u64 / 10;
    let raw = [0xdf, 0x05].repeat(samples as usize / 8);
    let file = dsd::tests::Fixture::new(
        &dsd::tests::dsf(2_822_400, 2, samples, true, &raw, &[]),
        "dsf",
    );
    let track = Track {
        id: 1,
        path: file.0.clone(),
        metadata: TrackMetadata::default(),
        missing: false,
        segment: None,
    };
    let (sender, _) = watch::channel(PlaybackSnapshot::default());
    let mut worker = Worker::with_devices(
        sender,
        Arc::new(|| {}),
        Arc::new(AtomicBool::new(false)),
        Vec::new(),
    );
    worker.output_factory = Some(|rate, channels, format, rate_selection, selected, volume| {
        assert_eq!(rate, dsd::PCM_RATE);
        assert_eq!(channels, 2);
        assert_eq!(format, PcmFormat::Float(64));
        assert_eq!(rate_selection, RateSelection::Highest);
        precision_output(384_000, channels, format, rate_selection, selected, volume)
    });
    worker
        .load(Prepared::open(track).unwrap(), None, 0, false)
        .unwrap();
    assert_eq!(worker.snapshot.source_rate, 2_822_400);
    assert_eq!(
        worker.snapshot.track.as_ref().unwrap().metadata.sample_rate,
        2_822_400
    );
    assert_eq!(worker.snapshot.output_rate, 384_000);
    assert_eq!(
        worker.output.as_ref().unwrap().sample_format,
        cpal::SampleFormat::I32
    );
    assert!(worker.session.is_none(), "Resampler must be fully flushed");
    assert_eq!(worker.written, 38_400 * 2);
    assert_eq!(worker.segments.back().unwrap().end_sample, Some(38_400 * 2));
    assert!(worker.snapshot.error.is_none());

    worker.command(AudioCommand::Pause).unwrap();
    worker
        .command(AudioCommand::Seek {
            position_ms: 50,
            request_id: 19,
        })
        .unwrap();
    assert_eq!(worker.snapshot.state, PlaybackState::Paused);
    assert_eq!(worker.snapshot.position_ms, 50);
    assert_eq!(worker.snapshot.seek_request_id, 19);
    assert_eq!(worker.snapshot.source_rate, 2_822_400);
    assert_eq!(worker.snapshot.output_rate, 384_000);
    assert_eq!(
        worker.output.as_ref().unwrap().sample_format,
        cpal::SampleFormat::I32
    );
    assert!(worker.session.is_none(), "Seek tail must be fully flushed");
    assert_eq!(worker.written, 19_200 * 2);
    assert_eq!(worker.segments.back().unwrap().end_sample, Some(19_200 * 2));
    assert!(worker.snapshot.error.is_none());
    worker.command(AudioCommand::Resume).unwrap();
    assert_eq!(worker.snapshot.state, PlaybackState::Playing);
}

#[test]
fn mixed_dsd_pcm_policies_drain_and_reopen_even_with_identical_decoded_pcm() {
    let samples = 2_822_400_u64 / 75;
    let raw = [0xdf, 0x05].repeat(samples as usize / 8);
    let dsd_file = dsd::tests::Fixture::new(
        &dsd::tests::dsf(2_822_400, 2, samples, true, &raw, &[]),
        "dsf",
    );
    let frames = dsd::PCM_RATE / 75;
    let data_size = frames * 2 * 8;
    let mut wav = Vec::new();
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_size).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16_u32.to_le_bytes());
    wav.extend_from_slice(&3_u16.to_le_bytes());
    wav.extend_from_slice(&2_u16.to_le_bytes());
    wav.extend_from_slice(&dsd::PCM_RATE.to_le_bytes());
    wav.extend_from_slice(&(dsd::PCM_RATE * 16).to_le_bytes());
    wav.extend_from_slice(&16_u16.to_le_bytes());
    wav.extend_from_slice(&64_u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_size.to_le_bytes());
    for _ in 0..frames * 2 {
        wav.extend_from_slice(&0.25_f64.to_le_bytes());
    }
    let pcm_file = dsd::tests::Fixture::new(&wav, "wav");

    for dsd_first in [true, false] {
        let mut prepared = Vec::new();
        for (id, file, is_dsd) in [(1, &dsd_file, true), (2, &pcm_file, false)] {
            let mut item = Prepared::open(Track {
                id,
                path: file.0.clone(),
                metadata: TrackMetadata::default(),
                missing: false,
                segment: None,
            })
            .unwrap();
            // Deliberately misleading labels must not change the backend's policy.
            let codec = if is_dsd { "pcm" } else { "dsd" };
            item.track.metadata.codec = codec.into();
            item.decoder.metadata.codec = codec.into();
            assert_eq!(item.decoder.is_dsd(), is_dsd);
            assert_eq!(
                item.rate_selection(),
                if is_dsd {
                    RateSelection::Highest
                } else {
                    RateSelection::Native
                }
            );
            assert_eq!(item.decoder.pcm_spec().rate, 176_400);
            assert_eq!(item.decoder.pcm_spec().channels, 2);
            assert_eq!(item.decoder.pcm_format(), PcmFormat::Float(64));
            prepared.push(item);
        }
        if !dsd_first {
            prepared.reverse();
        }
        let second = prepared.pop().unwrap();
        let first = prepared.pop().unwrap();
        let first_policy = first.rate_selection();
        let second_policy = second.rate_selection();
        let second_id = second.track.id;
        let (sender, _) = watch::channel(PlaybackSnapshot::default());
        let mut worker = Worker::with_devices(
            sender,
            Arc::new(|| {}),
            Arc::new(AtomicBool::new(false)),
            Vec::new(),
        );
        worker.output_factory = Some(|rate, channels, format, policy, selected, volume| {
            assert_eq!(rate, 176_400);
            assert_eq!(format, PcmFormat::Float(64));
            let target = match policy {
                RateSelection::Native => rate,
                RateSelection::Highest => 384_000,
            };
            recovery_output(target, channels, format, policy, selected, volume)
        });
        let next = second.track.clone();
        worker.prepared = Some(second);
        worker.load(first, Some(next), 0, false).unwrap();
        let initial_rate = if dsd_first { 384_000 } else { 176_400 };
        assert_eq!(worker.snapshot.output_rate, initial_rate);
        assert_eq!(worker.segments.len(), 1);
        assert_eq!(worker.segments[0].rate_selection, first_policy);
        assert!(!worker.compatible(worker.prepared.as_ref().unwrap()));
        assert!(
            worker.session.is_none(),
            "First track must be fully flushed"
        );
        assert_eq!(worker.snapshot.completed, 0);
        let state = worker.output.as_ref().unwrap().state.clone();
        state.consumed.store(worker.written, Ordering::Relaxed);
        worker.tick().unwrap();
        assert_eq!(worker.snapshot.completed, 1);
        assert_eq!(worker.snapshot.track.as_ref().unwrap().id, second_id);
        assert_eq!(
            worker.snapshot.output_rate,
            if dsd_first { 176_400 } else { 384_000 }
        );
        assert_eq!(worker.segments[0].rate_selection, second_policy);
        assert!(!Arc::ptr_eq(&state, &worker.output.as_ref().unwrap().state));
        assert!(worker.snapshot.error.is_none());
    }
}

#[test]
fn three_short_tracks_remain_contiguous_when_next_arrives_after_decoding_eof() {
    let (_a, a) = fixture_length(1, 8192, 1000);
    let (_b, b) = fixture_length(2, -16384, 1000);
    let (_c, c) = fixture_length(3, 4096, 1000);
    let (mut worker, mut consumer, callback) = worker(a, Some(b));
    assert!(
        worker.session.is_none(),
        "Both short tracks fit in the output ring"
    );
    let mut remaining_next = Some(c);
    let mut samples = Vec::new();
    for _ in 0..12 {
        let mut buffer = [0.0; 250];
        output::test_consume(&mut consumer, &mut buffer, 1, &callback);
        samples.extend(buffer);
        worker.tick().unwrap();
        if worker.snapshot.completed == 1
            && let Some(next) = remaining_next.take()
        {
            worker.command(AudioCommand::SetNext(Some(next))).unwrap();
        }
    }
    assert_eq!(worker.snapshot.completed, 3);
    assert_eq!(worker.snapshot.state, PlaybackState::Stopped);
    assert!(samples[..1000].iter().all(|sample| *sample == 0.25));
    assert!(samples[1000..2000].iter().all(|sample| *sample == -0.5));
    assert!(samples[2000..].iter().all(|sample| *sample == 0.125));
    assert_eq!(callback.underruns.load(Ordering::Relaxed), 0);
}

#[test]
fn enqueue_after_decoding_eof_appends_to_existing_stream() {
    let (_a, a) = fixture_length(1, 8192, 1000);
    let (_b, b) = fixture_length(2, -16384, 1000);
    let (mut worker, mut consumer, callback) = worker(a, None);
    assert!(worker.session.is_none());
    worker.command(AudioCommand::SetNext(Some(b))).unwrap();
    worker.tick().unwrap();
    let mut samples = Vec::new();
    for _ in 0..8 {
        let mut buffer = [0.0; 250];
        output::test_consume(&mut consumer, &mut buffer, 1, &callback);
        samples.extend(buffer);
        worker.tick().unwrap();
    }
    assert_eq!(worker.snapshot.completed, 2);
    assert!(samples[..1000].iter().all(|sample| *sample == 0.25));
    assert!(samples[1000..].iter().all(|sample| *sample == -0.5));
    assert_eq!(callback.underruns.load(Ordering::Relaxed), 0);
}

#[test]
fn resampled_gapless_tracks_equal_one_continuous_conversion() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tone24.flac");
    let track = Track {
        id: 1,
        path: path.clone(),
        metadata: TrackMetadata::default(),
        missing: false,
        segment: None,
    };
    let next = Track {
        id: 2,
        ..track.clone()
    };
    let (mut worker, mut consumer, callback) = worker_at_rate(track, Some(next), 48_000, 2);
    let mut source = Vec::new();
    let mut buffer = Vec::new();
    for _ in 0..2 {
        let mut decoder = Decoder::open(&path).unwrap();
        while decoder.next(&mut buffer).unwrap() {
            source.extend_from_slice(&buffer);
        }
    }
    let mut converter = Converter::new(44_100, 48_000, 2, 2).unwrap();
    let mut expected = Vec::new();
    for chunk in source.chunks(2048) {
        converter.convert(chunk, false, &mut buffer).unwrap();
        expected.extend_from_slice(&buffer);
    }
    converter.convert(&[], true, &mut buffer).unwrap();
    expected.extend_from_slice(&buffer);
    assert_eq!(expected.len(), 19_200);
    assert_eq!(worker.segments.front().unwrap().end_sample, Some(9_600));
    let mut actual = Vec::new();
    for index in 0..96 {
        let mut output = [0.0; 200];
        output::test_consume(&mut consumer, &mut output, 2, &callback);
        actual.extend(output);
        worker.tick().unwrap();
        if index < 47 {
            assert_eq!(worker.snapshot.completed, 0);
        }
        if index == 47 {
            assert_eq!(worker.snapshot.completed, 1);
            assert_eq!(worker.snapshot.track.as_ref().unwrap().id, 2);
        }
    }
    assert_eq!(
        actual, expected,
        "Track boundary must preserve FFT history and tail samples"
    );
    assert_eq!(worker.snapshot.completed, 2);
    assert_eq!(worker.snapshot.state, PlaybackState::Stopped);
    assert_eq!(callback.underruns.load(Ordering::Relaxed), 0);
}

fn recovery_output(
    rate: u32,
    channels: usize,
    _format: PcmFormat,
    _rate_selection: RateSelection,
    selected: Option<&str>,
    volume: f32,
) -> Result<Output> {
    let (mut output, _) = Output::fake(rate, channels);
    output.device = selected.unwrap_or("System default").to_string();
    output.pause(true);
    output.set_volume(volume);
    Ok(output)
}

fn precision_output(
    rate: u32,
    channels: usize,
    format: PcmFormat,
    rate_selection: RateSelection,
    selected: Option<&str>,
    volume: f32,
) -> Result<Output> {
    let mut output = recovery_output(rate, channels, format, rate_selection, selected, volume)?;
    output.sample_format = cpal::SampleFormat::I32;
    Ok(output)
}

#[test]
fn mixed_precision_tracks_reuse_or_drain_and_reopen_the_stream() {
    use cpal::SampleFormat;
    for initial_format in [SampleFormat::F32, SampleFormat::I32, SampleFormat::F64] {
        let (_a, a) = fixture_pcm(1, 8192, 16, 1000);
        let (_b, b) = fixture_pcm(2, 0x4000_0001, 32, 1000);
        let (mut worker, mut consumer, callback) = worker(a, None);
        worker.output.as_mut().unwrap().sample_format = initial_format;
        worker.output_factory = Some(precision_output);
        worker.command(AudioCommand::SetNext(Some(b))).unwrap();
        worker.tick().unwrap();
        let needs_reopen = initial_format == SampleFormat::F32;
        assert_eq!(worker.segments.len(), if needs_reopen { 1 } else { 2 });
        let mut first = [0.0_f64; 1000];
        output::test_consume(&mut consumer, &mut first, 1, &callback);
        assert!(first.iter().all(|&v| v == 0.25));
        worker.tick().unwrap();
        assert_eq!(worker.snapshot.track.as_ref().unwrap().id, 2);
        assert_eq!(worker.snapshot.completed, 1);
        assert_eq!(
            worker.output.as_ref().unwrap().sample_format,
            if needs_reopen {
                SampleFormat::I32
            } else {
                initial_format
            }
        );
        if !needs_reopen {
            let mut second = [0_i32; 1000];
            output::test_consume(&mut consumer, &mut second, 1, &callback);
            assert!(second.iter().all(|&v| v == 0x4000_0001));
            worker.tick().unwrap();
            assert_eq!(worker.snapshot.completed, 2);
        }
    }
}

#[test]
fn precision_reducing_output_falls_back_without_stopping_playback() {
    let (_fixture, track) = fixture_pcm(1, 0x4000_0001, 32, 1000);
    let (sender, _) = watch::channel(PlaybackSnapshot::default());
    let mut worker = Worker::with_devices(
        sender,
        Arc::new(|| {}),
        Arc::new(AtomicBool::new(false)),
        Vec::new(),
    );
    worker.output_factory = Some(|rate, channels, format, rate_selection, selected, volume| {
        assert_eq!(rate_selection, RateSelection::Native);
        let mut output = recovery_output(rate, channels, format, rate_selection, selected, volume)?;
        output.sample_format = cpal::SampleFormat::F32;
        Ok(output)
    });
    worker
        .load(Prepared::open(track).unwrap(), None, 0, false)
        .unwrap();
    assert_eq!(worker.snapshot.state, PlaybackState::Playing);
    assert_eq!(
        worker.output.as_ref().unwrap().sample_format,
        cpal::SampleFormat::F32
    );
    assert_eq!(worker.written, 1000);
    worker.command(AudioCommand::SetVolume(0.5)).unwrap();
    assert_eq!(worker.snapshot.volume, 0.5);
    assert!(worker.snapshot.error.is_none());
}

fn update_devices(worker: &mut Worker, devices: Vec<OutputDevice>) {
    let (sender, receiver) = watch::channel(devices);
    drop(sender);
    worker.device_updates = receiver;
    worker.refresh_devices();
}

fn reject_missing_output(
    rate: u32,
    channels: usize,
    format: PcmFormat,
    rate_selection: RateSelection,
    selected: Option<&str>,
    volume: f32,
) -> Result<Output> {
    assert_eq!(rate_selection, RateSelection::Native);
    if selected == Some("Missing output") {
        bail!("Selected output device is unavailable");
    }
    recovery_output(rate, channels, format, rate_selection, selected, volume)
}

#[test]
fn failed_initial_output_selection_can_be_changed_and_retried() {
    let (_fixture, track) = fixture(1, 8192);
    let (mut worker, _consumer, _callback) = worker(track.clone(), None);
    update_devices(&mut worker, vec!["Replacement output".into()]);
    worker.output_factory = Some(reject_missing_output);
    worker
        .command(AudioCommand::Stop { generation: 8 })
        .unwrap();
    worker
        .command(AudioCommand::SetDevice(Some("Missing output".into())))
        .unwrap();
    let error = worker
        .command(AudioCommand::Load {
            track,
            next: None,
            generation: 9,
        })
        .unwrap_err();
    worker.fail(error);
    assert_eq!(
        worker.snapshot.selected_device.as_deref(),
        Some("Missing output")
    );
    assert_eq!(worker.snapshot.state, PlaybackState::Error);
    worker
        .command(AudioCommand::SetDevice(Some("Replacement output".into())))
        .unwrap();
    assert_eq!(
        worker.snapshot.selected_device.as_deref(),
        Some("Replacement output")
    );
    assert_eq!(worker.snapshot.state, PlaybackState::Playing);
    assert_eq!(worker.snapshot.generation, 9);
    assert!(worker.snapshot.error.is_none());
}

#[test]
fn selecting_replacement_output_preserves_disconnected_track_position_and_queue() {
    let (_a, a) = fixture(1, 8192);
    let (_b, b) = fixture(2, -16384);
    let (mut worker, _consumer, callback) = worker(a, Some(b));
    update_devices(&mut worker, vec!["Replacement output".into()]);
    worker.output_factory = Some(recovery_output);
    callback.consumed.store(800, Ordering::Relaxed);
    callback.failed.store(true, Ordering::Relaxed);
    worker.tick().unwrap();
    assert_eq!(worker.snapshot.state, PlaybackState::Paused);
    assert!(worker.snapshot.error.is_none());
    assert_eq!(
        worker.snapshot.notification.as_deref(),
        Some("Audio output device changed. Playback paused.")
    );
    assert_eq!(worker.snapshot.position_ms, 100);
    assert_eq!(worker.snapshot.output_rate, 0);
    assert!(worker.snapshot.device.is_empty());
    worker
        .command(AudioCommand::SetDevice(Some("Replacement output".into())))
        .unwrap();
    assert_eq!(
        worker.snapshot.selected_device.as_deref(),
        Some("Replacement output")
    );
    assert_eq!(worker.snapshot.device, "Replacement output");
    assert_eq!(
        worker.snapshot.devices,
        [OutputDevice::from("Replacement output")]
    );
    assert_eq!(worker.snapshot.state, PlaybackState::Paused);
    assert_eq!(worker.snapshot.position_ms, 100);
    assert_eq!(worker.snapshot.track.as_ref().unwrap().id, 1);
    assert_eq!(worker.future_track().unwrap().id, 2);
    assert!(worker.snapshot.error.is_none());
    assert!(worker.snapshot.notification.is_none());
    worker.command(AudioCommand::Resume).unwrap();
    assert_eq!(worker.snapshot.state, PlaybackState::Playing);
    assert_eq!(worker.position(), 100);
}

#[test]
fn default_output_recovery_preserves_pause_state_and_idle_selection_clears_error() {
    let (_fixture, track) = fixture(1, 8192);
    let (mut worker, _consumer, callback) = worker(track, None);
    update_devices(&mut worker, vec!["Replacement output".into()]);
    worker.output_factory = Some(recovery_output);
    worker.selected_device = Some("Headphones".into());
    worker.snapshot.selected_device = worker.selected_device.clone();
    callback.consumed.store(400, Ordering::Relaxed);
    worker.command(AudioCommand::Pause).unwrap();
    callback.failed.store(true, Ordering::Relaxed);
    worker.tick().unwrap();
    worker.command(AudioCommand::SetDevice(None)).unwrap();
    assert_eq!(worker.snapshot.selected_device, None);
    assert_eq!(worker.snapshot.device, "System default");
    assert_eq!(worker.snapshot.state, PlaybackState::Paused);
    assert_eq!(worker.position(), 50);
    assert!(
        worker
            .output
            .as_ref()
            .unwrap()
            .state
            .paused
            .load(Ordering::Relaxed)
    );
    worker
        .command(AudioCommand::Stop { generation: 10 })
        .unwrap();
    worker
        .command(AudioCommand::SetDevice(Some("Replacement output".into())))
        .unwrap();
    assert_eq!(
        worker.snapshot.selected_device.as_deref(),
        Some("Replacement output")
    );
    assert_eq!(worker.snapshot.state, PlaybackState::Stopped);
    assert_eq!(worker.snapshot.output_rate, 0);
    assert!(worker.snapshot.device.is_empty());
    assert!(worker.snapshot.error.is_none());
}

#[test]
fn reselecting_current_output_keeps_the_stream_and_position() {
    for selected in [None, Some("Headphones".to_string())] {
        let (_fixture, track) = fixture(1, 8192);
        let (mut worker, _consumer, callback) = worker(track, None);
        update_devices(&mut worker, vec!["Replacement output".into()]);
        worker.selected_device = selected.clone();
        worker.snapshot.selected_device = selected.clone();
        callback.consumed.store(800, Ordering::Relaxed);

        worker.command(AudioCommand::SetDevice(selected)).unwrap();

        assert!(Arc::ptr_eq(
            &worker.output.as_ref().unwrap().state,
            &callback
        ));
        assert_eq!(worker.snapshot.state, PlaybackState::Playing);
        assert_eq!(worker.position(), 100);
    }
}

#[test]
fn reselecting_disconnected_output_preserves_position_until_play_resumes() {
    for selected in [None, Some("Headphones".to_string())] {
        let (_a, a) = fixture(1, 8192);
        let (_b, b) = fixture(2, -16384);
        let (mut worker, _consumer, callback) = worker(a, Some(b));
        update_devices(&mut worker, vec!["Replacement output".into()]);
        worker.output_factory = Some(recovery_output);
        worker.selected_device = selected.clone();
        worker.snapshot.selected_device = selected.clone();
        worker.snapshot.generation = 9;
        callback.consumed.store(800, Ordering::Relaxed);
        callback.failed.store(true, Ordering::Relaxed);

        worker.tick().unwrap();
        assert_eq!(worker.snapshot.state, PlaybackState::Paused);
        assert_eq!(worker.position(), 100);
        assert!(worker.snapshot.error.is_none());
        assert!(worker.snapshot.notification.is_some());

        worker.command(AudioCommand::SetDevice(selected)).unwrap();
        assert!(worker.output.is_none());
        assert_eq!(worker.position(), 100);
        assert_eq!(worker.snapshot.state, PlaybackState::Paused);

        worker.command(AudioCommand::Resume).unwrap();
        assert_eq!(worker.snapshot.state, PlaybackState::Playing);
        assert_eq!(worker.position(), 100);
        assert_eq!(worker.snapshot.track.as_ref().unwrap().id, 1);
        assert_eq!(worker.future_track().unwrap().id, 2);
        assert_eq!(worker.snapshot.generation, 9);
        assert!(worker.snapshot.notification.is_none());
        assert!(worker.snapshot.error.is_none());
    }
}
#[test]
fn cue_tracks_keep_logical_metadata_and_recover_relative_position() {
    let (_fixture, mut track) = fixture(1, 8192);
    track.metadata = TrackMetadata {
        title: "CUE title".into(),
        artist: "CUE artist".into(),
        duration_ms: 200,
        ..Default::default()
    };
    track.segment = Some(mplayer_core::TrackSegment {
        start_frame: 801,
        end_frame: Some(2401),
    });
    let prepared = Prepared::open(track.clone()).unwrap();
    assert_eq!(prepared.track.metadata.title, "CUE title");
    assert_eq!(prepared.track.metadata.artist, "CUE artist");
    assert_eq!(prepared.track.metadata.duration_ms, 200);
    let (mut worker, _consumer, callback) = worker(track, None);
    update_devices(&mut worker, vec!["Replacement output".into()]);
    worker.output_factory = Some(recovery_output);
    callback.consumed.store(400, Ordering::Relaxed);
    worker.command(AudioCommand::Pause).unwrap();
    callback.failed.store(true, Ordering::Relaxed);
    worker.tick().unwrap();
    assert_eq!(worker.position(), 50);
    worker.command(AudioCommand::Resume).unwrap();
    assert_eq!(worker.position(), 50);
    assert_eq!(worker.snapshot.duration_ms, 200);
    assert_eq!(
        worker.snapshot.track.as_ref().unwrap().metadata.title,
        "CUE title"
    );
    worker
        .command(AudioCommand::Seek {
            position_ms: 75,
            request_id: 1,
        })
        .unwrap();
    assert_eq!(worker.position(), 75);
    assert_eq!(worker.snapshot.seek_request_id, 1);
    assert_eq!(worker.snapshot.duration_ms, 200);
}

#[test]
fn resampled_cue_segments_equal_continuous_file_playback() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tone24.flac");
    let mut decoder = Decoder::open(&path).unwrap();
    let metadata = decoder.metadata.clone();
    let first = Track {
        id: 1,
        path: path.clone(),
        metadata: TrackMetadata {
            duration_ms: 13,
            ..metadata.clone()
        },
        missing: false,
        segment: Some(mplayer_core::TrackSegment {
            start_frame: 0,
            end_frame: Some(588),
        }),
    };
    let second = Track {
        id: 2,
        path,
        metadata: TrackMetadata {
            duration_ms: 86,
            ..metadata
        },
        missing: false,
        segment: Some(mplayer_core::TrackSegment {
            start_frame: 588,
            end_frame: None,
        }),
    };
    let (mut worker, mut consumer, callback) = worker_at_rate(first, Some(second), 48_000, 2);
    let mut converter = Converter::new(44_100, 48_000, 2, 2).unwrap();
    let mut expected = Vec::new();
    let mut buffer = Vec::new();
    let mut pcm = Vec::new();
    while decoder.next(&mut buffer).unwrap() {
        pcm.extend_from_slice(&buffer);
    }
    for chunk in pcm.chunks(2048) {
        converter.convert(chunk, false, &mut buffer).unwrap();
        expected.extend_from_slice(&buffer);
    }
    converter.convert(&[], true, &mut buffer).unwrap();
    expected.extend_from_slice(&buffer);
    let mut actual = Vec::new();
    for _ in 0..48 {
        let mut data = [0.0; 200];
        output::test_consume(&mut consumer, &mut data, 2, &callback);
        actual.extend_from_slice(&data);
        worker.tick().unwrap();
    }
    assert_eq!(actual.len(), expected.len());
    for (actual, expected) in actual.iter().zip(&expected) {
        assert!((actual - expected).abs() < 1e-6);
    }
    assert_eq!(worker.snapshot.completed, 2);
    assert_eq!(worker.snapshot.state, PlaybackState::Stopped);
    assert_eq!(callback.underruns.load(Ordering::Relaxed), 0);
}

#[test]
fn seek_acknowledges_only_successful_requests_and_preserves_pause() {
    let (_fixture, track) = fixture(1, 8192);
    let (mut worker, _consumer, _callback) = worker(track, None);
    worker.output_factory = Some(reject_missing_output);
    worker.command(AudioCommand::Pause).unwrap();
    worker
        .command(AudioCommand::Seek {
            position_ms: 100,
            request_id: 7,
        })
        .unwrap();
    assert_eq!(worker.snapshot.seek_request_id, 7);
    assert_eq!(worker.snapshot.position_ms, 100);
    assert_eq!(worker.snapshot.state, PlaybackState::Paused);

    worker.selected_device = Some("Missing output".into());
    let error = worker
        .command(AudioCommand::Seek {
            position_ms: 200,
            request_id: 8,
        })
        .unwrap_err();
    worker.fail(error);
    assert_eq!(worker.snapshot.seek_request_id, 7);
    assert_eq!(worker.snapshot.state, PlaybackState::Error);
}
