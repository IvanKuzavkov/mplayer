use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
    sync::Arc,
};

use crate::pcm::PcmFormat;
use anyhow::{Context, bail};
use mplayer_core::{Artwork, MetadataProbe, Result, TrackMetadata, TrackSegment};
use symphonia::core::{
    codecs::audio::{AudioDecoder, AudioDecoderOptions},
    common::Limit,
    errors::Error,
    formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType, probe::Hint},
    io::{MediaSource, MediaSourceStream},
    meta::{MetadataOptions, StandardTag, StandardVisualKey},
    units::{Time, TimeBase, Timestamp},
};

/// Keeps synchronous Symphonia reads separate from the async application interface.
/// A future network adapter can return a seekable cache-backed MediaSource here.
fn open_source(path: &Path) -> Result<Box<dyn MediaSource>> {
    Ok(Box::new(File::open(path)?))
}

pub(crate) const MAX_ARTWORK_BYTES: usize = 8 * 1024 * 1024;

fn read_picture_u32(file: &mut File, end: u64) -> Result<u32> {
    if file
        .stream_position()?
        .checked_add(4)
        .is_none_or(|position| position > end)
    {
        bail!("Truncated FLAC picture metadata");
    }
    let mut bytes = [0; 4];
    file.read_exact(&mut bytes)?;
    Ok(u32::from_be_bytes(bytes))
}

fn read_flac_picture(file: &mut File, end: u64) -> Result<Option<(bool, Artwork)>> {
    let front_cover = read_picture_u32(file, end)? == 3;
    let mime_length = read_picture_u32(file, end)? as usize;
    if mime_length > 256 || file.stream_position()?.saturating_add(mime_length as u64) > end {
        return Ok(None);
    }
    let mut mime = vec![0; mime_length];
    file.read_exact(&mut mime)?;
    let description_length = read_picture_u32(file, end)?;
    let after_dimensions = file
        .stream_position()?
        .checked_add(u64::from(description_length) + 16)
        .filter(|position| *position <= end)
        .with_context(|| "Truncated FLAC picture description")?;
    file.seek(SeekFrom::Start(after_dimensions))?;
    let image_length = read_picture_u32(file, end)? as usize;
    if image_length == 0
        || image_length > MAX_ARTWORK_BYTES
        || file.stream_position()?.saturating_add(image_length as u64) > end
    {
        return Ok(None);
    }
    let mut data = vec![0; image_length];
    file.read_exact(&mut data)?;
    Ok(Some((
        front_cover,
        Artwork {
            data: Arc::from(data),
            mime_type: String::from_utf8(mime)
                .unwrap_or_else(|_| "application/octet-stream".into()),
        },
    )))
}

