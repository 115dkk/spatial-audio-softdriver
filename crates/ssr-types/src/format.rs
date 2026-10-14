//! Source formats: what a source writes to the virtual device.
//!
//! The driver passes the format it negotiated with Windows through untouched
//! (a WAVEFORMATEXTENSIBLE in a format-change record); `ssr_abi` parses it into
//! a [`SourceFormat`] and the engine picks a decoder adapter from that.

/// How PCM samples are stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SampleType {
    /// Little-endian signed integer, `bits_per_sample` wide in the container.
    Int,
    /// Little-endian IEEE float (32 bits).
    Float,
}

/// Interleaved PCM as Windows' mixer writes it with spatial sound off.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PcmFormat {
    pub channels: u16,
    pub sample_rate: u32,
    /// Container width per sample (16, 24 or 32).
    pub bits_per_sample: u16,
    /// Significant bits within the container.
    pub valid_bits: u16,
    pub sample_type: SampleType,
    /// `dwChannelMask` (`SPEAKER_FRONT_LEFT` = bit 0, ...); 0 when the source
    /// gave none, in which case the standard order for `channels` applies.
    pub channel_mask: u32,
}

impl PcmFormat {
    pub fn bytes_per_frame(&self) -> usize {
        usize::from(self.channels) * usize::from(self.bits_per_sample / 8)
    }
}

/// The codec an IEC 61937 stream announces through its KS subtype.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BitstreamCodec {
    Ac3,
    Eac3,
    Eac3Joc,
    /// Dolby MLP, the TrueHD bitstream.
    TrueHd,
    Mat20,
    /// MAT 2.1, what Windows spatial sound's "Dolby Atmos for home theater" writes.
    Mat21,
    Dts,
    DtsHd,
    /// A subtype the driver compiled in but no adapter knows; carries the GUID's
    /// first field so the status screen can name it.
    Other(u32),
}

/// An IEC 61937 stream: the container rate and channel count the burst stream
/// rides in (2 ch / 48 kHz for AC-3, 2 ch / 192 kHz for DD+, 8 ch / 192 kHz
/// "HBR" for MAT and TrueHD).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BitstreamFormat {
    pub codec: BitstreamCodec,
    pub container_rate: u32,
    pub container_channels: u16,
    /// `dwEncodedSamplesPerSec` from WAVEFORMATEXTENSIBLE_IEC61937, 0 if absent.
    pub encoded_rate: u32,
    /// `dwEncodedChannelCount`, 0 if absent.
    pub encoded_channels: u32,
}

impl BitstreamFormat {
    /// Bytes per second the container carries (16-bit words on every channel).
    pub fn container_bytes_per_second(&self) -> usize {
        self.container_rate as usize * usize::from(self.container_channels) * 2
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SourceFormat {
    Pcm(PcmFormat),
    Bitstream(BitstreamFormat),
}

impl SourceFormat {
    /// Bytes the virtual device consumes per second in this format.
    pub fn bytes_per_second(&self) -> usize {
        match self {
            Self::Pcm(p) => p.sample_rate as usize * p.bytes_per_frame(),
            Self::Bitstream(b) => b.container_bytes_per_second(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_rates_match_the_containers() {
        let mat = SourceFormat::Bitstream(BitstreamFormat {
            codec: BitstreamCodec::Mat21,
            container_rate: 192_000,
            container_channels: 8,
            encoded_rate: 48_000,
            encoded_channels: 0,
        });
        // 61,440 bytes per 20 ms burst, as the build logs measured.
        assert_eq!(mat.bytes_per_second() / 50, 61_440);
        let pcm = SourceFormat::Pcm(PcmFormat {
            channels: 2,
            sample_rate: 48_000,
            bits_per_sample: 16,
            valid_bits: 16,
            sample_type: SampleType::Int,
            channel_mask: 0b11,
        });
        assert_eq!(pcm.bytes_per_second(), 192_000);
    }
}
