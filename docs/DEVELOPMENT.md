# Development and diagnostics

For player features and everyday use, see the [README](../README.md).

## Build requirements

Use a current stable Rust toolchain (the dependency stack requires Rust 1.95 or
newer), plus a C/C++ toolchain for bundled SQLite and native platform libraries.

- **macOS:** Xcode Command Line Tools; audio output requires macOS 14.2 or newer.
- **Windows:** Rust's MSVC toolchain and Visual Studio C++ Build Tools/Windows SDK.
- **Debian/Ubuntu:** install the native build dependencies:

  ```sh
  sudo apt install pkg-config libasound2-dev libfontconfig1-dev libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev libxkbcommon-dev libssl-dev libwayland-dev
  ```

On Linux, native folder dialogs also need an XDG Desktop Portal backend
(GNOME, GTK, or KDE), or Zenity as a fallback.

The application uses Rust 2024, egui/eframe, bundled SQLite through SQLx,
Symphonia, and CPAL. Development builds optimize `mplayer-audio` at level 2 so
DSD filtering and resampling keep up with playback while retaining debug symbols
and checks. Other workspace crates use the normal development settings.

## Checks and fixtures

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

Ordinary tests use generated audio, temporary databases, and simulated output;
they do not require an audio device. Regenerate the audio fixtures with:

```sh
python3 mplayer-audio/tests/fixtures/generate.py
```

The generator needs no Python packages or encoder executables. Ignored tests
cover manual performance and hardware diagnostics; inspect each test's required
environment variables before running it. List the available tests with:

```sh
cargo test --release --locked -p mplayer-audio -- --list
cargo test --release --locked -p mplayer-core -- --list
```

## Development data and logs

Debug builds store `library.sqlite3` and `logs/` beside the executable, normally
in `target/debug/data/`, regardless of the launch directory. Override this with:

```sh
MPLAYER_DATA_DIR=/tmp/mplayer-dev cargo run --locked -p mplayer
```

Release builds ignore `MPLAYER_DATA_DIR` and use the platform data location listed
in the README. The startup log records the actual path.

The folder-based schema replaced the initial development migration in place.
Databases created with that earlier schema are incompatible. Back up the database
before explicitly recreating it; doing so discards its playlists and settings.
The application does not automatically delete or reset an existing database.

Logs use JSON lines, rotate daily in UTC, and retain at most two matching files.
Application events default to `info`, dependencies to `warn`. Enable debug spans:

```sh
RUST_LOG=warn,mplayer=debug,mplayer_core=debug,mplayer_audio=debug,mplayer_ui=debug cargo run --release --locked -p mplayer
```

Playback diagnostics report position, PCM queue depth, callback intervals, and
application/device underrun counts every five seconds. Paths and track identifiers
can appear in logs. Logging runs outside audio callbacks through a bounded writer;
overload drops log records rather than blocking playback. A zero underrun count
does not prove uninterrupted delivery by the OS or DAC.

## Offline DSD comparison

`mplayer-audio/tests/compare_dsd.py` compares real DSF/DFF passages against FFmpeg.
It requires Python 3.9+, NumPy, Cargo, and `ffmpeg`/`ffprobe` on `PATH`. Set up an
isolated Python environment and provide FFmpeg through your platform's package
manager or a local build:

```sh
python3 -m venv target/diagnostics/dsd/.venv
source target/diagnostics/dsd/.venv/bin/activate
python -m pip install numpy
python mplayer-audio/tests/compare_dsd.py --self-test
python mplayer-audio/tests/compare_dsd.py "/path/to/track.dsf" --positions 110 121
```

The activation command above is for POSIX shells; on Windows, activate
`target\diagnostics\dsd\.venv\Scripts\Activate.ps1` in PowerShell. If the local
virtual environment already contains FFmpeg and NumPy, activation is sufficient.
The script itself installs nothing and does not open an audio device.

Positions are centers of ten-second windows, defaulting to 110 and 121 seconds.
Windows near the beginning start at zero. Each run creates a directory under
`target/diagnostics/dsd/`; use `--output PATH` to choose another parent directory.
The virtual environment and reports are build artifacts and disappear if `target/`
is cleaned.

The script checks exact agreement between continuous, seeked, and sequential
DSD decoding, then checks simulated i32 callback delivery with 883- and 3528-frame
periods. It compares exported PCM with FFmpeg through the same audible-band
analysis filter and fits one delay and gain per window. `report.json` contains
overall and local 10 ms differences. These comparisons are descriptive: different
DSD filters need not produce identical samples. The self-test checks alignment,
gain fitting, silence, and detection of an injected 5 ms gap.

## Audio implementation notes

PCM requests the native source rate. DSD is filtered to 176.4 kHz PCM, then uses
the device's highest advertised compatible rate within the converter's 768 kHz
limit, with fallback configurations tried when opening fails. Processing uses
`f64` and prefers output formats that preserve decoded precision where possible.
System mixing can still alter hardware output.

Compatible consecutive tracks share an output buffer and resampler history.
Enqueue successors before the preceding converter finalizes for continuous album
playback. Rate or format changes may restart the output device. Audio output
requests a 20 ms device period where supported and retries the device default if
rejected; the negotiated size is recorded in the log.

Scanning commits each folder atomically, retaining its previous catalog if a file
or CUE sheet fails. Metadata caching uses file size and whole-second modification
time, so same-size edits within one second may not be detected. Recursive scans
skip directory symlinks. CUE files are reread on every scan.

For filter coefficients, frequency response, delay compensation, and the design
history, see [DSD filter design and mathematics](FILTER.md).
