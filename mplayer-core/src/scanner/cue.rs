use std::{
    collections::HashSet,
    path::{Component, Path},
};

use crate::{
    Result, TrackSegment,
    storage::{PreparedMedia, PreparedTrack},
};
use anyhow::{Context, ensure};

/// A sheet is resolved against the complete folder inventory before any SQL writes.
#[tracing::instrument(skip_all, fields(cue = %path.display()), err)]
pub(super) async fn apply(path: &Path, contents: &str, media: &mut [PreparedMedia]) -> Result<()> {
    let cue = parse(contents)?;
    ensure!(
        !cue.files.is_empty(),
        "CUE sheet contains no files: {}",
        path.display()
    );
    let mut numbers = HashSet::new();
    for file in &cue.files {
        // CUE files commonly use Windows separators even on other platforms.
        let name = file.name.replace('\\', "/");
        let relative = Path::new(&name);
        ensure!(
            matches!(relative.components().next(), Some(Component::Normal(_)))
                && relative.components().count() == 1,
            "CUE references must be in the same folder: {}",
            file.name
        );
        let parent = path.parent().context("CUE has no parent")?;
        let referenced = tokio::fs::canonicalize(parent.join(relative))
            .await
            .with_context(|| format!("Missing CUE media: {}", file.name))?;
        ensure!(
            referenced.parent() == Some(parent),
            "CUE references must stay in the same folder"
        );
        let item = media
            .iter_mut()
            .find(|item| item.media.path == referenced)
            .with_context(|| {
                format!("Missing or unsupported CUE media: {}", referenced.display())
            })?;
        ensure!(
            item.tracks.len() == 1 && item.tracks[0].index == 0,
            "Competing CUE references for {}",
            referenced.display()
        );
        ensure!(
            !file.tracks.is_empty(),
            "CUE file contains no tracks: {}",
            file.name
        );
        let source = &item.media.metadata;
        ensure!(
            source.sample_rate > 0 && source.duration_ms > 0,
            "CUE media has no known rate or duration"
        );
        let mut starts = Vec::new();
        for track in &file.tracks {
            ensure!(
                track.format.eq_ignore_ascii_case("AUDIO"),
                "Only AUDIO CUE tracks are supported"
            );
            ensure!(
                (1..=99).contains(&track.id()) && numbers.insert(track.id()),
                "Invalid or duplicate CUE track number"
            );
            ensure!(
                track
                    .pregap
                    .iter()
                    .chain(track.postgap.iter())
                    .all(|time| time.total_seconds() == 0 && time.frames() == 0),
                "Synthetic CUE gaps are unsupported"
            );
            ensure!(
                track.index.iter().filter(|index| index.id() == 1).count() == 1,
                "CUE track needs exactly one INDEX 01"
            );
            let time = track.get_index(1).context("Missing INDEX 01")?.begin_time;
            let cd_frames = u64::from(time.total_seconds()) * 75 + u64::from(time.frames());
            let frame = cd_frames
                .checked_mul(u64::from(source.sample_rate))
                .context("CUE time overflow")?
                / 75;
            ensure!(
                frame * 1000 / u64::from(source.sample_rate) < source.duration_ms,
                "CUE index is outside the media"
            );
            ensure!(
                starts.last().is_none_or(|previous| *previous < frame),
                "CUE indices must increase"
            );
            if let Some(index) = track.get_index(0) {
                let before = u64::from(index.begin_time.total_seconds()) * 75
                    + u64::from(index.begin_time.frames());
                ensure!(before <= cd_frames, "INDEX 00 is after INDEX 01");
            }
            starts.push(frame);
        }
        item.tracks = file
            .tracks
            .iter()
            .enumerate()
            .map(|(index, track)| {
                let start = starts[index];
                let end = starts.get(index + 1).copied();
                let mut metadata = source.clone();
                if let Some(title) = track.title.first() {
                    metadata.title.clone_from(title);
                }
                if let Some(artist) = track.performer.first().or_else(|| cue.performer().first()) {
                    metadata.artist.clone_from(artist);
                }
                if let Some(album) = cue.title().first() {
                    metadata.album.clone_from(album);
                }
                if let Some(artist) = cue.performer().first() {
                    metadata.album_artist.clone_from(artist);
                }
                metadata.track_number = Some(u32::from(track.id()));
                metadata.duration_ms = end.map_or_else(
                    || {
                        source
                            .duration_ms
                            .saturating_sub(start * 1000 / u64::from(source.sample_rate))
                    },
                    |end| (end - start) * 1000 / u64::from(source.sample_rate),
                );
                PreparedTrack {
                    index: track.id(),
                    metadata,
                    segment: Some(TrackSegment {
                        start_frame: start,
                        end_frame: end,
                    }),
                    cue_path: Some(path.into()),
                }
            })
            .collect();
    }
    Ok(())
}

pub(super) fn parse(contents: &str) -> Result<cuna::Cuna> {
    let normalized = contents
        .lines()
        .enumerate()
        .map(|(index, line)| {
            carry_index_frame(line).map_or_else(
                || line.to_owned(),
                |normalized| {
                    tracing::warn!(line = index + 1, original = line.trim(), corrected = %normalized,
                        "Carried CUE frame 75 into the next second");
                    normalized
                },
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    std::panic::catch_unwind(|| cuna::Cuna::new(&normalized))
        .map_err(|_| anyhow::anyhow!("Invalid CUE sheet"))?
        .map_err(Into::into)
}

fn carry_index_frame(line: &str) -> Option<String> {
    let mut fields = line.split_whitespace();
    if !fields.next()?.eq_ignore_ascii_case("INDEX") {
        return None;
    }
    let index = fields.next()?;
    let timestamp = fields.next()?;
    if fields.next().is_some() {
        return None;
    }
    let (minutes, rest) = timestamp.split_once(':')?;
    let (seconds, frames) = rest.split_once(':')?;
    if frames != "75"
        || seconds.len() != 2
        || !minutes
            .bytes()
            .chain(seconds.bytes())
            .all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let seconds = seconds
        .parse::<u32>()
        .ok()
        .filter(|seconds| *seconds < 60)?;
    let total = minutes
        .parse::<u32>()
        .ok()?
        .checked_mul(60)?
        .checked_add(seconds)?
        .checked_add(1)?;
    Some(format!(
        "INDEX {index} {:02}:{:02}:00",
        total / 60,
        total % 60
    ))
}
