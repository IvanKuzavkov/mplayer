use cpal::SampleFormat;
use symphonia::core::audio::GenericAudioBufferRef;

/// Precision of the decoder's normalized PCM, independent of the file container.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PcmFormat {
    Integer(u32),
    Float(u32),
}

impl PcmFormat {
    pub fn decoded(audio: &GenericAudioBufferRef<'_>, bits: Option<u32>) -> Self {
        use GenericAudioBufferRef::*;
        let width = match audio {
            F32(_) => return Self::Float(32),
            F64(_) => return Self::Float(64),
            U8(_) | S8(_) => 8,
            U16(_) | S16(_) => 16,
            U24(_) | S24(_) => 24,
            U32(_) | S32(_) => 32,
        };
        Self::Integer(
            bits.filter(|&bits| bits > 0 && bits <= width)
                .unwrap_or(width),
        )
    }

    pub fn preserved_by(self, output: SampleFormat) -> bool {
        match (self, output) {
            (Self::Integer(bits), SampleFormat::F32) => bits <= 24,
            (Self::Integer(_), SampleFormat::F64) => true,
            (Self::Float(bits), SampleFormat::F32) => bits <= 32,
            (Self::Float(_), SampleFormat::F64) => true,
            (Self::Integer(bits), output) if output.is_int() || output.is_uint() => {
                bits <= output.bits_per_sample()
            }
            _ => false,
        }
    }

    pub fn preference(self, output: SampleFormat) -> (bool, u8) {
        if !self.preserved_by(output) {
            return (
                true,
                match output {
                    SampleFormat::F32 => 0,
                    SampleFormat::I32 | SampleFormat::U32 => 1,
                    SampleFormat::I24 => 2,
                    SampleFormat::I16 | SampleFormat::U16 => 3,
                    _ => 4,
                },
            );
        }
        let rank = match (self, output) {
            (Self::Integer(1..=24) | Self::Float(32), SampleFormat::F32)
            | (Self::Integer(25..=32), SampleFormat::I32)
            | (Self::Float(64), SampleFormat::F64) => 0,
            (_, SampleFormat::F64) => 1,
            (_, SampleFormat::I32) => 2,
            (_, SampleFormat::I24) => 3,
            (_, SampleFormat::I16) => 4,
            (_, SampleFormat::I64) => 5,
            (_, SampleFormat::U32) => 6,
            (_, SampleFormat::U16) => 7,
            (_, SampleFormat::U64) => 8,
            (_, SampleFormat::I8) => 9,
            (_, SampleFormat::U8) => 10,
            _ => 11,
        };
        (false, rank)
    }
}

#[cfg(test)]
#[path = "../tests/unit/pcm.rs"]
mod tests;
