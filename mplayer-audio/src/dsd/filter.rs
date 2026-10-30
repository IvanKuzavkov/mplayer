#![doc = include_str!("../../../docs/FILTER.md")]

use std::{
    f64::consts::PI,
    sync::{Arc, OnceLock},
};

use rayon::{ThreadPool, ThreadPoolBuilder, prelude::*};

use super::PCM_RATE;

// Measured crossover for whole-block jobs; keep small/partial reads sequential.
const PARALLEL_WORK: usize = 1 << 18;
const MIN_PARALLEL_FRAMES: usize = 128;
const AUDIO_TAPS: usize = 97;
const AUDIO_SPAN: usize = 2 * (AUDIO_TAPS - 1) + 1;
const MASK_TAPS: usize = 47;
const MASK_GROUPS: usize = 3;
const AUDIO_CUTOFF: f64 = 27_000.0;
const AUDIO_BETA: f64 = 10.0;
const AUDIO_HALF: usize = AUDIO_TAPS / 2;
const AUDIO_GROUPS: usize = AUDIO_HALF / 4;
pub(super) const WORKER_STACK_BYTES: usize = 256 * 1024;

/// Separate from Rayon's global pool so artwork work cannot occupy these workers.
pub(super) fn pool() -> Option<&'static ThreadPool> {
    static POOL: OnceLock<Option<ThreadPool>> = OnceLock::new();
    POOL.get_or_init(|| {
        let workers = std::thread::available_parallelism().map_or(1, |count| count.get().min(2));
        if workers < 2 {
            return None;
        }
        match ThreadPoolBuilder::new()
            .num_threads(workers)
            .stack_size(WORKER_STACK_BYTES)
            .thread_name(|index| format!("mplayer-dsd-{index}"))
            .build()
        {
            Ok(pool) => Some(pool),
            Err(error) => {
                tracing::warn!(%error, "DSD filter pool unavailable; using sequential filtering");
                None
            }
        }
    })
    .as_ref()
}

/// Packed DSD /8, half-band /2 stages down to 176.4 kHz, then a sparse
/// audio FIR and image-suppression FIR. All stages are centered and linear-phase.
struct Kernels {
    packed: [[f64; 128]; 16],
    packed_center: f64,
    halfband: [[f64; 4]; 2],
    halfband_center: f64,
    audio: [[f64; 4]; AUDIO_GROUPS],
    audio_center: f64,
    mask: [[f64; 4]; MASK_GROUPS],
    mask_center: f64,
}

pub(super) struct Filter {
    pub factor: usize,
    pub half: usize,
    stages: usize,
    kernels: Arc<Kernels>,
}

#[derive(Default)]
struct Job {
    first: Vec<f64>,
    second: Vec<f64>,
}

/// Decoder-owned scratch, reused across reads and seeks. Parallel jobs own
/// disjoint intermediate buffers and share the original packed input window.
#[derive(Default)]
pub(super) struct Scratch {
    jobs: Vec<Job>,
}

#[cfg(test)]
impl Scratch {
    pub fn bytes(&self) -> usize {
        self.jobs.capacity() * std::mem::size_of::<Job>()
            + self
                .jobs
                .iter()
                .map(|job| {
                    (job.first.capacity() + job.second.capacity()) * std::mem::size_of::<f64>()
                })
                .sum::<usize>()
    }
}

fn coefficients(order: usize, cutoff: f64) -> Vec<f64> {
    coefficients_with_window(order, cutoff, |i| {
        let phase = 2.0 * PI * i as f64 / order as f64;
        0.35875 - 0.48829 * phase.cos() + 0.14128 * (2.0 * phase).cos()
            - 0.01168 * (3.0 * phase).cos()
    })
}

// Modified Bessel I0 series; evaluated only once when constructing the kernels.
fn bessel_i0(x: f64) -> f64 {
    let mut term = 1.0;
    let mut sum = 1.0;
    let mut k = 1.0;
    loop {
        term *= (x * x / 4.0) / (k * k);
        sum += term;
        if term <= sum * f64::EPSILON {
            return sum;
        }
        k += 1.0;
    }
}

fn audio_coefficients() -> Vec<f64> {
    let denominator = bessel_i0(AUDIO_BETA);
    coefficients_with_window(
        AUDIO_TAPS - 1,
        AUDIO_CUTOFF / (f64::from(PCM_RATE) / 2.0),
        |i| {
            let x = 2.0 * i as f64 / (AUDIO_TAPS - 1) as f64 - 1.0;
            bessel_i0(AUDIO_BETA * (1.0 - x * x).max(0.0).sqrt()) / denominator
        },
    )
}

