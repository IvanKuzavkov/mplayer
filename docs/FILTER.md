# DSD filter design and mathematics

This document explains the implementation in `filter.rs` and its integration
with `DsdDecoder` in `mod.rs`. It covers why the design changed, how the current
filters work, and the timing and numerical properties that must be preserved.
The project's `VALIDATION.md` records benchmark methodology and results.

## How the design evolved

The first implementation used one long FIR directly on packed DSD. For total
decimation `D`, its length was `128D + 1`: 2,049 taps for DSD64, 4,097 for DSD128,
and 8,193 for DSD256. Byte lookup tables replaced per-bit multiplication, but
the tables still occupied 512 KiB, 1 MiB, and 2 MiB respectively. Evaluating each
output sample required walking many table rows.

Four independent accumulators, processing stereo channels together, and a
two-worker Rayon pool improved throughput. Optimizing the audio crate in the
development profile also fixed debug builds running below realtime. The long
FIR and its large lookup tables nevertheless remained expensive in release.

The next change split the conversion into stages. A small packed-bit filter
reduces the rate first; short half-band filters reduce it further; the final
audio filter runs at only 352.8 kHz. This keeps the demanding audio-band filtering
away from the original multi-megahertz stream. Multistage conversion is a standard
way to reduce rate-conversion work and storage; the actual benefit depends on
the chosen filters and implementation. See [MathWorks' multistage conversion
example](https://www.mathworks.com/help/dsp/ug/multistage-rate-conversion.html).

The prototype used a 257-tap final filter and 1,024-frame batches. Larger
4,096-frame batches amortized dispatch and overlapping source windows. The final
filter was then shortened to 161 taps after checking the complete cascade's
response. Arithmetic remains `f64`; the optimization changes filter structure
and coefficients rather than reducing numeric precision.

On the recorded stereo DSD128 excerpt, release throughput increased from about
23 times realtime to 150 times realtime. That earlier kernel occupied 17,112 bytes
shared by all three rates, instead of retaining 3.5 MiB of separate tables.
These are local measurements, not guarantees for every machine or file.

A subsequent 20–24 kHz revision used a 665-tap Blackman–Harris final FIR.
The next revision widened the transition to 24–30 kHz and used a 385-tap
Kaiser FIR at 352.8 kHz. It met the response requirements but was slower than
the original 161-tap multistage filter.

The current design moves the last half-band decimation before the audio FIR.
At 176.4 kHz it uses an **interpolated FIR (IFIR)**: a 97-tap Kaiser prototype
with its coefficients spaced two samples apart, followed by a 47-tap
Blackman–Harris half-band image-suppression filter. The sparse prototype sets
the sharp 24–30 kHz transition; the mask removes the extra passband created by
spacing the coefficients. Both run at 176.4 kHz without changing the output
sample rate. They require 49 + 13 = 62 symmetric products per output sample
and channel, instead of 193 for the 385-tap filter. The extra earlier half-band
stage adds nine products at this rate. All arithmetic remains f64 and all
stages remain centered and linear-phase. Native FMA is used on AArch64 and
targets built with FMA support; other targets retain multiplication and addition.
See [MathWorks' IFIR definition](https://www.mathworks.com/help/dsp/ref/ifir.html)
for the periodic model plus image-suppressor construction. These windowed
coefficients are not MATLAB's minimax IFIR design.

Full-cascade tests retain ±0.01 dB through 24 kHz and at least 90 dB rejection
from 30 kHz through each original source's Nyquist frequency. In the sampled
audio-band graph, maximum passband deviation is below 0.0002 dB and maximum
stopband gain is about −99.5 dB. A direct 193-tap lower-rate filter and a
stereo accumulator layout change were also measured; the IFIR design with
native FMA was faster.

Matched local generated-file benchmarks used the same 1-second fixtures,
4,096-frame reads, development profile (audio crate optimized), six measured
trials after warmup, median wall time, and the production parallel threshold.
Two runs of the final design gave 135.3× and 136.0× for stereo DSD128:

| Stereo source | Original 161-tap | Previous 385-tap | Current IFIR, repeat run |
| --- | --- | --- | --- |
| DSD64 | 190.5× (5.249 ms) | 114.4× (8.744 ms) | 210.8× (4.743 ms) |
| DSD128 | 129.0× (7.750 ms) | 87.8× (11.389 ms) | 135.3× (7.394 ms) |
| DSD256 | 78.1× (12.812 ms) | 60.2× (16.604 ms) | 80.1× (12.481 ms) |

This is approximately 5% more DSD128 throughput than the matched original,
not a large speedup. The separate repeated-frame DST64 stereo fixture measured
2.88× versus 3.09× originally (about 7% slower); the larger overlap windows can
increase source-reader work. These generated decoding/filtering benchmarks
exclude device conversion, callbacks, and physical output. They are not
release-mode or hardware playback guarantees. Current shared kernels occupy
16,960 bytes. Both measured parallel thresholds still pass the existing
crossover criteria. Re-run `dsd_throughput` when changing the design.

The new cascade meets the original response limits, but it has a different
impulse response. Its PCM values are not expected to match the old FIR sample
for sample. DSD-to-PCM conversion also does not preserve the original DSD
bitstream for native DSD/DoP playback.

## Signal representation and rate changes

The container reader supplies channel-interleaved bytes with the earliest bit
in the most significant position. DSF block layout and bit order, DFF layout,
and DST decompression are resolved before filtering. A DSD bit is mapped to a
normalized bipolar input value:

```text
b[n] ∈ {0, 1}
x[n] = 2b[n] − 1 ∈ {−1, +1}
```

Filtering recovers the audio-band component of this stream and suppresses
out-of-band energy before it can fold into the final audio band. No additional
gain boost is applied; each filter is normalized to unity DC gain.

Every supported source ends at 176,400 PCM frames per second:

| Source | DSD rate | After packed FIR /8 | After half-band stages /2 | After audio IFIR (no rate change) | Total D |
| --- | --- | --- | --- | --- | --- |
| DSD64 | 2,822,400 Hz | 352,800 Hz | 176,400 Hz | 176,400 Hz | 16 |
| DSD128 | 5,644,800 Hz | 705,600 Hz | 352,800 → 176,400 Hz | 176,400 Hz | 32 |
| DSD256 | 11,289,600 Hz | 1,411,200 Hz | 705,600 → 352,800 → 176,400 Hz | 176,400 Hz | 64 |

Let `s = 0, 1, 2` index DSD64/128/256. There are `s + 1` half-band
stages, and total decimation remains `D = 16 × 2^s`.
The device converter can subsequently resample this PCM to a supported output
rate, such as 96 kHz. Source metadata continues to describe the original DSD.

## FIR and decimation equations

For an odd-length FIR with `N = 2M + 1` taps, ordinary causal filtering followed
by decimation by `d` is:

```text
u[n] = Σ(i=0..2M) h[i] x[n − i]
y[k] = u[kd]
```

A symmetric FIR, `h[i] = h[2M − i]`, has linear phase and a group delay of `M`
input samples. This implementation reads enough input on both sides of a
requested position and evaluates the centered form:

```text
y[k] = Σ(i=0..2M) h[i] x[kd + i − M]
```

The result is aligned with input sample `kd`. The reader obtains future input
as lookahead before producing that result. Each stage computes the output
positions that will be retained after decimation.

The earlier long FIR already evaluated only retained positions. Merely renaming
it a polyphase filter would not remove another factor of `D` from its work. The
gain here comes from shorter stage filters, zero taps, symmetry, smaller tables,
and doing later work at reduced rates.

## Coefficient construction

`coefficients_with_window(order, cutoff, window)` takes the even order `Q = N − 1` and a normalized
cutoff `fc`, measured in cycles per input sample. For centered coordinate
`t = i − Q/2`, the ideal low-pass impulse response is:

```text
g[i] = 2fc                              when t = 0
g[i] = sin(2πfc t) / (πt)               otherwise
```

For the packed and half-band stages, `coefficients` multiplies it by the
symmetric four-term Blackman–Harris window:

```text
φ = 2πi / Q
w[i] = 0.35875 − 0.48829 cos(φ)
                 + 0.14128 cos(2φ) − 0.01168 cos(3φ)

v[i] = g[i] w[i]
h[i] = v[i] / Σ(j=0..Q) v[j]
```

The normalization gives `Σh[i] = 1`. The code computes one half and mirrors it
to make symmetry exact in storage. The window definition and constants are
documented in [MathWorks' Blackman–Harris
reference](https://www.mathworks.com/help/signal/ref/blackmanharris.html).

For the sparse audio prototype, `audio_coefficients` uses a symmetric Kaiser window:

```text
u = 2i/Q − 1
w[i] = I0(beta sqrt(1 − u²)) / I0(beta), beta = 10
I0(z) = Σ(k=0..∞) (z²/4)^k / (k!)²
```

The same sinc construction, normalization, and exact mirroring apply.
`bessel_i0` evaluates the series only when the shared kernels are initialized,
stopping when the next term is at most machine epsilon times the sum.
No Bessel or window evaluation occurs during playback. See the
[SciPy Kaiser window definition](https://docs.scipy.org/doc/scipy/reference/generated/scipy.signal.windows.kaiser.html).

At noncentral integer zeros of the sinc, the code stores exactly `0.0` rather
than retaining a small residual from evaluating `sin(kπ)`. The first two stages
use binary-exact cutoffs, so these structural zeros can be identified exactly.

| Stage | Taps | Normalized cutoff | Purpose |
| --- | --- | --- | --- |
| Packed DSD | 129 | `1/16` of its input rate | First reduction by eight |
| Half-band | 31 | `1/4` of its input rate | Additional reduction by two |
| Audio prototype | 97 | `27,000 / 88,200` | Sparse audio low-pass, expanded to a 193-sample span at 176.4 kHz |
| Image mask | 47 | `1/4` at 176.4 kHz | Suppress the sparse prototype’s extra passband |

The 27 kHz parameter places the ideal sinc edge inside the specified 24–30 kHz
transition band. Acceptance uses the complete measured response, rather than
treating the cutoff parameter as a passband-edge or brick-wall specification.

## Packed-bit lookup tables

For the first filter, `M = 64` and `fc = 1/16`. Its sinc numerator is
`sin(π(i − 64)/8)`. Consequently, every tap at a multiple of eight is zero,
except the center tap `h[64]`.

Group the coefficients by source byte. For `g = 0..15`, precompute:

```text
T_g[v] = Σ(i=1..7) h[8g + i] × (2 bit_i(v) − 1)
bit_i(v) = (v >> (7 − i)) & 1
v = 0..127
```

The byte's MSB corresponds to the omitted zero tap, so only seven bits are
needed to index each table. The center byte's MSB is handled separately.
For a source window starting at byte `B[j]`, the first-stage result is:

```text
r[j] = Σ(g=0..15) T_g[B[j + g] & 0x7f]
       + h[64] × (2 MSB(B[j + 8]) − 1)
```

Its center is the MSB of byte `B[j + 8]`. A 17-byte formal window covers all
129 taps; the last tap is zero. Advancing the window by one byte advances eight
DSD samples, so this produces the /8-decimated stream.

This replaces repeated bit extraction and multiplication with 16 lookup
operations and one center contribution per channel per intermediate sample.
The tables use `16 × 128 × 8 = 16,384` bytes. Adding the stored PCM coefficients
and center coefficients gives the complete 16,960-byte `Kernels` object.

## Half-band zeros and symmetric PCM evaluation

For a 31-tap half-band filter, `M = 15` and `fc = 1/4`. The sinc is zero at
nonzero even offsets from the center. Thus odd coefficient indices are zero,
apart from index 15. The stored left-side indices are `0, 2, …, 14`.

For either PCM filter, symmetry rewrites the centered convolution as:

```text
y[k] = h[M] x[kd]
       + Σ(i=0..M−1) h[i] × (x[kd + i − M] + x[kd + M − i])
```

Each symmetric pair needs one multiplication. The half-band stage needs eight
pair products plus its center: nine products per channel per output sample.
The sparse 97-tap model needs 48 pair products plus its center: 49 products.
Its 193-sample expanded span skips every odd coefficient index. The 47-tap
mask needs 12 pair products plus its center: 13 products. Like the 31-tap
half-band, its odd indices are zero except for its center at index 23.
The normalized center coefficient is stored explicitly; it is not assumed to
be exactly `0.5` for the finite, normalized half-band filter.

`pcm<C, N, STEP, GROUPS, DECIMATION>` specializes this operation:

- `C` is one or two channels.
- `N` is the full input span: 31, 193, or 47 samples.
- `STEP` is two to skip half-band zeros or inserted prototype zeros.
- `GROUPS` is the number of groups of four stored left-side coefficients.
- `DECIMATION` is two for the early half-bands and one for the model and mask.

Four independent accumulators shorten the addition dependency chain. Stereo
channels are evaluated while the same coefficients are available. Each result
uses the fixed sum `(a + b) + (c + d)`, then adds the center contribution.
FMA rounds each accumulated product once on supported targets; portable
multiplication/addition may differ in the last bits. Sequential and parallel
paths use the same target-specific arithmetic.

## Equivalent filter and alias rejection

Let `h0`, `hb`, `ha`, and `hm` denote the packed, 31-tap half-band,
97-tap prototype, and 47-tap mask FIRs.
Let `up_q(h)` insert `q − 1` zeros between coefficients, and `*` denote
convolution. The equivalent FIR at the original DSD rate is:

```text
DSD64:  h_eq = h0 * up_8(hb) * up_32(ha) * up_16(hm)
DSD128: h_eq = h0 * up_8(hb) * up_16(hb) * up_64(ha) * up_32(hm)
DSD256: h_eq = h0 * up_8(hb) * up_16(hb) * up_32(hb) * up_128(ha) * up_64(hm)
```

The production code evaluates the short stages rather than constructing this
large array. Tests construct it as an independent reference for the staged
evaluation, including its alignment and gain.

For a symmetric FIR at rate `Fs`, its real, centered frequency response is:

```text
A(f; Fs) = h[M] + 2 Σ(i=0..M−1) h[i] cos(2πf(M − i) / Fs)
```

The cascade's response to an original-source sinusoid is the product of the
stage responses evaluated at that frequency using each stage's input rate:

```text
A_eq(f) = A0(f; Fs_DSD)
          × Π(j=0..s) Ab(f; Fs_DSD / (8 × 2^j))
          × Aa(f; 88,200) × Am(f; 176,400)
```

These responses are periodic in their respective sample rates. Evaluating the
product at original-source frequencies therefore follows their folding through
the cascade. An input frequency `f` appears at the final output modulo 176.4 kHz,
with real-signal folding into its 0–88.2 kHz Nyquist interval.

Early stages have wide transitions. They must sufficiently suppress components
that would fold into the protected final band; later stages suppress remaining
components before further decimation. This is why validation considers the
whole cascade, including original-source frequencies well above the final
Nyquist frequency.

The response tests require:

```text
0 ≤ f ≤ 24,000 Hz:       |20 log10(|A_eq(f)|)| ≤ 0.01 dB
30,000 Hz ≤ f ≤ Fs_DSD/2: 20 log10(|A_eq(f)|) ≤ −90 dB
```

The stopband limit corresponds to amplitude gain at most
`10^(−90/20) ≈ 3.16 × 10^−5`. Tests sample the passband every 100 Hz,
30–100 kHz every 100 Hz, and the remaining source-frequency range every 500 Hz.
This is numerical response verification on a grid, not a formal continuous-band
proof or a signal-quality ranking against another library.

## Timing, source windows, seeking, and EOF

The equivalent FIR's radius `R`, measured in original DSD samples, adds the
stage delays after converting them to original-rate units:

```text
R = 64 + 8 × [15 × (2^(s+1) − 1) + 119 × 2^(s+1)]
N_eq = 2R + 1
```

`Filter::get` computes the same quantity backwards: start with the final
IFIR radius `48 × 2 + 23 = 119`; for each of the `s + 1` half-band stages apply `half = 2half + 15`;
then convert through the /8 stage and add its radius: `R = 8half + 64`.

| Source | R in DSD samples | R in source bytes/channel | Equivalent taps | Lookahead |
| --- | --- | --- | --- | --- |
| DSD64 | 2,088 | 261 | 4,177 | 0.740 ms |
| DSD128 | 4,232 | 529 | 8,465 | 0.750 ms |
| DSD256 | 8,520 | 1,065 | 17,041 | 0.755 ms |

For `F` output frames beginning at absolute PCM frame `K`, the source window
starts at bit `KD − R`. Both `D` and `R` are multiples of eight, so the start is
byte-aligned. The number of byte rows, each containing one byte per channel, is:

```text
B = ((F − 1)D + 2R) / 8 + 1
window_bytes = B × channels
```

This explains the `half / 4` expression in sub-job sizing: it represents the
full overlap width `2R / 8` in source bytes per channel.

The packed stage produces `B − 16` frames. Each PCM /2 stage with `N` taps
produces `floor((L − N)/2) + 1` frames from `L` input frames. With the source
window above, the early stages leave `F + 238` frames at 176.4 kHz.
The sparse model uses a 193-sample span and yields `F + 46` frames; the
47-tap mask yields exactly `F` frames. Both use stride one, including short batches.

At physical file boundaries the reader fills missing input with balanced
`0x55` DSD padding. Unused bits in the final byte are masked to the same pattern;
DSF storage padding is not played as audio. A source with `S` valid DSD samples
has `ceil(S/D)` PCM frames, whose centers are `0, D, 2D, … < S`. Lookahead padding
supports filtering these frames without extending the track's duration.

For a source segment `[a, b)`, the corresponding output centers are selected by
the PCM range `[ceil(a/D), ceil(b/D))`. Seeking rebuilds a window around the
absolute requested center. Scratch buffers hold intermediate results, not
causal filter history, so a seek does not require processing from the beginning
of the file. Filtering can use physical source samples outside a logical CUE
segment to preserve continuity at its boundaries.

## Batching, parallelism, and storage

The decoder processes up to 4,096 PCM frames per read. `Session::fill` still
passes at most 1,024 frames per call to the device converter. Each parallel job
gets its own complete source overlap, shared immutable coefficients/input, and
private reusable `Job` buffers. Splitting at an output-frame boundary preserves
all stage phases because the source offset is a multiple of `D`.

A dedicated pool has at most two workers, capped by available CPU parallelism,
with 256 KiB of requested stack per worker. Small jobs run sequentially; pool
creation failure also uses the sequential path. The callback does not run these
filters or perform their allocations.

The dispatch estimate per output sample/channel is:

```text
W = 16(D/8) + 49 + 13 + 9 Σ(j=0..s) (D / (8 × 2^(j+1)))
```

This counts packed lookups and symmetric PCM products, giving `W = 103, 153,
253` for the three rates. It is a scheduling heuristic, not a cycle count; it
omits overlap work and treats different operations equally. Parallel dispatch
requires `F × channels × W ≥ 2^18` and at least 256 output frames. The frame range
is split approximately in half, giving each job at least 128 frames. Both jobs
finish before the decoder advances its position.

Each frame uses the same arithmetic order regardless of which worker computes
it. Parallel output must therefore match sequential output bit-for-bit. The
equivalent full-rate reference can use a different summation order, so its
comparison allows an absolute normalized error of `1e-12`.

## Code map and maintenance checks

| Code | Responsibility |
| --- | --- |
| `Reader::read` | Canonical packed DSD bytes after container handling |
| `coefficients` / `audio_coefficients` | Blackman–Harris / Kaiser windowed sinc, exact zeros, symmetry, DC normalization |
| `Kernels::new` / `Kernels::packed` | Shared tables and the first /8 stage |
| `pcm` | Symmetric FIR with sparse coefficient spacing and configurable decimation |
| `Filter::get` | Rate-specific stage count, decimation factor, and source radius |
| `Filter::process_at_threshold` / `process_channels` | Dispatch and overlapping job windows |
| `Scratch` / `Job` | Decoder-owned reusable intermediate buffers |
| `DsdDecoder::next_with` | Physical source windows, boundary padding, output accounting |

When changing the design, preserve the assumptions behind packing and storage:

- The 129-tap first stage's cutoff gives its exact every-eighth-tap zeros.
- The 31-tap half-band layout gives eight nonzero left-side coefficients.
- The 47-tap mask gives 12 nonzero left-side coefficients; the sparse model
  gives 48, spaced two samples apart across its 193-sample span.
- The audio FIR has odd length and a half-length divisible by four, matching
  coefficient grouping. Radius and equivalent-filter calculations must agree.
- Complete-cascade response checks must cover folding bands at every supported
  source rate; testing only the final audio FIR is insufficient.
- Keep equivalent-FIR, sequential/parallel, constant/silence, tone, container,
  seek/CUE, and EOF/frame-count tests passing. Benchmark both the kernel and the
  full decode/resample path, including its 1,024-frame converter slices.

Use `dsd_throughput` for generated workloads and dispatch comparisons, and
`dsd_file_throughput` with `MPLAYER_DSD_BENCH_FILE` for a real DSF/DFF. See
`VALIDATION.md` for commands, measured results, and the limits of those tests.