/// FLAC's picture blocks carry their lengths explicitly. Read only a bounded selected
/// picture: upstream metadata options are currently not enforced by every reader.
fn native_flac_artwork(path: &Path) -> Result<Option<Option<Artwork>>> {
    let mut file = File::open(path)?;
    let mut signature = [0; 4];
    match file.read_exact(&mut signature) {
        Ok(()) if &signature == b"fLaC" => (),
        Ok(()) => return Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    let file_length = file.metadata()?.len();
    let mut fallback = None;
    for _ in 0..4096 {
        let mut header = [0; 4];
        file.read_exact(&mut header)?;
        let last = header[0] & 0x80 != 0;
        let length = u32::from_be_bytes([0, header[1], header[2], header[3]]);
        let end = file
            .stream_position()?
            .checked_add(u64::from(length))
            .filter(|position| *position <= file_length)
            .with_context(|| "Truncated FLAC metadata block")?;
        if header[0] & 0x7f == 6
            && let Some((front, artwork)) = read_flac_picture(&mut file, end)?
        {
            if front {
                return Ok(Some(Some(artwork)));
            }
            if fallback.is_none() {
                fallback = Some(artwork);
            }
        }
        file.seek(SeekFrom::Start(end))?;
        if last {
            return Ok(Some(fallback));
        }
    }
    bail!("FLAC metadata block count exceeds artwork lookup limit")
}

fn open_format(path: &Path, artwork: bool) -> Result<Box<dyn FormatReader>> {
    let stream = MediaSourceStream::new(open_source(path)?, Default::default());
    let mut hint = Hint::new();
    if let Some(extension) = path.extension().and_then(|value| value.to_str()) {
        hint.with_extension(extension);
    }
    let metadata = if artwork {
        MetadataOptions::default()
            .limit_tag_bytes(Limit::Maximum(64 * 1024))
            .limit_visual_bytes(Limit::Maximum(MAX_ARTWORK_BYTES))
    } else {
        MetadataOptions::default()
    };
    Ok(symphonia::default::get_probe().probe(&hint, stream, FormatOptions::default(), metadata)?)
}

#[derive(Default, Clone)]
pub struct SymphoniaProbe;

impl MetadataProbe for SymphoniaProbe {
    #[tracing::instrument(skip_all, fields(path = %path.display()), err)]
    fn probe(&self, path: &Path) -> Result<TrackMetadata> {
        Ok(Decoder::open(path)?.metadata)
    }

    #[tracing::instrument(skip_all, fields(path = %path.display()), err)]
    fn artwork(&self, path: &Path) -> Result<Option<Artwork>> {
        if crate::dsd::recognizes(path)? {
            return crate::dsd::artwork(path);
        }
        if let Some(artwork) = native_flac_artwork(path)? {
            return Ok(artwork);
        }
        let mut format = open_format(path, true)?;
        let mut metadata = format.metadata();
        let mut fallback = None;
        loop {
            if let Some(revision) = metadata.current() {
                for visual in revision.media.visuals.iter().chain(
                    revision
                        .per_track
                        .iter()
                        .flat_map(|track| &track.metadata.visuals),
                ) {
                    if visual.data.is_empty() || visual.data.len() > MAX_ARTWORK_BYTES {
                        continue;
                    }
                    let front = visual.usage == Some(StandardVisualKey::FrontCover);
                    if !front && fallback.is_some() {
                        continue;
                    }
                    let artwork = Artwork {
                        data: Arc::from(visual.data.as_ref()),
                        mime_type: visual
                            .media_type
                            .clone()
                            .unwrap_or_else(|| "application/octet-stream".into()),
                    };
                    if front {
                        return Ok(Some(artwork));
                    }
                    if fallback.is_none() {
                        fallback = Some(artwork);
                    }
                }
            }
            if metadata.pop().is_none() {
                break;
            }
        }
        Ok(fallback)
    }
}

pub(crate) fn apply_tag(metadata: &mut TrackMetadata, tag: &symphonia::core::meta::Tag) {
    match &tag.std {
        Some(StandardTag::TrackTitle(value)) => metadata.title = value.to_string(),
        Some(StandardTag::Artist(value)) => metadata.artist = value.to_string(),
        Some(StandardTag::Album(value)) => metadata.album = value.to_string(),
        Some(StandardTag::AlbumArtist(value)) => metadata.album_artist = value.to_string(),
        Some(StandardTag::ReleaseYear(value)) => {
            metadata.year = Some(u32::from(*value)).filter(|year| (1..=9999).contains(year));
        }
        Some(StandardTag::ReleaseDate(value) | StandardTag::RecordingDate(value))
            if metadata.year.is_none() =>
        {
            metadata.year = value
                .get(..4)
                .and_then(|year| year.parse().ok())
                .filter(|year| (1..=9999).contains(year));
        }
        Some(StandardTag::RecordingYear(value)) if metadata.year.is_none() => {
            metadata.year = Some(u32::from(*value)).filter(|year| (1..=9999).contains(year));
        }
        Some(StandardTag::TrackNumber(value)) => {
            metadata.track_number = u32::try_from(*value).ok().filter(|number| *number > 0)
        }
        Some(StandardTag::DiscNumber(value)) => {
            metadata.disc_number = u32::try_from(*value).ok().filter(|number| *number > 0)
        }
        _ => (),
    }
}

/// File metadata describes the source; these properties describe returned PCM.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PcmSpec {
    pub rate: u32,
    pub channels: u16,
    pub format: PcmFormat,
}

enum Backend {
    Symphonia(Box<SymphoniaDecoder>),
    Dsd(Box<crate::dsd::DsdDecoder>),
}

pub(crate) struct Decoder {
    backend: Backend,
    pub metadata: TrackMetadata,
}

impl Decoder {
    pub fn open(path: &Path) -> Result<Self> {
        let backend = if crate::dsd::recognizes(path)? {
            Backend::Dsd(Box::new(crate::dsd::DsdDecoder::open(path)?))
        } else {
            Backend::Symphonia(Box::new(SymphoniaDecoder::open(path)?))
        };
        let metadata = match &backend {
            Backend::Symphonia(decoder) => decoder.metadata.clone(),
            Backend::Dsd(decoder) => decoder.metadata.clone(),
        };
        Ok(Self { backend, metadata })
    }

    pub fn pcm_spec(&self) -> PcmSpec {
        match &self.backend {
            Backend::Symphonia(decoder) => PcmSpec {
                rate: decoder.metadata.sample_rate,
                channels: decoder.metadata.channels,
                format: decoder.pcm_format(),
            },
            Backend::Dsd(decoder) => PcmSpec {
                rate: crate::dsd::PCM_RATE,
                channels: decoder.metadata.channels,
                format: PcmFormat::Float(64),
            },
        }
    }

