use super::*;

#[test]
fn kaiser_bessel_series_matches_high_precision_reference() {
    // Defining series evaluated to 60 decimal digits, then rounded to f64.
    for (x, expected) in [
        (0.0, 1.0),
        (1.0, 1.266_065_877_752_008_4),
        (10.0, 2_815.716_628_466_254_4),
    ] {
        assert!((bessel_i0(x) - expected).abs() <= expected * 2e-15);
    }
}

fn response(taps: &[f64], f: f64, rate: f64) -> f64 {
    let half = taps.len() / 2;
    taps[half]
        + 2.0
            * taps[..half]
                .iter()
                .enumerate()
                .map(|(i, tap)| tap * (2.0 * PI * f * (half - i) as f64 / rate).cos())
                .sum::<f64>()
}

fn equivalent(rate: u32) -> Vec<f64> {
    let mut result = coefficients(128, 1.0 / 16.0);
    let mut step = 8;
    let mut intermediate = rate / 8;
    while intermediate > PCM_RATE {
        let taps = coefficients(30, 1.0 / 4.0);
        let mut next = vec![0.0; result.len() + (taps.len() - 1) * step];
        for (i, &value) in result.iter().enumerate() {
            for (j, &tap) in taps.iter().enumerate() {
                next[i + j * step] += value * tap;
            }
        }
        result = next;
        intermediate /= 2;
        step *= 2;
    }
    for (taps, spacing) in [
        (audio_coefficients(), step * 2),
        (coefficients(MASK_TAPS - 1, 0.25), step),
    ] {
        let mut next = vec![0.0; result.len() + (taps.len() - 1) * spacing];
        for (i, &value) in result.iter().enumerate() {
            for (j, &tap) in taps.iter().enumerate() {
                next[i + j * spacing] += value * tap;
            }
        }
        result = next;
    }
    result
}

#[test]
fn passband_and_every_source_alias_band_meet_quality_limits() {
    let first = coefficients(128, 1.0 / 16.0);
    let half = coefficients(30, 1.0 / 4.0);
    let last = audio_coefficients();
    let mask = coefficients(MASK_TAPS - 1, 0.25);
    for rate in [2_822_400, 5_644_800, 11_289_600] {
        let cascade = |frequency: u32| {
            let frequency = f64::from(frequency);
            let mut gain = response(&first, frequency, f64::from(rate));
            let mut intermediate = rate / 8;
            while intermediate > PCM_RATE {
                gain *= response(&half, frequency, f64::from(intermediate));
                intermediate /= 2;
            }
            gain * response(&last, frequency, f64::from(PCM_RATE) / 2.0)
                * response(&mask, frequency, f64::from(PCM_RATE))
        };
        for frequency in (0..=24_000).step_by(100) {
            let db = 20.0 * cascade(frequency).abs().log10();
            assert!(
                db.abs() <= 0.01,
                "rate={rate} frequency={frequency} dB={db}"
            );
        }
        for frequency in (30_000..=100_000)
            .step_by(100)
            .chain((100_000..=rate / 2).step_by(500))
        {
            let db = 20.0 * cascade(frequency).abs().log10();
            assert!(db <= -90.0, "rate={rate} frequency={frequency} dB={db}");
        }
    }
}