fn coefficients_with_window(order: usize, cutoff: f64, window: impl Fn(usize) -> f64) -> Vec<f64> {
    let mut taps = vec![0.0; order + 1];
    for i in 0..=order / 2 {
        let x = i as f64 - (order / 2) as f64;
        let sinc = if x == 0.0 {
            2.0 * cutoff
        } else {
            (2.0 * PI * cutoff * x).sin() / (PI * x)
        };
        // Exact sinc zeros make the packed first bit and half-band zero taps
        // removable. Keep the center coefficient separately in both kernels.
        let value = if x != 0.0 && (2.0 * cutoff * x).fract() == 0.0 {
            0.0
        } else {
            sinc * window(i)
        };
        taps[i] = value;
        taps[order - i] = value;
    }
    let sum: f64 = taps.iter().sum();
    for tap in &mut taps {
        *tap /= sum;
    }
    taps
}

impl Kernels {
    fn new() -> Self {
        let first = coefficients(128, 1.0 / 16.0);
        let half = coefficients(30, 1.0 / 4.0);
        let audio = audio_coefficients();
        let mask = coefficients(MASK_TAPS - 1, 1.0 / 4.0);
        let packed = std::array::from_fn(|group| {
            std::array::from_fn(|byte| {
                (1..8)
                    .map(|bit| {
                        first[group * 8 + bit] * if byte & (0x80 >> bit) == 0 { -1.0 } else { 1.0 }
                    })
                    .sum()
            })
        });
        Self {
            packed,
            packed_center: first[64],
            halfband: std::array::from_fn(|group| {
                std::array::from_fn(|lane| half[(group * 4 + lane) * 2])
            }),
            halfband_center: half[15],
            audio: std::array::from_fn(|group| std::array::from_fn(|lane| audio[group * 4 + lane])),
            audio_center: audio[AUDIO_HALF],
            mask: std::array::from_fn(|group| {
                std::array::from_fn(|lane| mask[(group * 4 + lane) * 2])
            }),
            mask_center: mask[MASK_TAPS / 2],
        }
    }

    fn halfband<const C: usize>(&self, input: &[f64], output: &mut Vec<f64>) {
        output.resize(((input.len() / C - 31) / 2 + 1) * C, 0.0);
        pcm::<C, 31, 2, 2, 2>(input, output, &self.halfband, self.halfband_center);
    }

    fn packed<const C: usize>(&self, bytes: &[u8], output: &mut Vec<f64>) {
        let rows = bytes.as_chunks::<C>().0;
        let frames = rows.len() - 16;
        output.resize(frames * C, 0.0);
        for (frame, out) in output.as_chunks_mut::<C>().0.iter_mut().enumerate() {
            let window = &rows[frame..frame + 17];
            let mut sums = [[0.0; 4]; C];
            for (group, tables) in self.packed.as_chunks::<4>().0.iter().enumerate() {
                for lane in 0..4 {
                    for (channel, sums) in sums.iter_mut().enumerate() {
                        sums[lane] +=
                            tables[lane][usize::from(window[group * 4 + lane][channel] & 0x7f)];
                    }
                }
            }
            for (channel, sample) in out.iter_mut().enumerate() {
                let [a, b, c, d] = sums[channel];
                *sample = ((a + b) + (c + d))
                    + self.packed_center
                        * if window[8][channel] & 0x80 == 0 {
                            -1.0
                        } else {
                            1.0
                        };
            }
        }
    }
}

fn pcm<
    const C: usize,
    const N: usize,
    const STEP: usize,
    const GROUPS: usize,
    const DECIMATION: usize,
>(
    input: &[f64],
    output: &mut [f64],
    side: &[[f64; 4]; GROUPS],
    center: f64,
) {
    let rows = input.as_chunks::<C>().0;
    debug_assert_eq!(output.len() / C, (rows.len() - N) / DECIMATION + 1);
    for (frame, out) in output.as_chunks_mut::<C>().0.iter_mut().enumerate() {
        let window = &rows[frame * DECIMATION..frame * DECIMATION + N];
        let mut sums = [[0.0; 4]; C];
        for (group, taps) in side.iter().enumerate() {
            for lane in 0..4 {
                let index = (group * 4 + lane) * STEP;
                for (channel, sums) in sums.iter_mut().enumerate() {
                    let pair = window[index][channel] + window[N - 1 - index][channel];
                    // Use fused f64 arithmetic only on targets with native FMA support.
                    sums[lane] = if cfg!(any(target_arch = "aarch64", target_feature = "fma")) {
                        taps[lane].mul_add(pair, sums[lane])
                    } else {
                        taps[lane] * pair + sums[lane]
                    };
                }
            }
        }
        for (channel, sample) in out.iter_mut().enumerate() {
            let [a, b, c, d] = sums[channel];
            *sample = ((a + b) + (c + d)) + center * window[N / 2][channel];
        }
    }
}

