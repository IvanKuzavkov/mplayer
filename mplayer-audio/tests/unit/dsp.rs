use super::*;

#[test]
fn mono_duplication_and_native_rate_are_exact() {
    let mut converter = Converter::new(44_100, 44_100, 1, 2).unwrap();
    let mut output = Vec::new();
    converter
        .convert(&[0.25, -0.5, 1.0], true, &mut output)
        .unwrap();
    assert_eq!(output, [0.25, 0.25, -0.5, -0.5, 1.0, 1.0]);
    assert_eq!(converter.target_frames().unwrap(), 3);
}

#[test]
fn short_streams_flush_exactly_once_with_either_eof_style() {
    for (source_rate, target_rate) in [(44_100, 44_100), (44_100, 48_000), (48_000, 44_100)] {
        for (input_channels, output_channels) in [(1, 1), (1, 2), (2, 2)] {
            for frames in [0, 1, 2, 147, 1023, 1024, 1025, 4097] {
                let mut separately_flushed = None;
                for eof_with_samples in [false, true] {
                    let mut converter =
                        Converter::new(source_rate, target_rate, input_channels, output_channels)
                            .unwrap();
                    let input = vec![0.25; frames * input_channels];
                    let mut output = Vec::new();
                    let mut all = Vec::new();
                    let chunk_size = 389 * input_channels;
                    for (index, chunk) in input.chunks(chunk_size).enumerate() {
                        let eof = eof_with_samples && (index + 1) * chunk_size >= input.len();
                        converter.convert(chunk, eof, &mut output).unwrap();
                        all.extend_from_slice(&output);
                    }
                    converter.convert(&[], true, &mut output).unwrap();
                    all.extend_from_slice(&output);
                    let expected = (frames as u64 * u64::from(target_rate)
                        + u64::from(source_rate) / 2)
                        / u64::from(source_rate);
                    assert_eq!(converter.target_frames().unwrap(), expected);
                    assert_eq!(all.len(), expected as usize * output_channels);
                    assert!(all.iter().all(|sample| sample.is_finite()));
                    if input_channels == 1 && output_channels == 2 {
                        assert!(all.as_chunks::<2>().0.iter().all(|pair| pair[0] == pair[1]));
                    }
                    if let Some(expected) = &separately_flushed {
                        assert_eq!(&all, expected);
                    } else {
                        separately_flushed = Some(all);
                    }
                    converter.convert(&[], true, &mut output).unwrap();
                    assert!(output.is_empty());
                    converter.convert(&[0.0, 0.0], false, &mut output).unwrap();
                    assert!(output.is_empty());
                    assert_eq!(converter.target_frames().unwrap(), expected);
                }
            }
        }
    }
}

#[test]
fn frame_count_uses_exact_integer_rounding_and_reports_overflow() {
    let mut native = Converter::new(44_100, 44_100, 2, 2).unwrap();
    native.received_frames = (1 << 53) + 1;
    assert_eq!(native.target_frames().unwrap(), native.received_frames);

    let mut upsampled = Converter::new(24_000, 48_000, 2, 2).unwrap();
    upsampled.received_frames = (1 << 53) + 1;
    assert_eq!(
        upsampled.target_frames().unwrap(),
        upsampled.received_frames * 2
    );
    upsampled.received_frames = u64::MAX;
    assert!(upsampled.target_frames().is_err());

    let mut downsampled = Converter::new(48_000, 24_000, 2, 2).unwrap();
    for (received, expected) in [(0, 0), (1, 1), (2, 1), (3, 2)] {
        downsampled.received_frames = received;
        assert_eq!(downsampled.target_frames().unwrap(), expected);
    }
}

#[test]
fn conversion_flushes_exact_frame_count_without_startup_silence() {
    for (source_rate, target_rate) in [(44_100, 48_000), (48_000, 44_100), (44_100, 96_000)] {
        let mut converter = Converter::new(source_rate, target_rate, 2, 2).unwrap();
        let input = vec![0.25; source_rate as usize * 2];
        let mut output = Vec::new();
        let mut all = Vec::new();
        for chunk in input.chunks(778) {
            converter.convert(chunk, false, &mut output).unwrap();
            all.extend_from_slice(&output);
        }
        converter.convert(&[], true, &mut output).unwrap();
        all.extend_from_slice(&output);
        assert_eq!(all.len(), target_rate as usize * 2);
        assert!(all.iter().all(|sample| sample.is_finite()));
        assert!((all[target_rate as usize] - 0.25).abs() < 0.002);
    }
}

#[test]
fn resampling_preserves_tones_across_irregular_chunk_boundaries() {
    for (source_rate, target_rate) in [
        (192_000, 96_000),
        (96_000, 48_000),
        (44_100, 48_000),
        (48_000, 44_100),
    ] {
        let tone = |frame: usize, rate: u32, frequency: f64| {
            (std::f64::consts::TAU * frequency * frame as f64 / f64::from(rate)).sin() * 0.25
        };
        let input: Vec<f64> = (0..source_rate as usize)
            .flat_map(|frame| {
                [
                    tone(frame, source_rate, 440.0) + tone(frame, source_rate, 4_000.0),
                    tone(frame, source_rate, 12_000.0),
                ]
            })
            .collect();
        let mut converter = Converter::new(source_rate, target_rate, 2, 2).unwrap();
        let mut converted = Vec::new();
        let mut all = Vec::new();
        let mut offset = 0;
        for frames in [1, 147, 389, 1024].into_iter().cycle() {
            if offset == input.len() {
                break;
            }
            let end = (offset + frames * 2).min(input.len());
            converter
                .convert(&input[offset..end], false, &mut converted)
                .unwrap();
            all.extend_from_slice(&converted);
            offset = end;
        }
        converter.convert(&[], true, &mut converted).unwrap();
        all.extend_from_slice(&converted);
        assert_eq!(all.len(), target_rate as usize * 2);
        // Exclude the filter's start/end transients. Every interior sample,
        // including FFT and input chunk boundaries, follows the original tone.
        for frame in 2048..target_rate as usize - 2048 {
            let expected = [
                tone(frame, target_rate, 440.0) + tone(frame, target_rate, 4_000.0),
                tone(frame, target_rate, 12_000.0),
            ];
            for channel in 0..2 {
                let error = (all[frame * 2 + channel] - expected[channel]).abs();
                assert!(
                    error < 0.000_002,
                    "{source_rate}->{target_rate}, frame {frame}, channel {channel}: {error}"
                );
            }
        }
    }
}

#[test]
fn rejects_unmapped_layouts_and_partial_frames() {
    assert!(Converter::new(48_000, 48_000, 6, 2).is_err());
    let mut converter = Converter::new(48_000, 48_000, 2, 2).unwrap();
    assert!(converter.convert(&[0.0], false, &mut Vec::new()).is_err());
    assert!(Converter::new(1, 768_000, 2, 2).is_err());
    assert!(Converter::new(767_999, 768_000, 2, 2).is_err());
    assert!(
        converter
            .convert(&vec![0.0; 2050], false, &mut Vec::new())
            .is_err()
    );
}
