use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_FILE: AtomicU64 = AtomicU64::new(0);

struct Fixture(std::path::PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn wav(bits: u16) -> Fixture {
    let values: Vec<_> = (0..8000)
        .map(|frame| {
            if frame < 4000 {
                1 << (bits - 3)
            } else {
                -(1 << (bits - 2))
            }
        })
        .collect();
    wav_samples(bits, &values)
}

fn wav_samples(bits: u16, values: &[i32]) -> Fixture {
    let samples: Vec<_> = values
        .iter()
        .flat_map(|value| value.to_le_bytes()[..usize::from(bits / 8)].to_vec())
        .collect();
    wav_bytes(bits, 1, &samples)
}

fn wav_bytes(bits: u16, encoding: u16, samples: &[u8]) -> Fixture {
    let bytes_per_sample = bits / 8;
    let data_size = samples.len() as u32;
    let mut data = Vec::new();
    data.extend_from_slice(b"RIFF");
    data.extend_from_slice(&(36 + data_size).to_le_bytes());
    data.extend_from_slice(b"WAVEfmt ");
    data.extend_from_slice(&16_u32.to_le_bytes());
    data.extend_from_slice(&encoding.to_le_bytes());
    data.extend_from_slice(&1_u16.to_le_bytes());
    data.extend_from_slice(&8000_u32.to_le_bytes());
    data.extend_from_slice(&(8000 * u32::from(bytes_per_sample)).to_le_bytes());
    data.extend_from_slice(&bytes_per_sample.to_le_bytes());
    data.extend_from_slice(&bits.to_le_bytes());
    data.extend_from_slice(b"data");
    data.extend_from_slice(&data_size.to_le_bytes());
    data.extend_from_slice(samples);
    let path = std::env::temp_dir().join(format!(
        "mplayer-audio-{}-{}.wav",
        std::process::id(),
        NEXT_FILE.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&path, data).unwrap();
    Fixture(path)
}

#[test]
fn decodes_16_24_and_32_bit_pcm_and_reports_duration() {
    for bits in [16, 24, 32] {
        let fixture = wav(bits);
        let mut decoder = Decoder::open(&fixture.0).unwrap();
        assert_eq!(decoder.metadata.duration_ms, 1000);
        assert_eq!(decoder.metadata.bit_depth, Some(u32::from(bits)));
        let mut samples = Vec::new();
        let mut all = Vec::new();
        while decoder.next(&mut samples).unwrap() {
            all.extend_from_slice(&samples);
        }
        assert_eq!(all.len(), 8000);
        assert_eq!(all[0], 0.25);
        assert_eq!(all[7999], -0.5);
    }
}

#[test]
fn floating_point_decoding_preserves_the_original_precision() {
    let samples = [
        -0.0_f64,
        0.0,
        -0.125,
        0.25,
        1.0 + f64::EPSILON,
        f64::MIN_POSITIVE,
    ];
    for bits in [32, 64] {
        let bytes: Vec<_> = if bits == 32 {
            samples
                .iter()
                .flat_map(|&v| (v as f32).to_le_bytes())
                .collect()
        } else {
            samples.iter().flat_map(|v| v.to_le_bytes()).collect()
        };
        let fixture = wav_bytes(bits, 3, &bytes);
        let mut decoder = Decoder::open(&fixture.0).unwrap();
        assert_eq!(decoder.pcm_format(), PcmFormat::Float(u32::from(bits)));
        let mut decoded = Vec::new();
        decoder.next(&mut decoded).unwrap();
        let expected: Vec<_> = samples
            .iter()
            .map(|&v| if bits == 32 { f64::from(v as f32) } else { v })
            .map(f64::to_bits)
            .collect();
        assert_eq!(
            decoded.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
            expected
        );
    }
}

#[test]
fn integer_low_bits_survive_decoding_conversion_and_callback() {
    use crate::{
        dsp::Converter,
        output::{Output, test_consume},
    };

    for bits in [16, 24, 32] {
        let minimum = i32::MIN >> (32 - bits);
        let maximum = i32::MAX >> (32 - bits);
        let mut values = vec![minimum, minimum + 1, maximum, maximum - 1, 0, 1, -1];
        for bit in 0..bits - 1 {
            values.extend([1 << bit, -(1 << bit)]);
        }
        let mut seed = 7_u32;
        values.extend((0..10_000).map(|_| {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed as i32) >> (32 - bits)
        }));
        let fixture = wav_samples(bits, &values);
        let mut decoder = Decoder::open(&fixture.0).unwrap();
        assert_eq!(decoder.pcm_format(), PcmFormat::Integer(u32::from(bits)));
        let mut converter = Converter::new(8000, 8000, 1, 1).unwrap();
        let (mut output, mut consumer) = Output::fake(8000, 1);
        output.pause(false);
        let mut decoded = Vec::new();
        let mut converted = Vec::new();
        let mut all = Vec::new();
        while decoder.next(&mut decoded).unwrap() {
            for chunk in decoded.chunks(389) {
                converter.convert(chunk, false, &mut converted).unwrap();
                assert_eq!(output.write(&converted), chunk.len());
                let mut rendered = vec![0_i32; chunk.len()];
                test_consume(&mut consumer, &mut rendered, 1, &output.state);
                all.extend(rendered);
            }
        }
        let expected: Vec<_> = values.iter().map(|&v| v << (32 - bits)).collect();
        assert_eq!(all, expected, "{bits}-bit PCM changed");
        decoder.seek(125).unwrap();
        decoder.next(&mut decoded).unwrap();
        assert_eq!(decoded[0], f64::from(expected[1000]) / 2_147_483_648.0);
    }
}

#[test]
fn seeking_discards_old_samples_and_starts_at_requested_frame() {
    let fixture = wav(16);
    let mut decoder = Decoder::open(&fixture.0).unwrap();
    decoder.seek(500).unwrap();
    let mut samples = Vec::new();
    let mut frames = 0;
    while decoder.next(&mut samples).unwrap() {
        assert!(samples.iter().all(|sample| *sample == -0.5));
        frames += samples.len();
    }
    assert_eq!(frames, 4000);
}

#[test]
fn malformed_input_returns_error() {
    let path = std::env::temp_dir().join(format!("mplayer-malformed-{}.flac", std::process::id()));
    std::fs::write(&path, b"not an audio file").unwrap();
    let fixture = Fixture(path);
    assert!(Decoder::open(&fixture.0).is_err());
}

#[test]
fn flac_16_and_24_bit_fixtures_decode_losslessly_and_seek() {
    for bits in [16, 24] {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("tests/fixtures/tone{bits}.flac"));
        let mut decoder = Decoder::open(&path).unwrap();
        assert_eq!(decoder.metadata.sample_rate, 44_100);
        assert_eq!(decoder.metadata.channels, 2);
        assert_eq!(decoder.metadata.bit_depth, Some(bits));
        assert_eq!(decoder.metadata.duration_ms, 100);
        let mut samples = Vec::new();
        let mut all = Vec::new();
        while decoder.next(&mut samples).unwrap() {
            all.extend_from_slice(&samples);
        }
        assert_eq!(all.len(), 4410 * 2);
        let scale = (1_u32 << (bits - 1)) as f64;
        for (index, frame) in all.as_chunks::<2>().0.iter().enumerate() {
            let expected = ((std::f64::consts::TAU * 440.0 * index as f64 / 44_100.0).sin()
                * (scale - 1.0)
                / 4.0)
                .round()
                / scale;
            assert_eq!(frame[0], expected);
            assert_eq!(frame[0], frame[1]);
        }
        decoder.seek(50).unwrap();
        let mut remaining = Vec::new();
        while decoder.next(&mut samples).unwrap() {
            remaining.extend_from_slice(&samples);
        }
        assert_eq!(remaining, all[2205 * 2..]);
    }
}

