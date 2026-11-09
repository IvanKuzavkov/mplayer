# mplayer

A local music player in Rust 2024: egui/eframe UI, bundled SQLite through SQLx,
Symphonia decoding, and CPAL output. Supports FLAC, PCM WAV, MP3, Ogg Vorbis,
and DSD in DSF/DSDIFF files; mono and stereo. Import music folders, search tags,
build a queue, and save playlists.

## Build and run

Use a current stable Rust toolchain (the dependency stack requires Rust 1.95 or
newer), and a C/C++ toolchain for bundled SQLite and native platform libraries.

- macOS: Xcode Command Line Tools; CPAL's current CoreAudio backend requires macOS 14.2+.
- Windows: Rust's MSVC toolchain and Visual Studio C++ Build Tools/Windows SDK.
- Debian/Ubuntu: install `pkg-config libasound2-dev libfontconfig1-dev
libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev libxkbcommon-dev
libssl-dev libwayland-dev` with your package manager.

```sh
cargo run --release --locked -p mplayer
cargo run --release --locked -p mplayer -- "/path/to/music"
```

Development builds optimize `mplayer-audio` at level 2 so DSD filtering and
resampling keep up with playback. Debug symbols and checks remain enabled;
the application and UI retain their normal development settings.

Choose "Add music folder…" in the sidebar to open the system folder picker;
multiple folders can be selected together. You can also import a path under
"Add a file or path", or drop local files/folders into the window. Saved roots show their full paths.
Additional imports queue while a scan runs; Cancel scans cancels current and queued
work. Rescan checks saved folders incrementally. Directory symlinks are skipped
during recursive scans to prevent cycles. Importing a file scans its containing
folder, including sibling audio files and CUE sheets. Nested roots are deduplicated.

Each music folder commits atomically: if a file or CUE sheet fails, its previous
contents remain available while other folders continue. The Folders tab shows
persisted status and error details on hover, with a Rescan button for that folder.
Targeted rescans reuse the current root generation. Removed files and folders stay
as missing records so playlist entries and track IDs survive restoration.
Audio metadata caching compares file size and modification time in whole seconds;
same-size edits within one second may not be detected.

The library has Tracks, Folders, and Albums tabs with full-height scrolling and
paginated queries. Folder groups use the immediate directory containing a track.
Albums use album title and album artist (falling back to track artist), and their
tracks sort by disc and track number. Rescan an existing library to read these tags.

Use each track's Play button or its menu to enqueue it or add it to a saved playlist.
Play a playlist to replace the queue; its Up / Down buttons change saved order.
Playback controls include pause, stop, previous/next, seeking, volume, and output
device selection. Volume, output preference, folders, tracks, and playlists
survive a restart; the application starts stopped and the queue is session-only.
The output selector stays visible above the now-playing panel. Choosing another
device after output failure retries the retained track at its previous position.
Output-device discovery runs immediately in the background, then three seconds
after each completed scan. Blocking native enumeration stays off the playback
worker and Tokio executor threads; scans never overlap. The device list may be
briefly empty at startup while the first scan completes.
The now-playing panel shows embedded PNG/JPEG front-cover artwork, with a record
placeholder for tracks without a supported image. Covers decode in the background
and are resized for display; rapidly skipping tracks does not show stale covers.

