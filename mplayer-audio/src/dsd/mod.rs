//! Streaming DSF/DSDIFF playback. Source metadata stays DSD; returned samples
//! are normalized f64 PCM at PCM_RATE, ready for the ordinary PCM converter.
mod container;
mod filter;

use std::{
    fs::File,
    io::{Cursor, Read, Seek, SeekFrom},
    path::Path,
    sync::Arc,
};

use anyhow::{Context, ensure};
use mplayer_core::{Artwork, Result, TrackMetadata, TrackSegment};
use symphonia::core::{
    io::MediaSourceStream,
    meta::{MetadataOptions, MetadataReader, MetadataRevision, StandardVisualKey},
};

use crate::decoder::{MAX_ARTWORK_BYTES, apply_tag};
use container::Reader;
use filter::{Filter, Scratch};

pub(crate) const PCM_RATE: u32 = 176_400;
// Larger blocks amortize pool dispatch and overlapping stage windows. The
// playback session still feeds its device resampler in 1024-frame slices.
const PCM_BLOCK: u64 = 4096;

pub(crate) fn recognizes(path: &Path) -> Result<bool> {
    let mut file = File::open(path)?;
    let mut header = [0; 4];
    match file.read_exact(&mut header) {
        Ok(()) => Ok(header == *b"DSD " || header == *b"FRM8"),
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => Ok(false),
        Err(error) => Err(error.into()),
    }
}

/// Bound the complete ID3 blob before passing it to the metadata reader: not
/// every upstream ID3 frame reader enforces MetadataOptions limits yet.
fn id3(path: &Path, tag: Option<(u64, u64)>) -> Result<Option<MetadataRevision>> {
    let Some((offset, length)) = tag else {
        return Ok(None);
    };
    ensure!(length >= 10, "Truncated DSD ID3 header");
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(offset))?;
    let mut header = [0; 10];
    file.read_exact(&mut header)?;
    ensure!(
        &header[..3] == b"ID3" && header[6..].iter().all(|&byte| byte < 128),
        "Invalid DSD ID3 header"
    );
    let size = header[6..]
        .iter()
        .fold(0_usize, |size, &byte| (size << 7) | usize::from(byte));
    let size = size + 10 + usize::from(header[3] == 4 && header[5] & 0x10 != 0) * 10;
    ensure!(size as u64 <= length, "Truncated DSD ID3 tag");
    ensure!(
        size <= MAX_ARTWORK_BYTES + 64 * 1024,
        "DSD ID3 tag exceeds metadata size limit"
    );
    let mut data = vec![0; size];
    file.seek(SeekFrom::Start(offset))?;
    file.read_exact(&mut data)?;
    let stream = MediaSourceStream::new(Box::new(Cursor::new(data)), Default::default());
    let mut reader =
        symphonia::default::meta::Id3v2Reader::try_new(stream, MetadataOptions::default())?;
    Ok(Some(reader.read_all()?.revision))
}

fn optional_id3(path: &Path, tag: Option<(u64, u64)>) -> Option<MetadataRevision> {
    match id3(path, tag) {
        Ok(metadata) => metadata,
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "Ignoring invalid DSD metadata");
            None
        }
    }
}

pub(crate) fn artwork(path: &Path) -> Result<Option<Artwork>> {
    let reader = Reader::open(path)?;
    let Some(metadata) = optional_id3(path, reader.tag) else {
        return Ok(None);
    };
    let visual = metadata
        .media
        .visuals
        .iter()
        .filter(|visual| !visual.data.is_empty() && visual.data.len() <= MAX_ARTWORK_BYTES)
        .min_by_key(|visual| visual.usage != Some(StandardVisualKey::FrontCover));
    Ok(visual.map(|visual| Artwork {
        data: Arc::from(visual.data.as_ref()),
        mime_type: visual
            .media_type
            .clone()
            .unwrap_or_else(|| "application/octet-stream".into()),
    }))
}

