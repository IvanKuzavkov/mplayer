#!/usr/bin/env python3
"""Offline DSD checks and audible-band comparison against FFmpeg.

Requires Python 3.9+ and NumPy. Real-file comparisons also require cargo,
ffmpeg, and ffprobe on PATH. Nothing is installed and no audio device is opened.
The Rust checks are exact; FFmpeg comparisons are descriptive, not pass/fail.
"""

import argparse
from datetime import datetime, timezone
import json
import math
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import sys


ROOT = Path(__file__).resolve().parents[2]
RATE = 176_400
SECONDS = 10
GUARD_FRAMES = RATE // 10
MAX_DELAY_FRAMES = RATE // 20
BLOCK_FRAMES = RATE // 100
FILTER_TAPS = 513
FILTER_CUTOFF_HZ = 20_000
FILTER_BETA = 8.6


class DiagnosticError(Exception):
    pass


def arguments(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("file", nargs="?", type=Path, help="local DSF or DFF file")
    parser.add_argument(
        "--positions", nargs="+", type=int, default=[110, 121], metavar="SECONDS",
        help="centers of ten-second windows (default: 110 121); near zero, start at zero",
    )
    parser.add_argument(
        "--output", type=Path, default=ROOT / "target/diagnostics/dsd",
        help="parent directory for a new run directory and report.json",
    )
    parser.add_argument(
        "--self-test", action="store_true",
        help="check synthetic identity, delay/gain, and a 5 ms gap; no external tools needed",
    )
    args = parser.parse_args(argv)
    if args.self_test and args.file is not None:
        parser.error("--self-test does not take an input file")
    if not args.self_test and args.file is None:
        parser.error("provide a DSF/DFF file, or use --self-test")
    if any(position < 0 for position in args.positions):
        parser.error("positions must be nonnegative whole seconds")
    return args


def window_starts(positions, duration):
    starts = sorted({max(0, position - SECONDS // 2) for position in positions})
    if not math.isfinite(duration) or duration <= 0:
        raise DiagnosticError("ffprobe could not determine a finite positive duration")
    if starts[-1] + SECONDS > duration:
        raise DiagnosticError("a requested ten-second window extends beyond the file duration")
    if starts[-1] + SECONDS > 3600:
        raise DiagnosticError("the existing callback diagnostic supports the first 3600 seconds")
    return starts


def run(command, log, env=None, capture=False):
    """Log exact arguments; capture stdout only for the small ffprobe JSON result."""
    with log.open("w", encoding="utf-8") as output:
        output.write("$ " + shlex.join([str(arg) for arg in command]) + "\n")
        output.flush()
        try:
            result = subprocess.run(
                command, cwd=ROOT, env=env, stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE if capture else output, stderr=output,
            )
        except OSError as error:
            raise DiagnosticError(f"cannot run {command[0]}: {error}; log: {log}") from error
        if capture:
            output.write(result.stdout.decode("utf-8", errors="replace"))
        if result.returncode:
            raise DiagnosticError(f"{command[0]} exited with {result.returncode}; log: {log}")
    return result.stdout if capture else None


def probe(path, directory):
    raw = run(
        ["ffprobe", "-v", "error", "-select_streams", "a:0", "-show_entries",
         "stream=codec_name,channels,duration:format=duration", "-of", "json", str(path)],
        directory / "ffprobe.log", capture=True,
    )
    try:
        info = json.loads(raw)
        stream = info["streams"][0]
        channels = int(stream["channels"])
        duration = None
        for value in (stream.get("duration"), info.get("format", {}).get("duration")):
            try:
                candidate = float(value)
                if math.isfinite(candidate) and candidate > 0:
                    duration = candidate
                    break
            except (TypeError, ValueError):
                continue
        if duration is None:
            raise ValueError("missing duration")
    except (KeyError, IndexError, TypeError, ValueError) as error:
        raise DiagnosticError(f"invalid audio information; see {directory / 'ffprobe.log'}") from error
    if stream.get("codec_name") not in {
        "dsd_lsbf", "dsd_msbf", "dsd_lsbf_planar", "dsd_msbf_planar", "dst",
    }:
        raise DiagnosticError("the selected audio stream is not DSD or DST")
    if channels not in (1, 2):
        raise DiagnosticError("only mono and stereo DSD are supported")
    return {"codec": stream["codec_name"], "channels": channels, "duration_seconds": duration}


def rust_checks(path, starts, channels, directory):
    env = os.environ.copy()
    env.update({
        "MPLAYER_DSD_CHECK_FILE": str(path),
        "MPLAYER_DSD_CHECK_DIR": str(directory),
        "MPLAYER_DSD_CHECK_STARTS": ",".join(map(str, starts)),
        "MPLAYER_DSD_CHECK_SECONDS": str(SECONDS),
    })
    checks = [
        ("dsd::tests::dsd_file_correctness_windows", "pcm-check.log"),
        ("tests::dsd_file_callback_correctness", "callback-check.log"),
    ]
    for name, log_name in checks:
        print(f"Running {name}", flush=True)
        run(
            ["cargo", "test", "--release", "--locked", "-p", "mplayer-audio", name,
             "--", "--ignored", "--exact", "--nocapture"],
            directory / log_name, env=env,
        )
    # A renamed or skipped test can exit successfully after running zero tests.
    pcm_log = (directory / "pcm-check.log").read_text(encoding="utf-8", errors="replace")
    for start in starts:
        for sequential in ("false", "true"):
            marker = (
                f"DSD_CORRECTNESS start={start}s seconds={SECONDS} channels={channels} "
                f"sequential={sequential} matching_samples={SECONDS * RATE * channels}"
            )
            if marker not in pcm_log:
                raise DiagnosticError("PCM diagnostic did not confirm every window; see pcm-check.log")
    callback_log = (directory / "callback-check.log").read_text(encoding="utf-8", errors="replace")
    end = starts[-1] + SECONDS
    for period in (883, 3528):
        marker = (
            f"DSD_CALLBACK_CORRECTNESS period={period} rate={RATE} channels={channels} "
            f"seconds={end} matching_i32_samples={end * RATE * channels}"
        )
        if marker not in callback_log:
            raise DiagnosticError("callback diagnostic did not confirm both periods; see callback-check.log")


def lowpass(np, samples):
    """Same centered 513-tap Kaiser FIR on both inputs; no fitted equalization."""
    offsets = np.arange(FILTER_TAPS) - FILTER_TAPS // 2
    cutoff = FILTER_CUTOFF_HZ / RATE
    kernel = 2 * cutoff * np.sinc(2 * cutoff * offsets) * np.kaiser(FILTER_TAPS, FILTER_BETA)
    kernel /= kernel.sum()
    length = 1 << (len(samples) + FILTER_TAPS - 2).bit_length()
    response = np.fft.rfft(kernel, n=length)
    filtered = np.empty(samples.shape, dtype=np.float64)
    for channel in range(samples.shape[1]):
        convolved = np.fft.irfft(np.fft.rfft(samples[:, channel], n=length) * response, n=length)
        filtered[:, channel] = convolved[FILTER_TAPS // 2:FILTER_TAPS // 2 + len(samples)]
    return filtered


def delay_frames(np, actual, reference):
    """One integer lag from summed per-channel correlations; antiphase stereo is safe.

    Positive lag means actual is delayed relative to reference.
    """
    length = 1 << (2 * len(actual) - 2).bit_length()
    spectrum = np.zeros(length // 2 + 1, dtype=np.complex128)
    for channel in range(actual.shape[1]):
        left = actual[:, channel] - actual[:, channel].mean()
        right = reference[:, channel] - reference[:, channel].mean()
        spectrum += np.fft.rfft(left, n=length) * np.fft.rfft(right, n=length).conj()
    correlation = np.fft.irfft(spectrum, n=length)
    lags = np.arange(-MAX_DELAY_FRAMES, MAX_DELAY_FRAMES + 1)
    scores = correlation[lags % length]
    if not np.any(scores):
        return 0
    return int(lags[np.argmax(scores)])


def rms(np, samples):
    return float(np.sqrt(np.mean(samples * samples)))


def db(value):
    # JSON has no infinity: null denotes zero amplitude (minus infinity dB).
    return 20 * math.log10(value) if value > 0 else None


def statistics(np, actual, expected):
    residual = actual - expected
    reference_rms = rms(np, expected)
    error_rms = rms(np, residual)
    relative = error_rms / reference_rms if reference_rms > 0 else None
    return {
        "actual_rms_dbfs": db(rms(np, actual)),
        "reference_rms_dbfs": db(reference_rms),
        "error_rms": error_rms,
        "error_rms_dbfs": db(error_rms),
        "error_peak": float(np.max(np.abs(residual))),
        "relative_error_rms": relative,
        "relative_error_db": db(relative) if relative is not None else None,
    }


def compare(np, actual, reference):
    if actual.shape != reference.shape or actual.ndim != 2 or actual.shape[1] not in (1, 2):
        raise DiagnosticError("PCM inputs must have matching mono/stereo frame shapes")
    if len(actual) <= 2 * GUARD_FRAMES + MAX_DELAY_FRAMES + BLOCK_FRAMES:
        raise DiagnosticError("PCM comparison window is too short")
    if not np.isfinite(actual).all() or not np.isfinite(reference).all():
        raise DiagnosticError("PCM contains non-finite samples")
    actual = lowpass(np, actual)[GUARD_FRAMES:-GUARD_FRAMES]
    reference = lowpass(np, reference)[GUARD_FRAMES:-GUARD_FRAMES]
    lag = delay_frames(np, actual, reference)
    actual_offset = GUARD_FRAMES + max(0, lag)
    reference_offset = GUARD_FRAMES + max(0, -lag)
    if lag > 0:
        actual, reference = actual[lag:], reference[:-lag]
    elif lag < 0:
        actual, reference = actual[:lag], reference[-lag:]
    energy = float(np.sum(reference * reference))
    actual_energy = float(np.sum(actual * actual))
    gain_fit_valid = energy > 0 and actual_energy > 0
    # A zero gain would conceal completely missing audio by also muting the reference.
    gain = float(np.sum(actual * reference) / energy) if gain_fit_valid else 1.0
    expected = reference * gain
    local = []
    for first in range(0, len(actual), BLOCK_FRAMES):
        last = min(first + BLOCK_FRAMES, len(actual))
        local.append({
            "actual_offset_seconds": (actual_offset + first) / RATE,
            "reference_offset_seconds": (reference_offset + first) / RATE,
            "frames": last - first,
            **statistics(np, actual[first:last], expected[first:last]),
        })
    return {
        "delay_frames": lag,
        "delay_ms": 1000 * lag / RATE,
        "delay_at_search_boundary": abs(lag) == MAX_DELAY_FRAMES,
        "gain_applied_to_reference": gain,
        "gain_fit_valid": gain_fit_valid,
        "reference_is_silent": energy == 0,
        "actual_is_silent": actual_energy == 0,
        "compared_frames": len(actual),
        "actual_offset_seconds": actual_offset / RATE,
        "reference_offset_seconds": reference_offset / RATE,
        **statistics(np, actual, expected),
        "local_10ms": local,
        "worst_10ms": sorted(local, key=lambda item: item["error_rms"], reverse=True)[:10],
    }


def read_pcm(np, path, channels):
    frames = SECONDS * RATE
    if not path.is_file() or path.stat().st_size != frames * channels * 8:
        raise DiagnosticError(f"missing or incomplete PCM window: {path}")
    return np.memmap(path, dtype="<f8", mode="r", shape=(frames, channels))


def self_test(np):
    signal = np.random.default_rng(7).standard_normal((RATE, 1)) * 0.05
    # An ordinary mono downmix would cancel this stereo signal completely.
    reference = np.concatenate((signal, -signal), axis=1)
    identity = compare(np, reference, reference)
    assert identity["delay_frames"] == 0 and identity["error_rms"] < 1e-12
    assert abs(identity["gain_applied_to_reference"] - 1) < 1e-12
    for lag, gain in ((37, 0.75), (-53, 1.25)):
        shifted = np.zeros_like(reference)
        if lag > 0:
            shifted[lag:] = reference[:-lag] * gain
        else:
            shifted[:lag] = reference[-lag:] * gain
        fitted = compare(np, shifted, reference)
        assert fitted["delay_frames"] == lag, fitted["delay_frames"]
        assert abs(fitted["gain_applied_to_reference"] - gain) < 1e-10
        assert fitted["error_rms"] < 1e-10
    broken = reference.copy()
    first = RATE // 2
    broken[first:first + round(RATE * 0.005)] = 0
    gap = compare(np, broken, reference)
    assert gap["relative_error_rms"] > 0.01
    assert any(
        abs(block["actual_offset_seconds"] - 0.5) <= 0.01
        and (block["relative_error_rms"] or 0) > 0.2
        for block in gap["worst_10ms"]
    ), "injected 5 ms gap was not localized"
    silence = np.zeros((RATE, 1))
    quiet = compare(np, silence, silence)
    assert quiet["delay_frames"] == 0 and quiet["error_rms"] == 0
    assert quiet["relative_error_rms"] is None
    muted = compare(np, np.zeros_like(reference), reference)
    assert muted["actual_is_silent"] and not muted["gain_fit_valid"]
    assert muted["relative_error_rms"] == 1
    print("Self-test passed: identity, signed delay/gain, antiphase stereo, silence, mute, and 5 ms gap.")


def real_file(np, args):
    path = args.file.expanduser().resolve()
    if not path.is_file():
        raise DiagnosticError(f"input file does not exist: {path}")
    missing = [tool for tool in ("cargo", "ffmpeg", "ffprobe") if shutil.which(tool) is None]
    if missing:
        raise DiagnosticError("required tools not found on PATH: " + ", ".join(missing))
    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S%fZ")
    stem = re.sub(r"[^A-Za-z0-9._-]+", "-", path.stem)[:80] or "dsd"
    directory = args.output.expanduser().resolve() / f"{stem}-{stamp}"
    directory.mkdir(parents=True)
    report = {
        "status": "running", "input": str(path), "created_utc": stamp,
        "positions_seconds": args.positions, "window_seconds": SECONDS, "pcm_rate": RATE,
        "analysis": {
            "filter": "Kaiser windowed-sinc FIR", "taps": FILTER_TAPS,
            "cutoff_hz": FILTER_CUTOFF_HZ, "kaiser_beta": FILTER_BETA,
            "discarded_edge_seconds": GUARD_FRAMES / RATE,
            "max_delay_seconds": MAX_DELAY_FRAMES / RATE,
            "gain_fit": "one least-squares gain applied to reference per window, shared by channels",
            "zero_amplitude_db": None,
            "interpretation": "descriptive comparison; different DSD filters need not be identical",
        },
        "rust_checks_passed": False, "windows": [],
    }
    print(f"Artifacts: {directory}", flush=True)
    try:
        source = probe(path, directory)
        report["source"] = source
        starts = window_starts(args.positions, source["duration_seconds"])
        report["window_starts_seconds"] = starts
        channels = source["channels"]
        rust_checks(path, starts, channels, directory)
        report["rust_checks_passed"] = True
        for start in starts:
            print(f"Comparing {start}–{start + SECONDS}s", flush=True)
            reference = directory / f"ffmpeg-{start}.f64le"
            # Output-side seeking decodes preceding audio, preserving filter history.
            run(
                ["ffmpeg", "-nostdin", "-v", "error", "-i", str(path),
                 "-ss", str(start), "-t", str(SECONDS), "-map", "0:a:0", "-vn",
                 "-ar", str(RATE), "-ac", str(channels), "-c:a", "pcm_f64le",
                 "-f", "f64le", "-y", str(reference)],
                directory / f"ffmpeg-{start}.log",
            )
            actual = directory / f"pcm-{start}.f64le"
            result = compare(np, read_pcm(np, actual, channels), read_pcm(np, reference, channels))
            result.update({
                "start_seconds": start, "mplayer_pcm": actual.name, "ffmpeg_pcm": reference.name,
            })
            report["windows"].append(result)
            error = result["relative_error_db"]
            error_text = f"{error:.2f} dB" if error is not None else "zero error or silent reference"
            print(
                f"  delay {result['delay_ms']:.3f} ms; "
                f"reference gain {result['gain_applied_to_reference']:.8g}; "
                f"relative RMS error {error_text}", flush=True,
            )
        report["status"] = "complete"
    except KeyboardInterrupt:
        report.update(status="interrupted", error="interrupted by user")
        raise
    except Exception as error:
        report.update(status="error", error=str(error))
        raise
    finally:
        destination = directory / "report.json"
        destination.write_text(json.dumps(report, indent=2, allow_nan=False) + "\n", encoding="utf-8")
        print(f"Report: {destination}", flush=True)


def main(argv=None):
    args = arguments(argv)
    try:
        try:
            import numpy as np
        except ImportError as error:
            raise DiagnosticError(
                f"NumPy is required for {sys.executable}. Use a Python environment with NumPy; "
                "this script installs nothing."
            ) from error
        if args.self_test:
            self_test(np)
        else:
            real_file(np, args)
        return 0
    except (DiagnosticError, OSError, ValueError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    except KeyboardInterrupt:
        print("Interrupted.", file=sys.stderr)
        return 130


if __name__ == "__main__":
    sys.exit(main())