#[test]
fn stages_match_equivalent_fir_and_parallel_matches_sequential() {
    let pool = ThreadPoolBuilder::new()
        .num_threads(2)
        .stack_size(WORKER_STACK_BYTES)
        .build()
        .unwrap();
    for rate in [2_822_400, 5_644_800, 11_289_600] {
        let filter = Filter::get(rate);
        let taps = equivalent(rate);
        assert_eq!(taps.len(), 2 * filter.half + 1);
        for channels in [1, 2] {
            for frames in [
                0_usize, 1, 127, 128, 255, 256, 511, 512, 1023, 1024, 1025, 4095, 4096, 4097,
            ] {
                let length = if frames == 0 {
                    0
                } else {
                    ((frames - 1) * filter.factor / 8 + filter.half / 4 + 1) * channels
                };
                let mut state = 0x0123_4567_89ab_cdef_u64;
                let bytes: Vec<_> = (0..length)
                    .map(|_| {
                        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                        (state >> 32) as u8
                    })
                    .collect();
                let mut output = vec![0.0; frames * channels];
                let mut parallel = output.clone();
                let mut scratch = Scratch::default();
                filter.process_sequential(&bytes, channels, &mut output, &mut scratch);
                filter.process_with_pool(
                    &bytes,
                    channels,
                    &mut parallel,
                    &mut scratch,
                    Some(&pool),
                );
                assert!(
                    output
                        .iter()
                        .zip(&parallel)
                        .all(|(a, b)| a.to_bits() == b.to_bits())
                );
                for threshold in [1 << 18, 1 << 19, usize::MAX] {
                    filter.process_at_threshold(
                        &bytes,
                        channels,
                        &mut parallel,
                        &mut scratch,
                        threshold,
                    );
                    assert!(
                        output
                            .iter()
                            .zip(&parallel)
                            .all(|(a, b)| a.to_bits() == b.to_bits())
                    );
                }
                // Full direct convolution at representative positions catches phase,
                // normalization and stage-assembly errors without excessive test work.
                for frame in [0, frames / 2, frames.saturating_sub(1)]
                    .into_iter()
                    .filter(|&f| f < frames)
                {
                    for ch in 0..channels {
                        let start = frame * filter.factor;
                        let expected: f64 = taps
                            .iter()
                            .enumerate()
                            .map(|(i, tap)| {
                                let bit = start + i;
                                tap * if bytes[bit / 8 * channels + ch] & (0x80 >> (bit % 8)) == 0 {
                                    -1.0
                                } else {
                                    1.0
                                }
                            })
                            .sum();
                        assert!(
                            (expected - output[frame * channels + ch]).abs() < 1e-12,
                            "rate={rate}, channels={channels}, frames={frames}, frame={frame}"
                        );
                    }
                }
            }
        }
    }
}

// Use absolute source-bit coordinates, not the decoder's padded byte window or
// any staged/packed filtering, so window offsets and EOF masks are under test.
fn streaming_reference(
    taps: &[f64],
    source: &[u8],
    samples: usize,
    channels: usize,
    factor: usize,
    frame: usize,
    channel: usize,
) -> f64 {
    let first = (frame * factor) as i128 - (taps.len() / 2) as i128;
    taps.iter()
        .enumerate()
        .map(|(index, tap)| {
            let bit = first + index as i128;
            let positive = if (0..samples as i128).contains(&bit) {
                let bit = bit as usize;
                source[bit / 8 * channels + channel] & (0x80 >> (bit % 8)) != 0
            } else {
                // MSB-first 0x55 silence, including negative source positions.
                bit.rem_euclid(2) == 1
            };
            tap * if positive { 1.0 } else { -1.0 }
        })
        .sum()
}