#[test]
fn mp3_silence_fixture_decodes_without_device_access() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/silence.mp3");
    let mut decoder = Decoder::open(&path).unwrap();
    assert_eq!(decoder.metadata.sample_rate, 44_100);
    assert_eq!(decoder.metadata.channels, 2);
    let mut samples = Vec::new();
    let mut frames = 0;
    while decoder.next(&mut samples).unwrap() {
        assert!(
            samples
                .iter()
                .all(|sample| sample.is_finite() && sample.abs() < 0.000_001)
        );
        frames += samples.len() / 2;
    }
    assert_eq!(frames, 20 * 1152);
}

#[test]
fn gapless_mp3_near_start_seeking_does_not_trim_encoder_delay_twice() {
    let mut bytes = vec![0; 417];
    bytes[..4].copy_from_slice(&[0xff, 0xfb, 0x90, 0]);
    bytes[36..40].copy_from_slice(b"Info");
    bytes[40..44].copy_from_slice(&1_u32.to_be_bytes());
    bytes[44..48].copy_from_slice(&20_u32.to_be_bytes());
    bytes[48..57].copy_from_slice(b"Lavc60.00");
    // LAME-compatible encoder trim:576 encoder+529 decoder delay,529 padding.
    let trim = (576_u32 << 12) | 1058;
    bytes[69..72].copy_from_slice(&trim.to_be_bytes()[1..]);
    bytes.extend_from_slice(include_bytes!("../fixtures/silence.mp3"));
    let path = std::env::temp_dir().join(format!("mplayer-gapless-{}.mp3", std::process::id()));
    std::fs::write(&path, bytes).unwrap();
    let fixture = Fixture(path);
    let mut decoder = Decoder::open(&fixture.0).unwrap();
    let mut samples = Vec::new();
    let mut original = Vec::new();
    while decoder.next(&mut samples).unwrap() {
        original.extend_from_slice(&samples);
    }
    assert_eq!(original.len(), (20 * 1152 - 1105 - 529) * 2);
    for (position_ms, start_frame) in [(0, 0), (1, 44)] {
        decoder.seek(position_ms).unwrap();
        let mut seeked = Vec::new();
        while decoder.next(&mut samples).unwrap() {
            seeked.extend_from_slice(&samples);
        }
        assert_eq!(seeked, original[start_frame * 2..]);
    }
    decoder
        .set_segment(TrackSegment {
            start_frame: 44,
            end_frame: None,
        })
        .unwrap();
    let mut segmented = Vec::new();
    while decoder.next(&mut samples).unwrap() {
        segmented.extend_from_slice(&samples);
    }
    assert_eq!(segmented, original[44 * 2..]);
}