impl Filter {
    pub fn get(rate: u32) -> Arc<Self> {
        static KERNELS: OnceLock<Arc<Kernels>> = OnceLock::new();
        static FILTERS: [OnceLock<Arc<Filter>>; 3] = [const { OnceLock::new() }; 3];
        let stages = match rate {
            2_822_400 => 0,
            5_644_800 => 1,
            11_289_600 => 2,
            _ => unreachable!("validated DSD rate"),
        };
        Arc::clone(FILTERS[stages].get_or_init(|| {
            let mut half = 2 * AUDIO_HALF + MASK_TAPS / 2;
            for _ in 0..=stages {
                half = half * 2 + 15;
            }
            Arc::new(Self {
                factor: (rate / PCM_RATE) as usize,
                half: half * 8 + 64,
                stages: stages + 1,
                kernels: Arc::clone(KERNELS.get_or_init(|| Arc::new(Kernels::new()))),
            })
        }))
    }

    pub fn process(
        &self,
        bytes: &[u8],
        channels: usize,
        output: &mut [f64],
        scratch: &mut Scratch,
    ) {
        self.process_at_threshold(bytes, channels, output, scratch, PARALLEL_WORK);
    }

    pub(super) fn process_at_threshold(
        &self,
        bytes: &[u8],
        channels: usize,
        output: &mut [f64],
        scratch: &mut Scratch,
        threshold: usize,
    ) {
        // Count packed lookups and symmetric PCM products per output sample.
        let mut work = 16 * (self.factor / 8) + AUDIO_HALF + 1 + MASK_GROUPS * 4 + 1;
        for stage in 0..self.stages {
            work += 9 * ((self.factor / 8) >> (stage + 1));
        }
        let pool = if output.len() * work >= threshold
            && output.len() / channels >= MIN_PARALLEL_FRAMES * 2
        {
            pool()
        } else {
            None
        };
        self.process_with_pool(bytes, channels, output, scratch, pool);
    }

    fn process_with_pool(
        &self,
        bytes: &[u8],
        channels: usize,
        output: &mut [f64],
        scratch: &mut Scratch,
        pool: Option<&ThreadPool>,
    ) {
        match channels {
            1 => self.process_channels::<1>(bytes, output, scratch, pool),
            2 => self.process_channels::<2>(bytes, output, scratch, pool),
            _ => unreachable!("validated DSD channel count"),
        }
    }

    fn process_channels<const C: usize>(
        &self,
        bytes: &[u8],
        output: &mut [f64],
        scratch: &mut Scratch,
        pool: Option<&ThreadPool>,
    ) {
        if output.is_empty() {
            return;
        }
        let jobs = if pool.is_some() { 2 } else { 1 };
        while scratch.jobs.len() < jobs {
            scratch.jobs.push(Job::default());
        }
        if let Some(pool) = pool {
            let chunk_frames = (output.len() / C).div_ceil(2);
            pool.install(|| {
                output
                    .par_chunks_mut(chunk_frames * C)
                    .zip(scratch.jobs[..jobs].par_iter_mut())
                    .enumerate()
                    .for_each(|(index, (out, job))| {
                        let start = index * chunk_frames * (self.factor / 8) * C;
                        let length =
                            ((out.len() / C - 1) * self.factor / 8 + self.half / 4 + 1) * C;
                        self.job::<C>(&bytes[start..start + length], out, job);
                    });
            });
        } else {
            self.job::<C>(bytes, output, &mut scratch.jobs[0]);
        }
    }

    fn job<const C: usize>(&self, bytes: &[u8], output: &mut [f64], job: &mut Job) {
        self.kernels.packed::<C>(bytes, &mut job.first);
        self.kernels.halfband::<C>(&job.first, &mut job.second);
        if self.stages >= 2 {
            self.kernels.halfband::<C>(&job.second, &mut job.first);
        }
        if self.stages == 3 {
            self.kernels.halfband::<C>(&job.first, &mut job.second);
        }
        let (input, temporary) = if self.stages.is_multiple_of(2) {
            (&job.first, &mut job.second)
        } else {
            (&job.second, &mut job.first)
        };
        temporary.resize((output.len() / C + MASK_TAPS - 1) * C, 0.0);
        pcm::<C, AUDIO_SPAN, 2, AUDIO_GROUPS, 1>(
            input,
            temporary,
            &self.kernels.audio,
            self.kernels.audio_center,
        );
        pcm::<C, MASK_TAPS, 2, MASK_GROUPS, 1>(
            temporary,
            output,
            &self.kernels.mask,
            self.kernels.mask_center,
        );
    }

    #[cfg(test)]
    pub(super) fn process_sequential(
        &self,
        bytes: &[u8],
        channels: usize,
        output: &mut [f64],
        scratch: &mut Scratch,
    ) {
        self.process_with_pool(bytes, channels, output, scratch, None);
    }

    #[cfg(test)]
    pub fn table_bytes(&self) -> usize {
        std::mem::size_of::<Kernels>()
    }
}

#[cfg(test)]
#[path = "../../tests/unit/dsd_filter.rs"]
mod tests;