    pub fn pcm_format(&self) -> PcmFormat {
        self.pcm_spec().format
    }

    pub fn next(&mut self, output: &mut Vec<f64>) -> Result<bool> {
        match &mut self.backend {
            Backend::Symphonia(decoder) => decoder.next(output),
            Backend::Dsd(decoder) => decoder.next(output),
        }
    }

    pub fn seek(&mut self, position_ms: u64) -> Result<()> {
        match &mut self.backend {
            Backend::Symphonia(decoder) => decoder.seek(position_ms),
            Backend::Dsd(decoder) => decoder.seek(position_ms),
        }
    }

    pub fn set_segment(&mut self, segment: TrackSegment) -> Result<()> {
        match &mut self.backend {
            Backend::Symphonia(decoder) => decoder.set_segment(segment),
            Backend::Dsd(decoder) => decoder.set_segment(segment),
        }
    }
}

struct SymphoniaDecoder {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    time_base: TimeBase,
    first: Vec<f64>,
    pcm_format: Option<PcmFormat>,
    skip_until: Option<Timestamp>,
    skip_until_frame: Option<u64>,
    segment: Option<TrackSegment>,
    remaining_frames: Option<u64>,
    pub metadata: TrackMetadata,
}

impl SymphoniaDecoder {
    #[tracing::instrument(skip_all, fields(path = %path.display()), err)]
    pub fn open(path: &Path) -> Result<Self> {
        let format = open_format(path, false)?;
        let track = format
            .default_track(TrackType::Audio)
            .with_context(|| "No decodable audio track")?;
        let params = track
            .codec_params
            .as_ref()
            .and_then(|params| params.audio())
            .with_context(|| "Missing audio codec parameters")?;
        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(params, &AudioDecoderOptions::default())?;
        let rate = params.sample_rate.unwrap_or(0);
        let duration_ms = track
            .time_base
            .and_then(|base| {
                track
                    .duration
                    .and_then(|duration| base.calc_duration(duration))
            })
            .map(|duration| duration.as_millis().max(0) as u64)
            .or_else(|| {
                track
                    .num_frames
                    .map(|frames| frames.saturating_mul(1000) / u64::from(rate.max(1)))
            })
            .unwrap_or(0);
        let metadata = TrackMetadata {
            title: path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            artist: String::new(),
            album: String::new(),
            duration_ms,
            sample_rate: rate,
            channels: params
                .channels
                .as_ref()
                .map_or(0, |channels| channels.count() as u16),
            bit_depth: params.bits_per_sample,
            codec: decoder.codec_info().short_name.into(),
            ..Default::default()
        };
        let mut decoded = Self {
            track_id: track.id,
            time_base: track
                .time_base
                .or_else(|| TimeBase::try_from_recip(rate))
                .with_context(|| "Missing audio timebase")?,
            format,
            decoder,
            first: Vec::new(),
            pcm_format: None,
            skip_until: None,
            skip_until_frame: None,
            segment: None,
            remaining_frames: None,
            metadata,
        };
        let mut first = Vec::new();
        if !decoded.read_packet(&mut first)? {
            bail!("Audio track contains no decodable samples");
        }
        decoded.first = first;
        if let Some(revision) = decoded.format.metadata().skip_to_latest() {
            for tag in revision.media.tags.iter().chain(
                revision
                    .per_track
                    .iter()
                    .filter(|metadata| metadata.track_id == u64::from(decoded.track_id))
                    .flat_map(|metadata| &metadata.metadata.tags),
            ) {
                apply_tag(&mut decoded.metadata, tag);
            }
        }
        Ok(decoded)
    }

    pub fn set_segment(&mut self, segment: TrackSegment) -> Result<()> {
        if segment
            .end_frame
            .is_some_and(|end| end <= segment.start_frame)
        {
            bail!("Invalid audio segment bounds");
        }
        self.segment = Some(segment);
        self.seek(0)
    }

    pub fn next(&mut self, output: &mut Vec<f64>) -> Result<bool> {
        output.clear();
        if self.remaining_frames == Some(0) {
            return Ok(false);
        }
        let available = if !self.first.is_empty() {
            output.append(&mut self.first);
            true
        } else {
            self.read_packet(output)?
        };
        if !available {
            if self.remaining_frames.is_some_and(|remaining| remaining > 0) {
                bail!("Audio file ended before the segment boundary");
            }
            return Ok(false);
        }
        if let Some(remaining) = &mut self.remaining_frames {
            let channels = usize::from(self.metadata.channels);
            let frames =
                (output.len() / channels).min(usize::try_from(*remaining).unwrap_or(usize::MAX));
            output.truncate(frames * channels);
            *remaining -= frames as u64;
        }
        Ok(!output.is_empty())
    }