#[test]
fn dsf_streaming_boundaries_match_direct_fir() {
    use crate::dsd::{
        DsdDecoder,
        tests::{Fixture, dsf},
    };

    let pool = ThreadPoolBuilder::new()
        .num_threads(2)
        .stack_size(WORKER_STACK_BYTES)
        .build()
        .unwrap();
    const FRAMES: usize = 3 * 4096 + 37;
    for rate in [2_822_400, 5_644_800, 11_289_600] {
        let factor = (rate / PCM_RATE) as usize;
        let taps = equivalent(rate);
        // Both a partial final PCM block and a partial final source byte. The
        // last PCM center has only five real source bits before EOF padding.
        let samples = (FRAMES - 1) * factor + 5;
        let mut edges = vec![0, FRAMES - 1];
        edges.extend((4096..FRAMES).step_by(4096));
        edges.extend((2048..FRAMES).step_by(4096));
        // DSF channel blocks are 4096 source bytes, not 4096 PCM frames.
        edges.extend((4096 * 8 / factor..FRAMES).step_by(4096 * 8 / factor));
        let mut positions: Vec<_> = edges
            .into_iter()
            .flat_map(|edge| edge.saturating_sub(2)..=(edge + 2).min(FRAMES - 1))
            .collect();
        positions.sort_unstable();
        positions.dedup();

        for channels in [1, 2] {
            // High LCG bits avoid repeating a short pattern at either kind of
            // block boundary; interleaved channels receive different bytes.
            let mut state = 0x0123_4567_89ab_cdef_u64;
            let mut source: Vec<_> = (0..samples.div_ceil(8) * channels)
                .map(|_| {
                    state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                    (state >> 32) as u8
                })
                .collect();
            let length = source.len();
            for byte in &mut source[length - channels..] {
                // Deliberately disagree with 0x55 in every unused bit.
                *byte = (*byte & 0xf8) | (0xaa & 0x07);
            }
            let expected: Vec<_> = positions
                .iter()
                .map(|&frame| {
                    (0..channels)
                        .map(|channel| {
                            streaming_reference(
                                &taps, &source, samples, channels, factor, frame, channel,
                            )
                        })
                        .collect::<Vec<_>>()
                })
                .collect();
            for lsb in [false, true] {
                let file = Fixture::new(
                    &dsf(rate, channels, samples as u64, lsb, &source, &[]),
                    "dsf",
                );
                for mode in ["default", "sequential", "two-worker"] {
                    let mut decoder = DsdDecoder::open(&file.0).unwrap();
                    let mut output = Vec::new();
                    let mut start = 0;
                    let mut checked = 0;
                    loop {
                        let more = match mode {
                            "default" => decoder.next(&mut output),
                            "sequential" => {
                                decoder.next_with(&mut output, Filter::process_sequential)
                            }
                            _ => decoder.next_with(
                                &mut output,
                                |filter, bytes, channels, out, scratch| {
                                    filter.process_with_pool(
                                        bytes,
                                        channels,
                                        out,
                                        scratch,
                                        Some(&pool),
                                    );
                                },
                            ),
                        }
                        .unwrap();
                        let context = format!(
                            "rate={rate}, channels={channels}, lsb={lsb}, mode={mode}, start={start}"
                        );
                        if !more {
                            assert!(output.is_empty(), "{context}");
                            break;
                        }
                        let frames = (FRAMES - start).min(4096);
                        assert_eq!(output.len(), frames * channels, "{context}");
                        assert!(output.iter().all(|value| value.is_finite()), "{context}");
                        for (index, &frame) in positions.iter().enumerate() {
                            if (start..start + frames).contains(&frame) {
                                for channel in 0..channels {
                                    let actual = output[(frame - start) * channels + channel];
                                    let reference = expected[index][channel];
                                    assert!(
                                        (actual - reference).abs() < 1e-12,
                                        "{context}, frame={frame}, channel={channel}, actual={actual}, reference={reference}"
                                    );
                                }
                                checked += 1;
                            }
                        }
                        start += frames;
                    }
                    assert_eq!(start, FRAMES);
                    assert_eq!(checked, positions.len());
                    // EOF stays terminal and clears a caller's stale samples.
                    output.push(f64::NAN);
                    assert!(!decoder.next(&mut output).unwrap());
                    assert!(output.is_empty());
                }
            }
        }
    }
}

#[test]
fn zero_tap_elision_and_shared_kernel_storage_are_valid() {
    let first = coefficients(128, 1.0 / 16.0);
    for (index, &tap) in first.iter().enumerate().step_by(8) {
        assert_eq!(tap == 0.0, index != 64);
    }
    for length in [31, MASK_TAPS] {
        let halfband = coefficients(length - 1, 1.0 / 4.0);
        for (index, &tap) in halfband.iter().enumerate().skip(1).step_by(2) {
            assert_eq!(tap == 0.0, index != length / 2);
        }
    }
    let filter = Filter::get(2_822_400);
    assert!(Arc::ptr_eq(
        &filter.kernels,
        &Filter::get(11_289_600).kernels
    ));
    assert!(filter.table_bytes() < 18 * 1024);
    if let Some(pool) = pool() {
        assert_eq!(pool.current_num_threads(), 2);
        assert!(std::ptr::eq(pool, super::pool().unwrap()));
    }
}

#[test]
fn constant_signals_and_silence_have_correct_gain_for_short_blocks() {
    for rate in [2_822_400, 5_644_800, 11_289_600] {
        let filter = Filter::get(rate);
        for channels in [1, 2] {
            let frames = 257;
            let length = ((frames - 1) * filter.factor / 8 + filter.half / 4 + 1) * channels;
            let mut output = vec![0.0; frames * channels];
            let mut scratch = Scratch::default();
            for (byte, level) in [(0x00, -1.0), (0xff, 1.0), (0x55, 0.0), (0xaa, 0.0)] {
                filter.process(&vec![byte; length], channels, &mut output, &mut scratch);
                assert!(
                    output
                        .iter()
                        .all(|value| value.is_finite() && (value - level).abs() < 1e-7)
                );
            }
        }
    }
}
