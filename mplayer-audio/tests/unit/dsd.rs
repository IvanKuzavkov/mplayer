use super::*;
use crate::{decoder::Decoder, pcm::PcmFormat};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_FILE: AtomicU64 = AtomicU64::new(0);

pub(crate) struct Fixture(pub PathBuf);
impl Fixture {
    pub fn new(data: &[u8], extension: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "mplayer-dsd-{}-{}.{}",
            std::process::id(),
            NEXT_FILE.fetch_add(1, Ordering::Relaxed),
            extension
        ));
        std::fs::write(&path, data).unwrap();
        Self(path)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

pub(crate) fn dsf(
    rate: u32,
    channels: usize,
    samples: u64,
    lsb: bool,
    data: &[u8],
    tag: &[u8],
) -> Vec<u8> {
    assert_eq!(data.len(), samples.div_ceil(8) as usize * channels);
    let block = 4096;
    let blocks = (data.len() / channels).div_ceil(block);
    let audio_length = blocks * block * channels;
    let tag_offset = 28 + 52 + 12 + audio_length;
    let length = tag_offset + tag.len();
    let mut file = Vec::new();
    file.extend_from_slice(b"DSD ");
    file.extend_from_slice(&28_u64.to_le_bytes());
    file.extend_from_slice(&(length as u64).to_le_bytes());
    file.extend_from_slice(&(if tag.is_empty() { 0 } else { tag_offset as u64 }).to_le_bytes());
    file.extend_from_slice(b"fmt ");
    file.extend_from_slice(&52_u64.to_le_bytes());
    for value in [
        1,
        0,
        channels as u32,
        channels as u32,
        rate,
        if lsb { 1 } else { 8 },
    ] {
        file.extend_from_slice(&value.to_le_bytes());
    }
    file.extend_from_slice(&samples.to_le_bytes());
    file.extend_from_slice(&(block as u32).to_le_bytes());
    file.extend_from_slice(&0_u32.to_le_bytes());
    file.extend_from_slice(b"data");
    file.extend_from_slice(&(audio_length as u64 + 12).to_le_bytes());
    for index in 0..blocks {
        for ch in 0..channels {
            for i in 0..block {
                let byte = data
                    .get((index * block + i) * channels + ch)
                    .copied()
                    .unwrap_or(0);
                file.push(if lsb { byte.reverse_bits() } else { byte });
            }
        }
    }
    file.extend_from_slice(tag);
    file
}

fn chunk(id: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut result = id.to_vec();
    result.extend_from_slice(&(data.len() as u64).to_be_bytes());
    result.extend_from_slice(data);
    if !data.len().is_multiple_of(2) {
        result.push(0);
    }
    result
}

fn dff(
    rate: u32,
    channels: usize,
    raw: &[u8],
    dst: Option<&[Vec<u8>]>,
    with_crc: bool,
    extra: &[u8],
) -> Vec<u8> {
    let mut props = b"SND ".to_vec();
    props.extend(chunk(b"FS  ", &rate.to_be_bytes()));
    let mut channel_ids = (channels as u16).to_be_bytes().to_vec();
    if channels == 2 {
        channel_ids.extend_from_slice(b"SLFTSRGT");
    } else {
        channel_ids.extend_from_slice(b"C   ");
    }
    props.extend(chunk(b"CHNL", &channel_ids));
    props.extend(chunk(
        b"CMPR",
        if dst.is_some() {
            b"DST \x00"
        } else {
            b"DSD \x00"
        },
    ));
    let mut contents = b"DSD ".to_vec();
    contents.extend(chunk(b"FVER", &0x0105_0000_u32.to_be_bytes()));
    contents.extend(chunk(b"PROP", &props));
    contents.extend(chunk(b"UNKN", b"odd"));
    if let Some(frames) = dst {
        let mut info = (frames.len() as u32).to_be_bytes().to_vec();
        info.extend_from_slice(&75_u16.to_be_bytes());
        let mut audio = chunk(b"FRTE", &info);
        let frame_length = rate as usize / 75 / 8 * channels;
        for (index, frame) in frames.iter().enumerate() {
            audio.extend(chunk(b"DSTF", frame));
            if with_crc {
                audio.extend(chunk(
                    b"DSTC",
                    &container::crc(&raw[index * frame_length..(index + 1) * frame_length])
                        .to_be_bytes(),
                ));
            }
        }
        contents.extend(chunk(b"DST ", &audio));
    } else {
        contents.extend(chunk(b"DSD ", raw));
    }
    contents.extend_from_slice(extra);
    chunk(b"FRM8", &contents)
}