#[test]
fn vorbis_fixture_decodes_to_finite_silent_stereo_frames() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/silence.ogg");
    let mut decoder = Decoder::open(&path).unwrap();
    assert_eq!(decoder.metadata.sample_rate, 44_100);
    assert_eq!(decoder.metadata.channels, 2);
    let mut samples = Vec::new();
    let mut frames = 0;
    while decoder.next(&mut samples).unwrap() {
        assert!(
            samples
                .iter()
                .all(|sample| sample.is_finite() && sample.abs() < 0.000_001)
        );
        frames += samples.len() / 2;
    }
    assert_eq!(frames, 320);
}

fn flac_with_blocks(blocks: Vec<(u8, Vec<u8>)>) -> Fixture {
    let original = include_bytes!("../fixtures/tone16.flac");
    let mut data = original[..42].to_vec();
    data[4] = 0;
    for (index, (kind, block)) in blocks.iter().enumerate() {
        data.push(kind | if index + 1 == blocks.len() { 0x80 } else { 0 });
        data.extend_from_slice(&(block.len() as u32).to_be_bytes()[1..]);
        data.extend_from_slice(block);
    }
    data.extend_from_slice(&original[42..]);
    let path = std::env::temp_dir().join(format!(
        "mplayer-tagged-{}-{}.flac",
        std::process::id(),
        NEXT_FILE.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&path, data).unwrap();
    Fixture(path)
}

fn picture(kind: u32, image: &[u8]) -> Vec<u8> {
    let mut data = kind.to_be_bytes().to_vec();
    data.extend_from_slice(&9_u32.to_be_bytes());
    data.extend_from_slice(b"image/png");
    data.extend_from_slice(&0_u32.to_be_bytes());
    for value in [1_u32, 1, 32, 0, image.len() as u32] {
        data.extend_from_slice(&value.to_be_bytes());
    }
    data.extend_from_slice(image);
    data
}

#[test]
fn album_artist_and_track_disc_numbers_are_extracted_from_flac_comments() {
    let mut comments = 0_u32.to_le_bytes().to_vec();
    let tags = [
        "TITLE=Blue",
        "ARTIST=Soloist",
        "ALBUM=Record",
        "ALBUMARTIST=Ensemble",
        "TRACKNUMBER=7",
        "DISCNUMBER=2",
        "DATE=2026-11-03",
    ];
    comments.extend_from_slice(&(tags.len() as u32).to_le_bytes());
    for tag in tags {
        comments.extend_from_slice(&(tag.len() as u32).to_le_bytes());
        comments.extend_from_slice(tag.as_bytes());
    }
    let fixture = flac_with_blocks(vec![(4, comments)]);
    let metadata = crate::metadata_probe().probe(&fixture.0).unwrap();
    assert_eq!(metadata.title, "Blue");
    assert_eq!(metadata.artist, "Soloist");
    assert_eq!(metadata.album, "Record");
    assert_eq!(metadata.album_artist, "Ensemble");
    assert_eq!(metadata.track_number, Some(7));
    assert_eq!(metadata.disc_number, Some(2));
    assert_eq!(metadata.year, Some(2026));
}

#[test]
fn unavailable_or_invalid_years_are_not_invented() {
    for tag in ["DATE=unknown", "DATE=0000", "DATE=", "ARTIST=No year"] {
        let mut comments = 0_u32.to_le_bytes().to_vec();
        comments.extend_from_slice(&1_u32.to_le_bytes());
        comments.extend_from_slice(&(tag.len() as u32).to_le_bytes());
        comments.extend_from_slice(tag.as_bytes());
        let fixture = flac_with_blocks(vec![(4, comments)]);
        assert_eq!(
            crate::metadata_probe().probe(&fixture.0).unwrap().year,
            None
        );
    }
}

#[test]
fn embedded_artwork_prefers_front_cover_and_never_enters_track_metadata() {
    let fixture = flac_with_blocks(vec![(6, picture(4, b"back")), (6, picture(3, b"front"))]);
    let artwork = crate::metadata_probe()
        .artwork(&fixture.0)
        .unwrap()
        .unwrap();
    assert_eq!(artwork.data.as_ref(), b"front");
    assert_eq!(artwork.mime_type, "image/png");
    let metadata = crate::metadata_probe().probe(&fixture.0).unwrap();
    assert_eq!(metadata.sample_rate, 44_100);
    assert_eq!(metadata.duration_ms, 100);
    let plain = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tone16.flac");
    assert!(crate::metadata_probe().artwork(&plain).unwrap().is_none());
}

#[test]
fn oversized_flac_artwork_is_skipped_and_truncated_picture_reports_error() {
    let fixture = flac_with_blocks(vec![(6, picture(3, &vec![0; MAX_ARTWORK_BYTES + 1]))]);
    assert!(
        crate::metadata_probe()
            .artwork(&fixture.0)
            .unwrap()
            .is_none()
    );
    let truncated = flac_with_blocks(vec![(6, vec![0; 3])]);
    assert!(crate::metadata_probe().artwork(&truncated.0).is_err());
}

#[test]
fn mp3_id3_front_cover_uses_the_generic_metadata_artwork_path() {
    let mut apic = vec![0];
    apic.extend_from_slice(b"image/png\0\x03\0embedded-front");
    let mut frame = b"APIC".to_vec();
    frame.extend_from_slice(&(apic.len() as u32).to_be_bytes());
    frame.extend_from_slice(&[0, 0]);
    frame.extend_from_slice(&apic);
    let length = frame.len() as u32;
    let mut bytes = b"ID3\x03\0\0".to_vec();
    bytes.extend_from_slice(&[
        ((length >> 21) & 0x7f) as u8,
        ((length >> 14) & 0x7f) as u8,
        ((length >> 7) & 0x7f) as u8,
        (length & 0x7f) as u8,
    ]);
    bytes.extend_from_slice(&frame);
    bytes.extend_from_slice(include_bytes!("../fixtures/silence.mp3"));
    let path = std::env::temp_dir().join(format!(
        "mplayer-artwork-{}-{}.mp3",
        std::process::id(),
        NEXT_FILE.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&path, bytes).unwrap();
    let fixture = Fixture(path);
    let artwork = crate::metadata_probe()
        .artwork(&fixture.0)
        .unwrap()
        .unwrap();
    assert_eq!(artwork.data.as_ref(), b"embedded-front");
    assert_eq!(artwork.mime_type, "image/png");
}

#[test]
#[ignore = "manual release-mode decode throughput measurement"]
fn decode_throughput() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tone24.flac");
    let started = std::time::Instant::now();
    let mut total_frames = 0;
    let mut samples = Vec::new();
    for _ in 0..1000 {
        let mut decoder = Decoder::open(&path).unwrap();
        while decoder.next(&mut samples).unwrap() {
            total_frames += samples.len() / 2;
        }
    }
    let elapsed = started.elapsed();
    let audio_seconds = total_frames as f64 / 44_100.0;
    eprintln!(
        "Decoded {audio_seconds:.3}s of stereo24bit FLAC in {:.6}s: {:.1}x real time",
        elapsed.as_secs_f64(),
        audio_seconds / elapsed.as_secs_f64()
    );
    assert_eq!(total_frames, 4_410_000);
}
#[test]
fn segments_trim_wav_and_flac_at_exact_frames_and_seek_relative_to_start() {
    let wave = wav(16);
    let fixtures = [
        wave.0.clone(),
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tone16.flac"),
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tone24.flac"),
    ];
    for path in fixtures {
        let mut full = Decoder::open(&path).unwrap();
        let rate = full.metadata.sample_rate;
        let channels = usize::from(full.metadata.channels);
        let mut expected = Vec::new();
        let mut buffer = Vec::new();
        while full.next(&mut buffer).unwrap() {
            expected.extend_from_slice(&buffer);
        }
        let mut decoder = Decoder::open(&path).unwrap();
        decoder
            .set_segment(TrackSegment {
                start_frame: 589,
                end_frame: Some(1901),
            })
            .unwrap();
        let mut actual = Vec::new();
        while decoder.next(&mut buffer).unwrap() {
            actual.extend_from_slice(&buffer);
        }
        assert_eq!(
            actual,
            expected[589 * channels..1901 * channels],
            "{}",
            path.display()
        );
        decoder.seek(1).unwrap();
        actual.clear();
        while decoder.next(&mut buffer).unwrap() {
            actual.extend_from_slice(&buffer);
        }
        let start = 589 + rate as usize / 1000;
        assert_eq!(actual, expected[start * channels..1901 * channels]);
        decoder
            .set_segment(TrackSegment {
                start_frame: 1901,
                end_frame: None,
            })
            .unwrap();
        actual.clear();
        while decoder.next(&mut buffer).unwrap() {
            actual.extend_from_slice(&buffer);
        }
        assert_eq!(actual, expected[1901 * channels..]);
    }
}