pub(crate) struct DsdDecoder {
    reader: Reader,
    filter: Option<Arc<Filter>>,
    window: Vec<u8>,
    scratch: Scratch,
    position: u64,
    end: u64,
    segment: Option<TrackSegment>,
    pub metadata: TrackMetadata,
}

impl DsdDecoder {
    pub fn open(path: &Path) -> Result<Self> {
        let reader = Reader::open(path)
            .with_context(|| format!("Reading DSD container {}", path.display()))?;
        let mut metadata = reader.metadata.clone();
        if let Some(tags) = optional_id3(path, reader.tag) {
            for tag in &tags.media.tags {
                apply_tag(&mut metadata, tag);
            }
        }
        let factor = u64::from(reader.rate / PCM_RATE);
        let end = reader.samples.div_ceil(factor);
        Ok(Self {
            reader,
            metadata,
            filter: None,
            window: Vec::new(),
            scratch: Scratch::default(),
            position: 0,
            end,
            segment: None,
        })
    }

    pub fn next(&mut self, output: &mut Vec<f64>) -> Result<bool> {
        self.next_with(output, Filter::process)
    }

    fn next_with(
        &mut self,
        output: &mut Vec<f64>,
        process: impl FnOnce(&Filter, &[u8], usize, &mut [f64], &mut Scratch),
    ) -> Result<bool> {
        output.clear();
        if self.position >= self.end {
            return Ok(false);
        }
        let filter = self
            .filter
            .get_or_insert_with(|| Filter::get(self.reader.rate));
        let channels = self.reader.channels;
        let frames = (self.end - self.position).min(PCM_BLOCK) as usize;
        let first_bit = i128::from(self.position) * filter.factor as i128 - filter.half as i128;
        let first_byte = first_bit / 8;
        let byte_frames = ((frames - 1) * filter.factor + 2 * filter.half) / 8 + 1;
        self.window.resize(byte_frames * channels, 0x55);
        self.window.fill(0x55);
        let read_start = first_byte.max(0) as u64;
        let read_end = (first_byte + byte_frames as i128)
            .min(i128::from(self.reader.samples.div_ceil(8)))
            .max(0) as u64;
        if read_end > read_start {
            let offset = (i128::from(read_start) - first_byte) as usize * channels;
            let count = (read_end - read_start) as usize * channels;
            self.reader
                .read(read_start, &mut self.window[offset..offset + count])?;
            if !self.reader.samples.is_multiple_of(8) && read_end == self.reader.samples.div_ceil(8)
            {
                let mask = 0xff_u8 << (8 - self.reader.samples % 8);
                for byte in &mut self.window[offset + count - channels..offset + count] {
                    *byte = (*byte & mask) | (0x55 & !mask);
                }
            }
        }
        output.resize(frames * channels, 0.0);
        process(filter, &self.window, channels, output, &mut self.scratch);
        self.position += frames as u64;
        Ok(true)
    }

    pub fn set_segment(&mut self, segment: TrackSegment) -> Result<()> {
        ensure!(
            segment.start_frame < self.reader.samples
                && segment
                    .end_frame
                    .is_none_or(|end| end > segment.start_frame && end <= self.reader.samples),
            "Invalid DSD segment bounds"
        );
        self.segment = Some(segment);
        self.seek(0)
    }

    pub fn seek(&mut self, position_ms: u64) -> Result<()> {
        let segment = self.segment.unwrap_or(TrackSegment {
            start_frame: 0,
            end_frame: None,
        });
        let end = segment.end_frame.unwrap_or(self.reader.samples);
        let target = u128::from(segment.start_frame)
            + u128::from(position_ms) * u128::from(self.reader.rate) / 1000;
        // Source frames remain DSD bits (including CUE bounds); output is on
        // the same absolute PCM grid as an uninterrupted decode.
        let factor = u64::from(self.reader.rate / PCM_RATE);
        self.position = target
            .min(u128::from(end))
            .try_into()
            .map(|frame: u64| frame.div_ceil(factor))?;
        self.end = end.div_ceil(factor);
        Ok(())
    }
}

#[cfg(test)]
#[path = "../../tests/unit/dsd.rs"]
pub(crate) mod tests;