fn pattern(bytes: usize, channels: usize) -> Vec<u8> {
    (0..bytes)
        .flat_map(|index| {
            (0..channels).map(move |ch| {
                ((index.wrapping_mul(173 + ch * 30) ^ (index >> (ch + 1))) & 255) as u8
            })
        })
        .collect()
}

fn decode(decoder: &mut Decoder) -> Vec<f64> {
    let mut result = Vec::new();
    let mut buffer = Vec::new();
    while decoder.next(&mut buffer).unwrap() {
        result.extend_from_slice(&buffer);
    }
    assert!(!decoder.next(&mut buffer).unwrap());
    assert!(buffer.is_empty());
    result
}

#[test]
fn containers_bit_orders_and_rates_produce_identical_pcm() {
    for rate in [2_822_400, 5_644_800, 11_289_600] {
        for channels in [1, 2] {
            let samples = rate as u64 / 75 * 2;
            let raw = pattern(samples as usize / 8, channels);
            let frame_length = raw.len() / 2;
            let frames: Vec<_> = raw
                .chunks_exact(frame_length)
                .map(|data| {
                    let mut frame = vec![0];
                    frame.extend_from_slice(data);
                    frame
                })
                .collect();
            let files = [
                dsf(rate, channels, samples, true, &raw, &[]),
                dsf(rate, channels, samples, false, &raw, &[]),
                dff(rate, channels, &raw, None, false, &[]),
                dff(rate, channels, &raw, Some(&frames), true, &[]),
            ];
            let mut reference = None;
            for file in files {
                let fixture = Fixture::new(&file, "bin"); // dispatch uses magic, not extension
                let mut decoder = Decoder::open(&fixture.0).unwrap();
                assert_eq!(decoder.metadata.sample_rate, rate);
                assert_eq!(decoder.metadata.bit_depth, Some(1));
                assert_eq!(decoder.metadata.channels as usize, channels);
                assert_eq!(decoder.pcm_spec().rate, PCM_RATE);
                assert_eq!(decoder.pcm_format(), PcmFormat::Float(64));
                let pcm = decode(&mut decoder);
                assert_eq!(
                    pcm.len(),
                    samples.div_ceil((rate / PCM_RATE) as u64) as usize * channels
                );
                if let Some(reference) = &reference {
                    assert_eq!(&pcm, reference);
                } else {
                    reference = Some(pcm);
                }
            }
        }
    }
}

#[test]
fn compressed_dst_matches_reference_dsd_and_checks_crc() {
    let raw = include_bytes!("../fixtures/dsd/frame_001.dsd");
    // Independently calculated by polynomial long division of DSD bits * x^32.
    assert_eq!(container::crc(b"123456789"), 0xb27c_e117);
    assert_eq!(container::crc(raw), 0x9ee3_d1c6);
    let frame = include_bytes!("../fixtures/dsd/frame_001.dst").to_vec();
    let mut codec = dst_decoder::decoder::DstDecoder::new(2, 2_822_400).unwrap();
    let mut decoded = vec![0; codec.dsd_frame_bytes()];
    codec.decode_frame(&frame, &mut decoded).unwrap();
    assert_eq!(decoded, raw);
    let raw_file = Fixture::new(&dff(2_822_400, 2, raw, None, false, &[]), "dff");
    let mut file = dff(2_822_400, 2, raw, Some(&[frame]), true, &[]);
    let fixture = Fixture::new(&file, "dff");
    assert_eq!(
        decode(&mut Decoder::open(&fixture.0).unwrap()),
        decode(&mut Decoder::open(&raw_file.0).unwrap())
    );
    let crc_position = file.windows(4).position(|bytes| bytes == b"DSTC").unwrap() + 12;
    file[crc_position] ^= 1;
    let corrupt = Fixture::new(&file, "dff");
    let error = Decoder::open(&corrupt.0)
        .unwrap()
        .next(&mut Vec::new())
        .unwrap_err();
    assert!(format!("{error:#}").contains("CRC mismatch"));
}

