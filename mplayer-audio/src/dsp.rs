use anyhow::{anyhow, bail};
use mplayer_core::Result;
use rubato::{Fft, FixedSync, Resampler, audioadapter_buffers::direct::InterleavedSlice};

struct RubatoResampler {
    resampler: Fft<f64>,
    pending: Vec<f64>,
    output: Vec<f64>,
    delay: usize,
    output_channel_count: usize,
}

/// Streaming conversion with reusable buffers and explicit resampler delay/tail accounting.
pub(crate) struct Converter {
    input_rate: u32,
    output_rate: u32,
    input_channels: usize,
    output_channels: usize,
    resampler: Option<RubatoResampler>,
    received_frames: u64,
    emitted_frames: u64,
    finished: bool,
}

impl RubatoResampler {
    pub fn new(input_rate: usize, output_rate: usize, channel_num: usize) -> Result<Self> {
        let resampler =
            Fft::<f64>::new(input_rate, output_rate, 1024, channel_num, FixedSync::Input)?;
        let input_size = resampler
            .input_frames_max()
            .checked_mul(channel_num)
            .ok_or(anyhow!("Resampler input too large"))?;
        let output_size = resampler
            .output_frames_max()
            .checked_mul(channel_num)
            .ok_or(anyhow!("Resampler output too large"))?;
        if input_size > 2_000_000 || output_size > 2_000_000 {
            bail!("Unsupported sample-rate conversion ratio");
        }
        Ok(Self {
            pending: Vec::with_capacity(input_size),
            delay: resampler.output_delay(),
            resampler,
            output: vec![0.0; output_size],
            output_channel_count: channel_num,
        })
    }

    pub fn convert(
        &mut self,
        samples: &[f64],
        target_frames: u64,
        input_ch_count: usize,
        eof: bool,
        destination: &mut Vec<f64>,
    ) -> Result<u64> {
        if input_ch_count == self.output_channel_count {
            self.pending.extend_from_slice(samples);
        } else {
            for &sample in samples {
                self.pending.extend_from_slice(&[sample, sample]);
            }
        }
        let mut produced_frames = 0;
        loop {
            let needed = self.resampler.input_frames_next();
            if self.pending.len() < needed * self.output_channel_count && !eof {
                break Ok(produced_frames);
            }
            if eof && self.pending.is_empty() && produced_frames >= target_frames {
                break Ok(produced_frames);
            }
            let samples_needed = needed * self.output_channel_count;
            if self.pending.len() < samples_needed {
                // Only EOF permits a partial block; pad it to flush the filter tail.
                self.pending.resize(samples_needed, 0.0);
            }
            let input = InterleavedSlice::new(
                &self.pending[..samples_needed],
                self.output_channel_count,
                needed,
            )?;
            let output_frames = self.output.len() / self.output_channel_count;
            let mut output = InterleavedSlice::new_mut(
                &mut self.output,
                self.output_channel_count,
                output_frames,
            )?;
            let (_, produced) = self
                .resampler
                .process_into_buffer(&input, &mut output, None)?;
            self.pending.drain(..samples_needed);
            let skip = self.delay.min(produced);
            self.delay -= skip;
            let frames =
                (produced - skip).min(target_frames.saturating_sub(produced_frames) as usize);
            destination.extend_from_slice(
                &self.output
                    [skip * self.output_channel_count..(skip + frames) * self.output_channel_count],
            );
            produced_frames += frames as u64;
        }
    }
}

impl Converter {
    pub fn new(
        input_rate: u32,
        output_rate: u32,
        input_channels: usize,
        output_channels: usize,
    ) -> Result<Self> {
        if input_rate == 0 || output_rate == 0 || input_rate > 768_000 || output_rate > 768_000 {
            bail!("Unsupported sample rate (must be 1..=768000 Hz)");
        }
        if !(input_channels == output_channels || (input_channels == 1 && output_channels == 2))
            || !(1..=2).contains(&input_channels)
            || !(1..=2).contains(&output_channels)
        {
            bail!("Only mono and stereo playback is supported");
        }
        // Rubato allocates its FFT and scratch storage in the constructor. Bound the
        // GCD-derived FFT sizes before calling it, including unusual coprime rates.
        let mut divisor = input_rate;
        let mut remainder = output_rate;
        while remainder != 0 {
            (divisor, remainder) = (remainder, divisor % remainder);
        }
        let minimum_input = input_rate / divisor;
        let minimum_output = output_rate / divisor;
        let subchunks = 256_u32.div_ceil(minimum_input);
        if input_rate.max(output_rate) > input_rate.min(output_rate).saturating_mul(96)
            || subchunks.saturating_mul(minimum_input) > 65_536
            || subchunks.saturating_mul(minimum_output) > 65_536
        {
            bail!("Unsupported conversion ratio or FFT size; choose a closer output rate");
        }
        let resampler = if input_rate != output_rate {
            Some(RubatoResampler::new(
                input_rate as usize,
                output_rate as usize,
                output_channels,
            )?)
        } else {
            None
        };
        Ok(Self {
            input_rate,
            output_rate,
            input_channels,
            output_channels,
            resampler,
            received_frames: 0,
            emitted_frames: 0,
            finished: false,
        })
    }

    pub fn convert(
        &mut self,
        samples: &[f64],
        eof: bool,
        destination: &mut Vec<f64>,
    ) -> Result<()> {
        destination.clear();
        if self.finished {
            return Ok(());
        }
        if !samples.len().is_multiple_of(self.input_channels) {
            bail!("Incomplete input PCM frame");
        }
        if samples.len() / self.input_channels > 1024 {
            bail!("PCM conversion input must be at most 1024 frames");
        }
        self.received_frames += (samples.len() / self.input_channels) as u64;
        let target_frames = self.target_frames()? - self.emitted_frames;
        if let Some(resampler) = self.resampler.as_mut() {
            self.emitted_frames += resampler.convert(
                samples,
                target_frames,
                self.input_channels,
                eof,
                destination,
            )?;
        } else {
            if self.input_channels == self.output_channels {
                destination.extend_from_slice(samples);
            } else {
                for &sample in samples {
                    destination.extend_from_slice(&[sample, sample]);
                }
            }
            self.emitted_frames += (destination.len() / self.output_channels) as u64;
        }

        self.finished = eof;
        Ok(())
    }

    pub fn target_frames(&self) -> Result<u64> {
        let frames = (u128::from(self.received_frames) * u128::from(self.output_rate)
            + u128::from(self.input_rate) / 2)
            / u128::from(self.input_rate);
        u64::try_from(frames).map_err(|_| anyhow!("Audio frame count overflow"))
    }
}

#[cfg(test)]
#[path = "../tests/unit/dsp.rs"]
mod tests;