    pub fn pcm_format(&self) -> PcmFormat {
        self.pcm_format.expect("open decodes the first packet")
    }

    fn read_packet(&mut self, output: &mut Vec<f64>) -> Result<bool> {
        let mut errors = 0;
        loop {
            let Some(packet) = self.format.next_packet()? else {
                return Ok(false);
            };
            if packet.track_id != self.track_id {
                continue;
            }
            let audio = match self.decoder.decode(&packet) {
                Ok(audio) => audio,
                Err(Error::DecodeError(message)) if errors < 32 => {
                    errors += 1;
                    tracing::warn!(message, "Skipping malformed audio packet");
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            let channels = audio.spec().channels().count();
            let pcm_format = PcmFormat::decoded(&audio, self.metadata.bit_depth);
            if self
                .pcm_format
                .is_some_and(|previous| previous != pcm_format)
            {
                bail!("Decoded PCM representation changed inside a track");
            }
            self.pcm_format = Some(pcm_format);
            let rate = audio.spec().rate();
            if !(1..=2).contains(&channels) {
                bail!("Only mono and stereo audio is supported");
            }
            if self.metadata.sample_rate != 0 && rate != self.metadata.sample_rate {
                bail!("Audio sample rate changed inside a track; chained streams are unsupported");
            }
            if self.metadata.channels != 0 && channels != usize::from(self.metadata.channels) {
                bail!("Audio channel layout changed inside a track");
            }
            self.metadata.sample_rate = rate;
            self.metadata.channels = channels as u16;
            let skip_frames = if let Some(target) = self.skip_until_frame {
                let packet_start = i128::from(packet.pts.get())
                    * i128::from(self.time_base.numer.get())
                    * i128::from(rate)
                    / i128::from(self.time_base.denom.get())
                    + i128::from(packet.trim_start.get());
                let frames = usize::try_from((i128::from(target) - packet_start).max(0))
                    .with_context(|| "Segment seek offset overflow")?;
                if frames >= audio.frames() {
                    continue;
                }
                self.skip_until_frame = None;
                frames
            } else if let Some(target) = self.skip_until {
                let nanos = target
                    .duration_from(packet.pts)
                    .and_then(|duration| self.time_base.calc_duration(duration))
                    .map_or(0, |time| time.as_nanos().max(0) as u128);
                // Gapless decoders have already removed trim_start from their PCM.
                // The packet timestamp still includes that encoder-delay region.
                let frames = ((nanos * u128::from(rate)).div_ceil(1_000_000_000) as usize)
                    .saturating_sub(packet.trim_start.get() as usize);
                if frames >= audio.frames() {
                    continue;
                }
                self.skip_until = None;
                frames
            } else {
                0
            };
            if audio.frames() == 0 {
                continue;
            }
            if audio.samples_interleaved() > 4_000_000 {
                bail!("Decoded audio packet exceeds safety limit");
            }
            output.resize((audio.frames() - skip_frames) * channels, 0.0);
            audio
                .slice(skip_frames..)
                .copy_to_slice_interleaved(output.as_mut_slice());
            return Ok(true);
        }
    }

    #[tracing::instrument(skip_all, fields(position_ms), err)]
    pub fn seek(&mut self, position_ms: u64) -> Result<()> {
        if let Some(segment) = self.segment {
            let frame = segment
                .start_frame
                .checked_add(
                    position_ms
                        .checked_mul(u64::from(self.metadata.sample_rate))
                        .with_context(|| "Segment seek overflow")?
                        / 1000,
                )
                .with_context(|| "Segment seek overflow")?;
            let frame = segment
                .end_frame
                .map_or(frame, |end| frame.min(end.saturating_sub(1)));
            let timestamp = u128::from(frame) * u128::from(self.time_base.denom.get())
                / (u128::from(self.metadata.sample_rate) * u128::from(self.time_base.numer.get()));
            self.format.seek(
                SeekMode::Accurate,
                SeekTo::Timestamp {
                    ts: Timestamp::new(timestamp.try_into()?),
                    track_id: self.track_id,
                },
            )?;
            self.decoder.reset();
            self.first.clear();
            self.skip_until = None;
            self.skip_until_frame = Some(frame);
            self.remaining_frames = segment.end_frame.map(|end| end - frame);
            return Ok(());
        }
        let target = self.format.seek(
            SeekMode::Accurate,
            SeekTo::Time {
                time: Time::from_millis_u64(position_ms),
                track_id: Some(self.track_id),
            },
        )?;
        self.decoder.reset();
        self.first.clear();
        self.skip_until = Some(target.required_ts);
        Ok(())
    }
}

#[cfg(test)]
#[path = "../tests/unit/decoder.rs"]
mod tests;
