use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

use anyhow::{Context, bail, ensure};
use dst_decoder::decoder::DstDecoder;
use mplayer_core::{Result, TrackMetadata};

const MAX_DST_FRAMES: usize = 1_000_000;
const MAX_DST_FRAME_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Copy)]
struct Chunk {
    id: [u8; 4],
    data: u64,
    end: u64,
    next: u64,
}

fn bytes<const N: usize>(file: &mut File) -> Result<[u8; N]> {
    let mut bytes = [0; N];
    file.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn chunk(file: &mut File, position: u64, parent: u64, dsf: bool) -> Result<Chunk> {
    ensure!(
        position.checked_add(12).is_some_and(|end| end <= parent),
        "Truncated DSD chunk header"
    );
    file.seek(SeekFrom::Start(position))?;
    let id = bytes(file)?;
    let length = if dsf {
        u64::from_le_bytes(bytes(file)?)
    } else {
        u64::from_be_bytes(bytes(file)?)
    };
    let data = position + 12;
    let end = if dsf {
        position.checked_add(length).filter(|_| length >= 12)
    } else {
        data.checked_add(length)
    }
    .filter(|&end| end <= parent)
    .context("DSD chunk extends beyond its container")?;
    let next = end
        .checked_add(if dsf { 0 } else { length & 1 })
        .filter(|&end| end <= parent)
        .context("Missing DSDIFF chunk alignment byte")?;
    Ok(Chunk {
        id,
        data,
        end,
        next,
    })
}

fn payload(file: &mut File, chunk: Chunk, maximum: u64) -> Result<Vec<u8>> {
    let length = chunk.end - chunk.data;
    ensure!(length <= maximum, "DSD metadata exceeds size limit");
    file.seek(SeekFrom::Start(chunk.data))?;
    let mut data = vec![0; length as usize];
    file.read_exact(&mut data)?;
    Ok(data)
}

#[derive(Clone, Copy)]
struct Frame {
    offset: u64,
    length: u32,
    crc: Option<u32>,
}

enum Layout {
    Dsf {
        offset: u64,
        block: usize,
        lsb_first: bool,
    },
    Raw {
        offset: u64,
    },
    Dst {
        frames: Vec<Frame>,
        decoder: Box<DstDecoder>,
        encoded: Vec<u8>,
        frame_bytes: usize,
    },
}

pub(super) struct Reader {
    file: File,
    layout: Layout,
    pub rate: u32,
    pub channels: usize,
    pub samples: u64,
    pub metadata: TrackMetadata,
    pub tag: Option<(u64, u64)>,
    cache: Vec<u8>,
    cache_start: u64,
    cache_frames: usize,
    scratch: Vec<u8>,
}

fn validate(rate: u32, channels: usize) -> Result<()> {
    ensure!(
        [2_822_400, 5_644_800, 11_289_600].contains(&rate),
        "Unsupported DSD rate {rate}; expected DSD64, DSD128 or DSD256"
    );
    ensure!(
        (1..=2).contains(&channels),
        "Only mono and stereo DSD is supported"
    );
    Ok(())
}

impl Reader {
    pub fn open(path: &Path) -> Result<Self> {
        let mut file = File::open(path)?;
        let length = file.metadata()?.len();
        let signature = bytes::<4>(&mut file)?;
        let mut reader = match &signature {
            b"DSD " => Self::dsf(file, length)?,
            b"FRM8" => Self::dff(file, length)?,
            _ => bail!("Unrecognized DSD container"),
        };
        reader.metadata.sample_rate = reader.rate;
        reader.metadata.channels = reader.channels as u16;
        reader.metadata.bit_depth = Some(1);
        reader.metadata.codec = "dsd".into();
        reader.metadata.duration_ms =
            (u128::from(reader.samples) * 1000 / u128::from(reader.rate)).try_into()?;
        if reader.metadata.title.is_empty() {
            reader.metadata.title = path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
        }
        Ok(reader)
    }

    fn new(
        file: File,
        layout: Layout,
        rate: u32,
        channels: usize,
        samples: u64,
        tag: Option<(u64, u64)>,
        metadata: TrackMetadata,
    ) -> Result<Self> {
        validate(rate, channels)?;
        ensure!(samples > 0, "DSD file contains no audio samples");
        Ok(Self {
            file,
            layout,
            rate,
            channels,
            samples,
            tag,
            metadata,
            cache: Vec::new(),
            cache_start: 0,
            cache_frames: 0,
            scratch: Vec::new(),
        })
    }

    fn dsf(mut file: File, length: u64) -> Result<Self> {
        let header = chunk(&mut file, 0, length, true)?;
        ensure!(header.end == 28, "Invalid DSF DSD header size");
        file.seek(SeekFrom::Start(12))?;
        ensure!(
            u64::from_le_bytes(bytes(&mut file)?) == length,
            "DSF file size does not match header"
        );
        let tag = u64::from_le_bytes(bytes(&mut file)?);
        let fmt = chunk(&mut file, header.next, length, true)?;
        ensure!(
            fmt.id == *b"fmt " && fmt.end - fmt.data == 40,
            "Invalid DSF fmt chunk"
        );
        file.seek(SeekFrom::Start(fmt.data))?;
        ensure!(
            u32::from_le_bytes(bytes(&mut file)?) == 1,
            "Unsupported DSF version"
        );
        ensure!(
            u32::from_le_bytes(bytes(&mut file)?) == 0,
            "Unsupported DSF encoding"
        );
        let channel_type = u32::from_le_bytes(bytes(&mut file)?);
        let channels = u32::from_le_bytes(bytes(&mut file)?) as usize;
        let rate = u32::from_le_bytes(bytes(&mut file)?);
        validate(rate, channels)?;
        ensure!(
            channel_type == channels as u32,
            "Unsupported DSF channel layout"
        );
        let bits = u32::from_le_bytes(bytes(&mut file)?);
        ensure!(bits == 1 || bits == 8, "Invalid DSF bit order");
        let samples = u64::from_le_bytes(bytes(&mut file)?);
        let block = u32::from_le_bytes(bytes(&mut file)?) as usize;
        ensure!(block == 4096, "Unsupported DSF channel block size {block}");
        ensure!(
            u32::from_le_bytes(bytes(&mut file)?) == 0,
            "Invalid DSF reserved field"
        );
        let data = chunk(&mut file, fmt.next, length, true)?;
        ensure!(data.id == *b"data", "Missing DSF data chunk");
        let required = samples
            .div_ceil(8)
            .div_ceil(block as u64)
            .checked_mul((block * channels) as u64)
            .context("DSF sample count overflow")?;
        ensure!(
            data.end - data.data == required,
            "DSF data length does not match sample count"
        );
        let tag = if tag == 0 {
            None
        } else {
            ensure!(
                tag >= data.end && tag < length,
                "Invalid DSF metadata offset"
            );
            Some((tag, length - tag))
        };
        Self::new(
            file,
            Layout::Dsf {
                offset: data.data,
                block,
                lsb_first: bits == 1,
            },
            rate,
            channels,
            samples,
            tag,
            TrackMetadata::default(),
        )
    }

    fn dff(mut file: File, length: u64) -> Result<Self> {
        let root = chunk(&mut file, 0, length, false)?;
        ensure!(
            root.end == length && root.end >= 16,
            "Invalid DSDIFF FORM length"
        );
        ensure!(
            bytes::<4>(&mut file)? == *b"DSD ",
            "Invalid DSDIFF FORM type"
        );
        let mut position = 16;
        let mut rate = 0;
        let mut channels = 0;
        let mut compression = None;
        let mut version = false;
        let mut properties = false;
        let mut audio = None;
        let mut tag = None;
        let mut metadata = TrackMetadata::default();
        while position < root.end {
            let c = chunk(&mut file, position, root.end, false)?;
            match &c.id {
                b"FVER" => {
                    ensure!(
                        !version && position == 16 && c.end - c.data == 4,
                        "Invalid DSDIFF FVER chunk"
                    );
                    let value = u32::from_be_bytes(bytes(&mut file)?);
                    ensure!(value >> 24 == 1, "Unsupported DSDIFF version");
                    version = true;
                }
                b"PROP" => {
                    ensure!(
                        version && !properties && audio.is_none() && c.end - c.data >= 4,
                        "Invalid DSDIFF properties"
                    );
                    ensure!(
                        bytes::<4>(&mut file)? == *b"SND ",
                        "Invalid DSDIFF property type"
                    );
                    let mut sub = c.data + 4;
                    while sub < c.end {
                        let property = chunk(&mut file, sub, c.end, false)?;
                        match &property.id {
                            b"FS  " => {
                                ensure!(
                                    rate == 0 && property.end - property.data == 4,
                                    "Invalid DSDIFF sample rate chunk"
                                );
                                rate = u32::from_be_bytes(bytes(&mut file)?);
                            }
                            b"CHNL" => {
                                ensure!(
                                    channels == 0 && property.end - property.data >= 2,
                                    "Invalid DSDIFF channel chunk"
                                );
                                channels = usize::from(u16::from_be_bytes(bytes(&mut file)?));
                                ensure!(
                                    (1..=2).contains(&channels),
                                    "Only mono and stereo DSD is supported"
                                );
                                ensure!(
                                    property.end - property.data == 2 + channels as u64 * 4,
                                    "Invalid DSDIFF channel identifiers"
                                );
                                if channels == 2 {
                                    let left = bytes::<4>(&mut file)?;
                                    let right = bytes::<4>(&mut file)?;
                                    ensure!(
                                        (left == *b"SLFT" && right == *b"SRGT")
                                            || (left == *b"MLFT" && right == *b"MRGT"),
                                        "Unsupported DSDIFF channel order"
                                    );
                                }
                            }
                            b"CMPR" => {
                                ensure!(
                                    compression.is_none() && property.end - property.data >= 5,
                                    "Invalid DSDIFF compression chunk"
                                );
                                compression = Some(bytes::<4>(&mut file)?);
                                let name_length = bytes::<1>(&mut file)?[0];
                                ensure!(
                                    u64::from(name_length) + 5 <= property.end - property.data,
                                    "Truncated DSDIFF compression name"
                                );
                            }
                            _ => (),
                        }
                        sub = property.next;
                    }
                    validate(rate, channels)?;
                    properties = true;
                }
                b"DSD " | b"DST " => {
                    ensure!(
                        properties && audio.is_none() && compression == Some(c.id),
                        "Missing or inconsistent DSDIFF audio properties"
                    );
                    audio = Some(c);
                }
                b"ID3 " => {
                    ensure!(tag.is_none(), "Duplicate DSDIFF ID3 chunk");
                    tag = Some((c.data, c.end - c.data));
                }
                b"DIIN" => {
                    let mut sub = c.data;
                    while sub < c.end {
                        let field = chunk(&mut file, sub, c.end, false)?;
                        if field.id == *b"DITI" || field.id == *b"DIAR" {
                            let value = payload(&mut file, field, 64 * 1024)?;
                            ensure!(value.len() >= 4, "Truncated DSDIFF text field");
                            let count = u32::from_be_bytes(value[..4].try_into()?) as usize;
                            ensure!(count <= value.len() - 4, "Invalid DSDIFF text length");
                            let text = String::from_utf8_lossy(&value[4..4 + count])
                                .trim_end_matches('\0')
                                .to_owned();
                            if field.id == *b"DITI" {
                                metadata.title = text;
                            } else {
                                metadata.artist = text;
                            }
                        }
                        sub = field.next;
                    }
                }
                _ => (),
            }
            position = c.next;
        }
        let audio = audio.context("Missing DSDIFF audio data")?;
        let (layout, samples) = if audio.id == *b"DSD " {
            let size = audio.end - audio.data;
            ensure!(
                size.is_multiple_of(channels as u64),
                "Incomplete DSDIFF channel frame"
            );
            (
                Layout::Raw { offset: audio.data },
                (size / channels as u64)
                    .checked_mul(8)
                    .context("DSDIFF sample count overflow")?,
            )
        } else {
            let (frames, samples) = dst_frames(&mut file, audio, rate)?;
            let decoder = Box::new(DstDecoder::new(channels, rate as usize)?);
            let frame_bytes = decoder.dsd_frame_bytes() / channels;
            (
                Layout::Dst {
                    frames,
                    decoder,
                    encoded: Vec::new(),
                    frame_bytes,
                },
                samples,
            )
        };
        Self::new(file, layout, rate, channels, samples, tag, metadata)
    }

    /// Returns MSB-first channel-interleaved bytes, never container padding.
    pub fn read(&mut self, mut start: u64, destination: &mut [u8]) -> Result<()> {
        ensure!(
            destination.len().is_multiple_of(self.channels),
            "Incomplete DSD read frame"
        );
        ensure!(
            start
                .checked_add((destination.len() / self.channels) as u64)
                .is_some_and(|end| end <= self.samples.div_ceil(8)),
            "DSD read exceeds audio bounds"
        );
        let mut written = 0;
        while written < destination.len() {
            if start < self.cache_start || start >= self.cache_start + self.cache_frames as u64 {
                self.fill_cache(start)?;
            }
            let offset = (start - self.cache_start) as usize * self.channels;
            let count =
                (destination.len() - written).min(self.cache_frames * self.channels - offset);
            destination[written..written + count]
                .copy_from_slice(&self.cache[offset..offset + count]);
            written += count;
            start += (count / self.channels) as u64;
        }
        Ok(())
    }

    fn fill_cache(&mut self, start: u64) -> Result<()> {
        let result = self.load_cache(start);
        if result.is_err() {
            // A failed read may have overwritten only part of the cache.
            self.cache_frames = 0;
        }
        result
    }

    fn load_cache(&mut self, start: u64) -> Result<()> {
        match &mut self.layout {
            Layout::Dsf {
                offset,
                block,
                lsb_first,
            } => {
                let index = start / *block as u64;
                self.cache_start = index * *block as u64;
                self.cache_frames =
                    (self.samples.div_ceil(8) - self.cache_start).min(*block as u64) as usize;
                self.scratch.resize(*block * self.channels, 0);
                self.file.seek(SeekFrom::Start(
                    *offset + index * (*block * self.channels) as u64,
                ))?;
                self.file.read_exact(&mut self.scratch)?;
                self.cache.resize(self.cache_frames * self.channels, 0);
                for (i, row) in self.cache.chunks_exact_mut(self.channels).enumerate() {
                    for (ch, byte) in row.iter_mut().enumerate() {
                        let value = self.scratch[ch * *block + i];
                        *byte = if *lsb_first {
                            value.reverse_bits()
                        } else {
                            value
                        };
                    }
                }
            }
            Layout::Raw { offset } => {
                self.cache_start = start / 4096 * 4096;
                self.cache_frames = (self.samples / 8 - self.cache_start).min(4096) as usize;
                self.cache.resize(self.cache_frames * self.channels, 0);
                self.file.seek(SeekFrom::Start(
                    *offset + self.cache_start * self.channels as u64,
                ))?;
                self.file.read_exact(&mut self.cache)?;
            }
            Layout::Dst {
                frames,
                decoder,
                encoded,
                frame_bytes,
            } => {
                let index = (start / *frame_bytes as u64) as usize;
                let frame = frames.get(index).context("Missing DST frame")?;
                encoded.resize(frame.length as usize, 0);
                self.file.seek(SeekFrom::Start(frame.offset))?;
                self.file.read_exact(encoded)?;
                self.cache.resize(*frame_bytes * self.channels, 0);
                decoder
                    .decode_frame(encoded, &mut self.cache)
                    .with_context(|| format!("Invalid DST frame {index}"))?;
                if let Some(expected) = frame.crc {
                    ensure!(
                        crc(&self.cache) == expected,
                        "DST frame {index} CRC mismatch"
                    );
                }
                self.cache_start = index as u64 * *frame_bytes as u64;
                self.cache_frames = *frame_bytes;
            }
        }
        Ok(())
    }
}

fn dst_frames(file: &mut File, audio: Chunk, rate: u32) -> Result<(Vec<Frame>, u64)> {
    let info = chunk(file, audio.data, audio.end, false)?;
    ensure!(
        info.id == *b"FRTE" && info.end - info.data == 6,
        "Missing or invalid DST frame information"
    );
    let count = u32::from_be_bytes(bytes(file)?) as usize;
    ensure!(
        (1..=MAX_DST_FRAMES).contains(&count),
        "DST frame count exceeds index limit"
    );
    ensure!(
        u16::from_be_bytes(bytes(file)?) == 75,
        "Unsupported DST frame rate"
    );
    let mut frames: Vec<Frame> = Vec::new();
    let mut position = info.next;
    let mut crc_count = 0;
    while position < audio.end {
        let c = chunk(file, position, audio.end, false)?;
        match &c.id {
            b"DSTF" => {
                ensure!(
                    frames.len() < count && (1..=MAX_DST_FRAME_BYTES).contains(&(c.end - c.data)),
                    "Invalid DST frame length or count"
                );
                frames.push(Frame {
                    offset: c.data,
                    length: (c.end - c.data) as u32,
                    crc: None,
                });
            }
            b"DSTC" => {
                ensure!(c.end - c.data == 4, "Invalid DST CRC length");
                let frame = frames.last_mut().context("DST CRC precedes frame data")?;
                ensure!(frame.crc.is_none(), "Duplicate DST frame CRC");
                frame.crc = Some(u32::from_be_bytes(bytes(file)?));
                crc_count += 1;
            }
            b"FRTE" => bail!("Duplicate DST frame information"),
            _ => (),
        }
        position = c.next;
    }
    ensure!(
        frames.len() == count && (crc_count == 0 || crc_count == count),
        "DST frame count or CRC count mismatch"
    );
    Ok((frames, count as u64 * u64::from(rate / 75)))
}

// DSDIFF polynomial x^32 + x^31 + x^4 + 1, MSB first, initial remainder zero.
pub(super) fn crc(data: &[u8]) -> u32 {
    let mut crc = 0_u32;
    for &byte in data {
        crc ^= u32::from(byte) << 24;
        for _ in 0..8 {
            crc = (crc << 1)
                ^ if crc & 0x8000_0000 != 0 {
                    0x8000_0011
                } else {
                    0
                };
        }
    }
    crc
}