#[test]
fn seeks_and_cue_segments_match_uninterrupted_decode() {
    for dst in [false, true] {
        let rate = 2_822_400;
        let raw = pattern(rate as usize / 75 / 8 * 5, 2);
        let frames: Vec<_> = raw
            .chunks_exact(raw.len() / 5)
            .map(|data| {
                let mut frame = vec![0];
                frame.extend_from_slice(data);
                frame
            })
            .collect();
        let data = if dst {
            dff(rate, 2, &raw, Some(&frames), false, &[])
        } else {
            dsf(rate, 2, (raw.len() / 2 * 8) as u64, true, &raw, &[])
        };
        let fixture = Fixture::new(&data, "dsf");
        let mut decoder = Decoder::open(&fixture.0).unwrap();
        let reference = decode(&mut decoder);
        for ms in [0, 1, 6, 13, 14, 24, 26, 37, 51, 66, 100, u64::MAX] {
            decoder.seek(ms).unwrap();
            let start = ((u128::from(ms) * u128::from(PCM_RATE)).div_ceil(1000) * 2)
                .min(reference.len() as u128) as usize;
            assert_eq!(
                decode(&mut decoder),
                reference[start..],
                "DST={dst}, seek={ms}"
            );
        }
        decoder
            .set_segment(TrackSegment {
                start_frame: rate as u64 / 75,
                end_frame: Some(rate as u64 / 75 * 3),
            })
            .unwrap();
        assert_eq!(decode(&mut decoder), reference[2352 * 2..2352 * 3 * 2]);
        decoder.seek(1).unwrap();
        assert_eq!(
            decode(&mut decoder),
            reference[(2352 + 177) * 2..2352 * 3 * 2]
        );
        assert!(
            decoder
                .set_segment(TrackSegment {
                    start_frame: 2,
                    end_frame: Some(1)
                })
                .is_err()
        );
    }
}

#[test]
fn partial_bytes_and_short_tracks_ignore_dsf_padding() {
    for samples in [1_u64, 7, 8, 17, 1023, 8193] {
        let mut data = pattern(samples.div_ceil(8) as usize, 2);
        let file = Fixture::new(&dsf(2_822_400, 2, samples, true, &data, &[]), "dsf");
        let reference = decode(&mut Decoder::open(&file.0).unwrap());
        assert_eq!(reference.len(), samples.div_ceil(16) as usize * 2);
        if samples % 8 != 0 {
            let mask = (1_u8 << (8 - samples % 8)) - 1;
            let len = data.len();
            for byte in &mut data[len - 2..] {
                *byte ^= mask;
            }
            let file = Fixture::new(&dsf(2_822_400, 2, samples, false, &data, &[]), "dsf");
            assert_eq!(decode(&mut Decoder::open(&file.0).unwrap()), reference);
        }
    }
}

#[test]
fn irregular_reads_across_channel_blocks_and_dst_frames_match_source_bytes() {
    let rate = 2_822_400;
    let raw = pattern(4704 * 3, 2);
    let frames: Vec<_> = raw
        .as_chunks::<{ 4704 * 2 }>()
        .0
        .iter()
        .map(|data| {
            let mut frame = vec![0];
            frame.extend_from_slice(data);
            frame
        })
        .collect();
    for data in [
        dsf(rate, 2, 4704 * 3 * 8, true, &raw, &[]),
        dff(rate, 2, &raw, None, false, &[]),
        dff(rate, 2, &raw, Some(&frames), false, &[]),
    ] {
        let file = Fixture::new(&data, "bin");
        let mut reader = Reader::open(&file.0).unwrap();
        for start in [0, 1, 4095, 4096, 4703, 4704, 8190, 9407, 9408, 14111] {
            for count in [1, 3, 511, 1025] {
                let count = count.min(14112 - start);
                let mut output = vec![0; count * 2];
                reader.read(start as u64, &mut output).unwrap();
                assert_eq!(output, raw[start * 2..(start + count) * 2]);
            }
        }
        assert!(reader.read(14112, &mut [0; 2]).is_err());
    }
}