On Linux, native dialogs use an installed XDG Desktop Portal backend (GNOME,
GTK, or KDE), with Zenity as a fallback. These are runtime requirements for the
[native dialog backend](https://docs.rs/rfd/0.17.2/rfd/); Windows and macOS use their system dialogs.

## Audio behavior

UTF-8 CUE sheets (with or without BOM) support single- and multiple-file AUDIO
tracks referencing audio files in the same directory. CUE tags override embedded
tags, and INDEX 01 defines sample-frame playback boundaries. INDEX 00 does not
create a separate track; pregap audio between INDEX 01 boundaries belongs to the
preceding track. INDEX timestamps ending in frame `75` carry into the next second
with a log warning; the source CUE file is unchanged. Synthetic nonzero gaps,
cross-folder references, invalid bounds,
and competing sheets for the same file produce a folder error. CUE edits are read
on every scan, even when the audio file is unchanged. Seeking and output recovery
use positions relative to the selected logical track.

DSF (`.dsf`) and DSDIFF (`.dff`) support DSD64, DSD128, and DSD256, including
DST-compressed DFF. DSD is filtered to 176.4 kHz PCM internally, then resampled
to the device's automatically selected output rate. Volume, seeking, queue
playback, and embedded artwork work through the existing controls; the format
display retains the source
DSD rate and 1-bit depth. DFF files appear as one track, with external CUE sheets
available for track boundaries. SACD ISO, DoP/native DSD output, DSD512, and
multichannel DSD are not supported.

DSD conversion uses a 129-tap packed-bit filter, one to three short half-band stages,
and a final interpolated FIR at 176.4 kHz (a sparse 97-tap Kaiser model
followed by a 47-tap image mask). Processing stays `f64`, with 0.01 dB flatness through
24 kHz and at least 90 dB rejection from 30 kHz. All source rates share about
17 KiB of kernels. Decoder-owned intermediate buffers are reused between reads.
The decoder batches 4,096 PCM frames; the playback session feeds the device
converter in 1,024-frame slices. Large batches share a dedicated Rayon pool capped
at two workers; small reads use the sequential kernel. The pool initializes on
demand, requests 256 KiB of stack per worker, and falls back to sequential
processing on a single CPU or if pool creation fails. Centered source windows
compensate filter delay and preserve timing across reads and seeks.

See the [DSD filter design and mathematics](docs/FILTER.md) for
the design history, equations, lookup-table derivation, and timing rules.

For ordinary PCM tracks, the player requests the source's native rate. For DSD,
it automatically tries the device's highest advertised compatible PCM rate first,
within the converter's 768 kHz limit. At each rate it retains the existing
precision and channel-layout preferences; it does not force 16-bit output. If a
candidate cannot be opened, other advertised candidates are tried before falling
back to the default output configuration. Rubato resampling is used when needed.
No output-rate environment override is required. The UI and signal-path log show
the source and actual output rates. Switching between PCM and DSD renegotiates the
output rather than reusing a stream with the other rate policy. OS mixing can
still change the hardware signal; this is shared output, not an exclusive or
bit-perfect hardware mode. The playback pipeline uses `f64` samples and chooses
output formats that preserve decoded PCM precision where possible. DSD-to-PCM
conversion does not preserve the original DSD bitstream.

On Linux, output requests a 20 ms device period within the advertised limits,
with a retry using the device default if that request is rejected. ALSA normally
buffers about two periods. This trades some output latency for scheduling
headroom without changing the decoded samples; actual negotiated sizes are
recorded in the output-open log.

Compatible consecutive tracks are decoded ahead into the same bounded output
buffer. Decoder delay/padding is trimmed where supported. Changing source rates
may briefly restart the output device. Multichannel layouts and chained streams
that change layout/rate inside a track are rejected explicitly. Streaming URLs,
ALAC/AAC/AIFF, external artwork files, folder watching, media keys, and exclusive output are not
implemented in this release.

Compatible queued tracks share resampler history. A successor added after the
preceding converter has already finalized cannot retroactively preserve that
history; enqueue upcoming tracks in advance for continuous album playback.

## Data and logging

The folder-based schema replaces the initial migration in place. Databases made
with the previous schema are incompatible: stop the player and explicitly recreate
the development database before using this version. The application does not
delete or reset an existing database automatically. Recreating it also discards
its playlists and settings, so preserve a copy if needed.

Debug builds store SQLite and `logs/` in a `data/` directory beside the executable
(normally `target/debug/data/`), regardless of the directory used to launch the
player. `MPLAYER_DATA_DIR` can override this debug location.

Release builds use the standard application-data directory resolved using
`directories`:

- Windows: `%APPDATA%\mplayer\mplayer\data` (Roaming AppData).
- macOS: `~/Library/Application Support/org.mplayer.mplayer`.
- Linux: `$XDG_DATA_HOME/mplayer`, falling back to `~/.local/share/mplayer`.

Release builds ignore `MPLAYER_DATA_DIR` and always use the platform-standard
path. To override the debug location:

```sh
MPLAYER_DATA_DIR=/tmp/mplayer-dev cargo run --locked -p mplayer
```

On startup the log records the actual data-directory path. `RUST_LOG` filtering
remains available in both debug and release builds.

Logs are structured JSON lines, rotate daily in UTC, and retain **at most two
matching files total, including the current file**. They use a bounded background
writer; overload drops log records instead of blocking the UI or playback worker.
The application reports dropped-record counts. If files cannot be opened, logging
falls back to stderr. The logging guard remains alive through service shutdown to
flush records. Abrupt process termination cannot guarantee flushing.

Application events default to `info`, dependencies to `warn`. Enable function-level
debug spans with `RUST_LOG`:

```sh
RUST_LOG=warn,mplayer=debug,mplayer_core=debug,mplayer_audio=debug,mplayer_ui=debug cargo run --release --locked -p mplayer
```

Spans cover application tasks, database operations, scanning, source opening,
playback commands, seeking and device setup. Async tasks and blocking workers
inherit operation context. Span-close records include elapsed busy/idle time.
Audio callbacks perform no tracing; the worker reports aggregated underruns.
While playing, the worker also logs diagnostics every five seconds: track
position, current and minimum callback-observed PCM queue depth, maximum callback
interval, and separate application-buffer and device-underrun counters. A zero
underrun count does not prove successful hardware delivery; backend and downstream
losses may not be reported. Paths and track identifiers may appear in diagnostic logs.

## Development and validation

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

Tests use generated audio, simulated output buffers, temporary databases, and a
fake playback backend; they require no output device. Original audio fixtures can
be regenerated with `python3 mplayer-audio/tests/fixtures/generate.py` (no Python
packages or encoder executables required). Manual performance tests run with:

```sh
cargo test --release --locked -p mplayer-audio -- --ignored --nocapture
cargo test --release --locked -p mplayer-core -- --ignored --nocapture
```

Offline DSD correctness checks can compare local passages against an independently
decoded FFmpeg reference. This optional diagnostic uses existing Python NumPy,
`ffmpeg`, and `ffprobe` installations; it installs nothing and does not open an
audio device:

```sh
python3 mplayer-audio/tests/compare_dsd.py "/path/to/track.dsf" --positions 110 121
python3 mplayer-audio/tests/compare_dsd.py --self-test
```

The diagnostic checks sample-exact agreement between continuous, seeked, and
sequential-filter decoding, and verifies playback-queue delivery through an i32
callback using simulated 883- and 3528-frame periods. It then compares ten-second
windows with FFmpeg using a common audible-band analysis filter and one fitted
delay/gain per window. Different DSD filters need not be bit-identical: the report
shows global and local 10 ms discrepancies rather than asserting exact agreement.
Artifacts and `report.json` stay under `target/diagnostics/dsd/` by default. The
self-test verifies that the comparison detects an injected 5 ms silence gap.