#[test]
fn failed_reads_never_leave_a_partially_valid_cache() {
    let raw = pattern(5000, 2);
    for data in [
        dsf(2_822_400, 2, 40_000, true, &raw, &[]),
        dff(2_822_400, 2, &raw, None, false, &[]),
    ] {
        let file = Fixture::new(&data, "bin");
        let mut reader = Reader::open(&file.0).unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&file.0)
            .unwrap()
            .set_len(100)
            .unwrap();
        for _ in 0..2 {
            assert!(reader.read(0, &mut [0; 16]).is_err());
        }
    }
}

#[test]
fn tone_amplitude_timing_and_channel_isolation_survive_conversion() {
    use std::f64::consts::TAU;
    for rate in [2_822_400, 5_644_800, 11_289_600] {
        let samples = rate / 50;
        let mut raw = vec![0; samples as usize / 8 * 2];
        // Second-order sigma-delta test encoder; its noise is pushed above
        // the audio band rather than dominating the decoded tone measurement.
        let mut integrator = 0.0;
        let mut error = 0.0;
        let mut feedback = 0.0;
        for index in 0..samples as usize {
            let input = 0.3 * (TAU * 1000.0 * index as f64 / f64::from(rate)).sin();
            integrator += input - feedback;
            error += integrator - feedback;
            let bit = error >= 0.0;
            feedback = if bit { 1.0 } else { -1.0 };
            if bit {
                raw[index / 8 * 2] |= 0x80 >> (index % 8);
            }
        }
        for row in raw.as_chunks_mut::<2>().0 {
            row[1] = 0x55;
        }
        let file = Fixture::new(&dsf(rate, 2, u64::from(samples), true, &raw, &[]), "dsf");
        let output = decode(&mut Decoder::open(&file.0).unwrap());
        let mut squared_error = 0.0;
        let mut count = 0;
        for (index, row) in output
            .as_chunks::<2>()
            .0
            .iter()
            .enumerate()
            .skip(100)
            .take(output.len() / 2 - 200)
        {
            let expected = 0.3 * (TAU * 1000.0 * index as f64 / f64::from(PCM_RATE)).sin();
            squared_error += (row[0] - expected).powi(2);
            count += 1;
            assert!(row[1].abs() < 1e-7, "Channel leakage: {}", row[1]);
        }
        let rms = (squared_error / f64::from(count)).sqrt();
        assert!(rms < 0.001, "DSD rate {rate}: tone error {rms}");
    }
}

#[test]
fn malformed_containers_fail_without_panicking() {
    let valid = dsf(2_822_400, 2, 8192, true, &pattern(1024, 2), &[]);
    for length in [0, 3, 4, 12, 27, 28, 51, 79, 91, valid.len() - 1] {
        let file = Fixture::new(&valid[..length], "dsf");
        assert!(Decoder::open(&file.0).is_err());
    }
    for (offset, value) in [
        (4, u64::MAX),
        (12, 7),
        (20, 1),
        (32, u64::MAX),
        (64, u64::MAX),
        (84, u64::MAX),
    ] {
        let mut bad = valid.clone();
        bad[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
        let file = Fixture::new(&bad, "dsf");
        assert!(Decoder::open(&file.0).is_err(), "offset {offset}");
    }
    for (offset, value) in [
        (40, 2_u32),
        (44, 1),
        (48, 3),
        (52, 6),
        (56, 22_579_200),
        (60, 2),
        (72, u32::MAX),
    ] {
        let mut bad = valid.clone();
        bad[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        let file = Fixture::new(&bad, "dsf");
        assert!(Decoder::open(&file.0).is_err(), "offset {offset}");
    }
    let raw = pattern(4704, 2);
    let mut frame = vec![0];
    frame.extend_from_slice(&raw);
    let valid = dff(2_822_400, 2, &raw, Some(&[frame]), false, &[]);
    for marker in [
        b"FRM8", b"PROP", b"FS  ", b"CHNL", b"CMPR", b"DST ", b"FRTE", b"DSTF",
    ] {
        let mut bad = valid.clone();
        let offset = bad.windows(4).position(|value| value == marker).unwrap();
        bad[offset + 4..offset + 12].copy_from_slice(&u64::MAX.to_be_bytes());
        let file = Fixture::new(&bad, "dff");
        assert!(Decoder::open(&file.0).is_err(), "{marker:?}");
    }
    let mut bad = valid.clone();
    let offset = bad.windows(4).position(|value| value == b"DSTF").unwrap();
    bad[offset + 12] = 0x3f; // invalid stuffing bits in raw DST frame
    let file = Fixture::new(&bad, "dff");
    assert!(
        Decoder::open(&file.0)
            .unwrap()
            .next(&mut Vec::new())
            .is_err()
    );
}

fn id3_tag() -> Vec<u8> {
    let mut frames = Vec::new();
    for (id, text) in [
        (b"TIT2", "DSD title"),
        (b"TPE1", "Artist"),
        (b"TALB", "Album"),
        (b"TYER", "2025"),
        (b"TRCK", "3/8"),
    ] {
        let mut data = vec![0];
        data.extend_from_slice(text.as_bytes());
        frames.extend_from_slice(id);
        frames.extend_from_slice(&(data.len() as u32).to_be_bytes());
        frames.extend_from_slice(&[0, 0]);
        frames.extend(data);
    }
    let mut picture = b"\0image/png\0\x03\0".to_vec();
    picture.extend_from_slice(b"\x89PNG\r\n\x1a\nfixture");
    frames.extend_from_slice(b"APIC");
    frames.extend_from_slice(&(picture.len() as u32).to_be_bytes());
    frames.extend_from_slice(&[0, 0]);
    frames.extend(picture);
    let len = frames.len();
    let mut tag = b"ID3\x03\0\0".to_vec();
    tag.extend_from_slice(&[
        ((len >> 21) & 127) as u8,
        ((len >> 14) & 127) as u8,
        ((len >> 7) & 127) as u8,
        (len & 127) as u8,
    ]);
    tag.extend(frames);
    tag
}

#[test]
fn metadata_artwork_and_invalid_tags_do_not_change_audio() {
    use mplayer_core::MetadataProbe;
    let raw = pattern(4704, 2);
    let tag = id3_tag();
    let files = [
        dsf(2_822_400, 2, 4704 * 8, true, &raw, &tag),
        dff(2_822_400, 2, &raw, None, false, &chunk(b"ID3 ", &tag)),
    ];
    for data in files {
        let file = Fixture::new(&data, "dsf");
        let probe = crate::SymphoniaProbe;
        let metadata = probe.probe(&file.0).unwrap();
        assert_eq!(
            (
                metadata.title.as_str(),
                metadata.artist.as_str(),
                metadata.album.as_str(),
                metadata.year,
                metadata.track_number
            ),
            ("DSD title", "Artist", "Album", Some(2025), Some(3))
        );
        let artwork = probe.artwork(&file.0).unwrap().unwrap();
        assert_eq!(&*artwork.data, b"\x89PNG\r\n\x1a\nfixture");
    }
    for tag in [
        &[][..],
        b"bad".as_slice(),
        b"ID3\x03\0\0\x7f\x7f\x7f\x7f".as_slice(),
    ] {
        let file = Fixture::new(&dsf(2_822_400, 2, 4704 * 8, true, &raw, tag), "dsf");
        let mut decoder = Decoder::open(&file.0).unwrap();
        assert_eq!(decode(&mut decoder).len(), 2352 * 2);
        assert!(crate::SymphoniaProbe.artwork(&file.0).unwrap().is_none());
    }
    let mut info = Vec::new();
    for (id, text) in [
        (b"DITI", b"Title".as_slice()),
        (b"DIAR", b"Artist".as_slice()),
    ] {
        let mut value = (text.len() as u32).to_be_bytes().to_vec();
        value.extend_from_slice(text);
        info.extend(chunk(id, &value));
    }
    let file = Fixture::new(
        &dff(2_822_400, 2, &raw, None, false, &chunk(b"DIIN", &info)),
        "dff",
    );
    let metadata = crate::SymphoniaProbe.probe(&file.0).unwrap();
    assert_eq!(
        (metadata.title.as_str(), metadata.artist.as_str()),
        ("Title", "Artist")
    );
}

#[test]
#[ignore = "manual real-file PCM correctness/export; set MPLAYER_DSD_CHECK_FILE and MPLAYER_DSD_CHECK_DIR"]
fn dsd_file_correctness_windows() {
    use std::io::Write;

    let Some(file) = std::env::var_os("MPLAYER_DSD_CHECK_FILE") else {
        eprintln!(
            "Set MPLAYER_DSD_CHECK_FILE and MPLAYER_DSD_CHECK_DIR for offline PCM comparison"
        );
        return;
    };
    let directory = PathBuf::from(
        std::env::var_os("MPLAYER_DSD_CHECK_DIR").expect("set MPLAYER_DSD_CHECK_DIR"),
    );
    assert!(
        directory.is_dir(),
        "Create the diagnostic output directory first"
    );
    let starts: Vec<u64> = std::env::var("MPLAYER_DSD_CHECK_STARTS")
        .unwrap_or_else(|_| "105,116".into())
        .split(',')
        .map(|value| value.parse().expect("invalid window start"))
        .collect();
    let seconds: u64 = std::env::var("MPLAYER_DSD_CHECK_SECONDS")
        .map_or(10, |value| value.parse().expect("invalid window length"));
    assert!(!starts.is_empty() && seconds > 0 && seconds <= 60);
    let frames = (seconds * u64::from(PCM_RATE)) as usize;
    let mut decoder = DsdDecoder::open(Path::new(&file)).unwrap();
    let channels = decoder.reader.channels;
    let end = (starts.iter().max().unwrap() + seconds) * u64::from(PCM_RATE);
    assert!(end <= decoder.end, "Requested windows extend beyond EOF");
    let mut windows: Vec<Vec<f64>> = starts.iter().map(|_| Vec::new()).collect();
    let mut output = Vec::new();
    let mut position = 0;
    while position < end {
        assert!(decoder.next(&mut output).unwrap());
        assert!(output.iter().all(|sample| sample.is_finite()));
        let next = position + (output.len() / channels) as u64;
        for (&start, window) in starts.iter().zip(&mut windows) {
            let start = start * u64::from(PCM_RATE);
            let first = position.max(start);
            let last = next.min(start + frames as u64);
            if first < last {
                window.extend_from_slice(
                    &output[(first - position) as usize * channels
                        ..(last - position) as usize * channels],
                );
            }
        }
        position = next;
    }
    for (&start, continuous) in starts.iter().zip(windows) {
        assert_eq!(continuous.len(), frames * channels);
        for sequential in [false, true] {
            let mut decoder = DsdDecoder::open(Path::new(&file)).unwrap();
            decoder.seek(start * 1000).unwrap();
            let mut checked = 0;
            while checked < continuous.len() {
                let more = if sequential {
                    decoder.next_with(&mut output, Filter::process_sequential)
                } else {
                    decoder.next(&mut output)
                };
                assert!(more.unwrap());
                let count = output.len().min(continuous.len() - checked);
                if let Some(index) = output[..count]
                    .iter()
                    .zip(&continuous[checked..checked + count])
                    .position(|(actual, expected)| actual.to_bits() != expected.to_bits())
                {
                    panic!(
                        "PCM differs: start={start}s sequential={sequential} frame={} channel={} actual={} expected={}",
                        (checked + index) / channels,
                        (checked + index) % channels,
                        output[index],
                        continuous[checked + index]
                    );
                }
                checked += count;
            }
            eprintln!(
                "DSD_CORRECTNESS start={start}s seconds={seconds} channels={channels} sequential={sequential} matching_samples={checked}"
            );
        }
        let destination = directory.join(format!("pcm-{start}.f64le"));
        let mut writer = std::io::BufWriter::new(std::fs::File::create(&destination).unwrap());
        for samples in continuous.chunks(4096 * channels) {
            let bytes: Vec<_> = samples
                .iter()
                .flat_map(|sample| sample.to_le_bytes())
                .collect();
            writer.write_all(&bytes).unwrap();
        }
        writer.flush().unwrap();
        eprintln!(
            "DSD_PCM_EXPORT rate={PCM_RATE} channels={channels} file={}",
            destination.display()
        );
    }
}

#[test]
#[ignore = "manual real-file DSD decode/resample timing; set MPLAYER_DSD_BENCH_FILE"]
fn dsd_file_throughput() {
    use crate::dsp::Converter;
    use std::time::Instant;

    let Some(file) = std::env::var_os("MPLAYER_DSD_BENCH_FILE") else {
        eprintln!("Skipping real-file benchmark: set MPLAYER_DSD_BENCH_FILE to a DSF/DFF path");
        return;
    };
    let rate: u32 = std::env::var("MPLAYER_DSD_BENCH_RATE")
        .map_or(PCM_RATE, |rate| rate.parse().expect("invalid output rate"));
    for trial in 0..4 {
        let mut decoder = DsdDecoder::open(Path::new(&file)).unwrap();
        let channels = decoder.reader.channels;
        let mut converter = Converter::new(PCM_RATE, rate, channels, channels).unwrap();
        let mut decoded = Vec::new();
        let mut output = Vec::new();
        let mut blocks = Vec::new();
        let mut late = 0;
        let mut source_frames = 0;
        let mut output_frames = 0;
        let started = Instant::now();
        loop {
            let block_started = Instant::now();
            if !decoder.next(&mut decoded).unwrap() {
                break;
            }
            source_frames += decoded.len() / channels;
            // Match Session::fill: a decoder can return a larger block, while
            // the device converter accepts up to 1024 input frames per call.
            for samples in decoded.chunks(1024 * channels) {
                converter.convert(samples, false, &mut output).unwrap();
                output_frames += output.len() / channels;
                std::hint::black_box(&output);
            }
            let elapsed = block_started.elapsed().as_secs_f64();
            let budget = decoded.len() as f64 / channels as f64 / f64::from(PCM_RATE);
            if !blocks.is_empty() && elapsed > budget {
                late += 1;
            }
            blocks.push(elapsed * 1000.0);
        }
        converter.convert(&[], true, &mut output).unwrap();
        output_frames += output.len() / channels;
        std::hint::black_box(&output);
        let elapsed = started.elapsed().as_secs_f64();
        assert_eq!(output_frames as u64, converter.target_frames().unwrap());
        eprintln!(
            "DSD_MEMORY scratch_bytes={} window_bytes={} pcm_bytes={} kernel_bytes={}",
            decoder.scratch.bytes(),
            decoder.window.capacity(),
            decoded.capacity() * std::mem::size_of::<f64>(),
            decoder.filter.as_ref().unwrap().table_bytes()
        );
        let first = blocks[0];
        blocks.sort_by(f64::total_cmp);
        let seconds = source_frames as f64 / f64::from(PCM_RATE);
        eprintln!(
            "MPLAYER_FILE trial={trial} rate={rate} frames={output_frames} seconds={seconds:.6} wall_ms={:.3} realtime={:.3} first_ms={first:.3} p50_ms={:.3} p95_ms={:.3} max_ms={:.3} late_steady_blocks={late}/{}",
            elapsed * 1000.0,
            seconds / elapsed,
            blocks[blocks.len() / 2],
            blocks[blocks.len() * 95 / 100],
            blocks.last().unwrap(),
            blocks.len() - 1
        );
    }
}

#[test]
#[ignore = "manual release-mode DSD/DST kernel, dispatch and buffer measurement"]
fn dsd_throughput() {
    use std::time::{Duration, Instant};

    type Process = fn(&Filter, &[u8], usize, &mut [f64], &mut Scratch);
    fn low(
        filter: &Filter,
        bytes: &[u8],
        channels: usize,
        output: &mut [f64],
        scratch: &mut Scratch,
    ) {
        filter.process_at_threshold(bytes, channels, output, scratch, 1 << 18);
    }
    fn high(
        filter: &Filter,
        bytes: &[u8],
        channels: usize,
        output: &mut [f64],
        scratch: &mut Scratch,
    ) {
        filter.process_at_threshold(bytes, channels, output, scratch, 1 << 19);
    }
    fn median(values: &[[f64; 2]], column: usize) -> f64 {
        let mut sorted: Vec<_> = values.iter().map(|row| row[column]).collect();
        sorted.sort_by(f64::total_cmp);
        (sorted[2] + sorted[3]) / 2.0
    }

    let mut fixtures = Vec::new();
    for rate in [2_822_400, 5_644_800, 11_289_600] {
        for channels in [1, 2] {
            let raw = pattern(rate as usize / 8, channels);
            fixtures.push((
                format!("DSD{}-{}ch", rate / 44_100, channels),
                Fixture::new(
                    &dsf(rate, channels, u64::from(rate), true, &raw, &[]),
                    "dsf",
                ),
            ));
        }
    }
    let frame = include_bytes!("../fixtures/dsd/frame_001.dst").to_vec();
    let reference = include_bytes!("../fixtures/dsd/frame_001.dsd").repeat(75);
    fixtures.push((
        "DST64-2ch".into(),
        Fixture::new(
            &dff(2_822_400, 2, &reference, Some(&vec![frame; 75]), false, &[]),
            "dff",
        ),
    ));
    let modes: [(&str, Process); 3] = [
        ("sequential", Filter::process_sequential),
        ("parallel18", low),
        ("parallel19", high),
    ];
    let workers = filter::pool().map_or(0, rayon::ThreadPool::current_num_threads);
    eprintln!(
        "DSD_POOL workers={workers} requested_stack_bytes={}",
        workers * filter::WORKER_STACK_BYTES
    );
    if let Some(pool) = filter::pool() {
        pool.install(|| {});
    }
    let mut summaries = Vec::new();
    for (name, file) in fixtures {
        let decoder = DsdDecoder::open(&file.0).unwrap();
        let channels = decoder.reader.channels;
        let filter = Filter::get(decoder.reader.rate);
        let mut measurements: [Vec<[f64; 2]>; 3] = std::array::from_fn(|_| Vec::new());
        for trial in 0..7 {
            let order = if trial % 2 == 0 { [0, 1, 2] } else { [2, 1, 0] };
            for mode in order {
                let mut decoder = DsdDecoder::open(&file.0).unwrap();
                let mut frames = 0;
                let mut buffer = Vec::new();
                let mut kernel = Duration::ZERO;
                let started = Instant::now();
                while decoder
                    .next_with(&mut buffer, |filter, bytes, channels, output, scratch| {
                        let started = Instant::now();
                        modes[mode].1(filter, bytes, channels, output, scratch);
                        kernel += started.elapsed();
                    })
                    .unwrap()
                {
                    frames += buffer.len() / channels;
                    std::hint::black_box(&buffer);
                }
                let elapsed = started.elapsed().as_secs_f64();
                assert_eq!(frames, PCM_RATE as usize);
                if trial > 0 {
                    measurements[mode].push([elapsed, kernel.as_secs_f64()]);
                }
            }
        }
        let wall: [f64; 3] = std::array::from_fn(|mode| median(&measurements[mode], 0));
        for (mode, &(label, _)) in modes.iter().enumerate() {
            eprintln!(
                "DSD_BENCH {name} mode={label} realtime={:.3} wall_ms={:.3} kernel_ms={:.3}",
                1.0 / wall[mode],
                wall[mode] * 1000.0,
                median(&measurements[mode], 1) * 1000.0
            );
        }
        eprintln!("DSD_TABLE {name} bytes={}", filter.table_bytes());
        summaries.push((name, wall));
    }
    for mode in [1, 2] {
        let qualifies = summaries.iter().all(|(name, wall)| {
            if name == "DSD256-2ch" {
                wall[mode] <= wall[0] / 1.10
            } else {
                wall[mode] <= wall[0] * 1.05
            }
        });
        eprintln!("DSD_THRESHOLD {} qualifies={qualifies}", modes[mode].0);
    }
}
