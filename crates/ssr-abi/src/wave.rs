//! WAVEFORMATEX(TENSIBLE) as a format-change record carries it.
//!
//! The driver copies the structure Windows negotiated without reading it; this
//! module is where user mode does. Subtype GUIDs follow `ksmedia.h` (Windows
//! SDK 10.0.26100): the PCM family is `{tag-0000-0010-8000-00aa00389b71}`,
//! the IEC 61937 family `{id-0cea-0010-8000-00aa00389b71}`.

use core::fmt;

use ssr_types::{BitstreamCodec, BitstreamFormat, PcmFormat, SampleType, SourceFormat};

pub const WAVE_FORMAT_PCM: u16 = 0x0001;
pub const WAVE_FORMAT_IEEE_FLOAT: u16 = 0x0003;
pub const WAVE_FORMAT_DTS: u16 = 0x0008;
pub const WAVE_FORMAT_DOLBY_AC3_SPDIF: u16 = 0x0092;
pub const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;

/// The fixed tail shared by both GUID families.
pub const WAVE_GUID_TAIL: [u8; 10] = [0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71];
pub const WAVE_FAMILY: u16 = 0x0000;
pub const IEC61937_FAMILY: u16 = 0x0cea;

pub const WAVEFORMATEX_BYTES: usize = 18;
pub const WAVEFORMATEXTENSIBLE_BYTES: usize = 40;
pub const IEC61937_FORMAT_BYTES: usize = 52;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum WaveFormatError {
    #[error("the format is {have} bytes; at least {need} are needed")]
    Truncated { need: usize, have: usize },
    #[error("wave format tag {0:#06x} is not one this app decodes")]
    UnknownTag(u16),
    #[error("subformat GUID {} is not one this app knows", GuidDisplay(.0))]
    UnknownSubformat([u8; 16]),
    #[error("the format is inconsistent: {0}")]
    Inconsistent(&'static str),
}

struct GuidDisplay<'a>(&'a [u8; 16]);

impl fmt::Display for GuidDisplay<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let g = self.0;
        write!(
            f,
            "{{{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}}}",
            u32_at(g, 0),
            u16_at(g, 4),
            u16_at(g, 6),
            g[8],
            g[9],
            g[10],
            g[11],
            g[12],
            g[13],
            g[14],
            g[15]
        )
    }
}

fn u16_at(b: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([b[i], b[i + 1]])
}

fn u32_at(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

/// Parses the raw structure into a [`SourceFormat`].
pub fn parse_wave_format(b: &[u8]) -> Result<SourceFormat, WaveFormatError> {
    if b.len() < WAVEFORMATEX_BYTES {
        return Err(WaveFormatError::Truncated {
            need: WAVEFORMATEX_BYTES,
            have: b.len(),
        });
    }
    let tag = u16_at(b, 0);
    let channels = u16_at(b, 2);
    let rate = u32_at(b, 4);
    let bits = u16_at(b, 14);
    if channels == 0 || rate == 0 {
        return Err(WaveFormatError::Inconsistent("zero channels or rate"));
    }
    let pcm = |sample_type, valid_bits, channel_mask| {
        if bits == 0 || !bits.is_multiple_of(8) || valid_bits > bits {
            return Err(WaveFormatError::Inconsistent("sample width"));
        }
        Ok(SourceFormat::Pcm(PcmFormat {
            channels,
            sample_rate: rate,
            bits_per_sample: bits,
            valid_bits,
            sample_type,
            channel_mask,
        }))
    };
    let bitstream = |codec, encoded_rate, encoded_channels| {
        Ok(SourceFormat::Bitstream(BitstreamFormat {
            codec,
            container_rate: rate,
            container_channels: channels,
            encoded_rate,
            encoded_channels,
        }))
    };
    let plain_mask = match channels {
        1 => 0x4,
        2 => 0x3,
        _ => 0,
    };
    match tag {
        WAVE_FORMAT_PCM => pcm(SampleType::Int, bits, plain_mask),
        WAVE_FORMAT_IEEE_FLOAT => pcm(SampleType::Float, bits, plain_mask),
        WAVE_FORMAT_DOLBY_AC3_SPDIF => bitstream(BitstreamCodec::Ac3, 0, 0),
        WAVE_FORMAT_DTS => bitstream(BitstreamCodec::Dts, 0, 0),
        WAVE_FORMAT_EXTENSIBLE => {
            if b.len() < WAVEFORMATEXTENSIBLE_BYTES {
                return Err(WaveFormatError::Truncated {
                    need: WAVEFORMATEXTENSIBLE_BYTES,
                    have: b.len(),
                });
            }
            let cb_size = usize::from(u16_at(b, 16));
            if cb_size < WAVEFORMATEXTENSIBLE_BYTES - WAVEFORMATEX_BYTES {
                return Err(WaveFormatError::Inconsistent(
                    "cbSize too small for EXTENSIBLE",
                ));
            }
            let valid_bits = u16_at(b, 18);
            let mask = u32_at(b, 20);
            let guid: [u8; 16] = b[24..40].try_into().expect("slice length checked above");
            let id = u32_at(&guid, 0);
            let family = u16_at(&guid, 4);
            if guid[6..16] != WAVE_GUID_TAIL {
                return Err(WaveFormatError::UnknownSubformat(guid));
            }
            let (encoded_rate, encoded_channels) = if cb_size
                >= IEC61937_FORMAT_BYTES - WAVEFORMATEX_BYTES
                && b.len() >= IEC61937_FORMAT_BYTES
            {
                (u32_at(b, 40), u32_at(b, 44))
            } else {
                (0, 0)
            };
            let valid = if valid_bits == 0 { bits } else { valid_bits };
            match (family, id) {
                (WAVE_FAMILY, 0x0001) => pcm(SampleType::Int, valid, mask),
                (WAVE_FAMILY, 0x0003) => pcm(SampleType::Float, valid, mask),
                (WAVE_FAMILY, 0x0092) => {
                    bitstream(BitstreamCodec::Ac3, encoded_rate, encoded_channels)
                }
                (WAVE_FAMILY, 0x0008) => {
                    bitstream(BitstreamCodec::Dts, encoded_rate, encoded_channels)
                }
                (IEC61937_FAMILY, id) => {
                    let codec = match id {
                        0x0000_000a => BitstreamCodec::Eac3,
                        0x0000_010a => BitstreamCodec::Eac3Joc,
                        // DTS:X E1 (0x10b) and E2 (0x30b) are DTS-HD streams
                        // carrying DTS:X; the decoder reads which from the stream.
                        0x0000_000b | 0x0000_010b | 0x0000_030b => BitstreamCodec::DtsHd,
                        0x0000_000c => BitstreamCodec::TrueHd,
                        0x0000_010c => BitstreamCodec::Mat20,
                        // MAT21_PROFILE4 (0x70c) is the same container.
                        0x0000_030c | 0x0000_070c => BitstreamCodec::Mat21,
                        other => BitstreamCodec::Other(other),
                    };
                    bitstream(codec, encoded_rate, encoded_channels)
                }
                _ => Err(WaveFormatError::UnknownSubformat(guid)),
            }
        }
        other => Err(WaveFormatError::UnknownTag(other)),
    }
}

/// The WAVEFORMATEXTENSIBLE Windows would negotiate for `format`: 40 bytes for
/// PCM, 52 (the IEC 61937 extension) for a bitstream. Used to synthesize
/// format-change records outside the driver.
pub fn wave_format_bytes(format: &SourceFormat) -> Vec<u8> {
    let (channels, rate, bits, valid, mask, family, id, block_bytes, encoded) = match *format {
        SourceFormat::Pcm(p) => {
            let id = match p.sample_type {
                SampleType::Int => 0x0001,
                SampleType::Float => 0x0003,
            };
            (
                p.channels,
                p.sample_rate,
                p.bits_per_sample,
                p.valid_bits,
                p.channel_mask,
                WAVE_FAMILY,
                id,
                p.bytes_per_frame(),
                None,
            )
        }
        SourceFormat::Bitstream(s) => {
            let (family, id) = match s.codec {
                BitstreamCodec::Ac3 => (WAVE_FAMILY, 0x0092),
                BitstreamCodec::Dts => (WAVE_FAMILY, 0x0008),
                BitstreamCodec::Eac3 => (IEC61937_FAMILY, 0x000a),
                BitstreamCodec::Eac3Joc => (IEC61937_FAMILY, 0x010a),
                BitstreamCodec::DtsHd => (IEC61937_FAMILY, 0x000b),
                BitstreamCodec::TrueHd => (IEC61937_FAMILY, 0x000c),
                BitstreamCodec::Mat20 => (IEC61937_FAMILY, 0x010c),
                BitstreamCodec::Mat21 => (IEC61937_FAMILY, 0x030c),
                BitstreamCodec::Other(id) => (IEC61937_FAMILY, id),
            };
            (
                s.container_channels,
                s.container_rate,
                16,
                16,
                0,
                family,
                id,
                usize::from(s.container_channels) * 2,
                Some((s.encoded_rate, s.encoded_channels)),
            )
        }
    };
    let len = if encoded.is_some() {
        IEC61937_FORMAT_BYTES
    } else {
        WAVEFORMATEXTENSIBLE_BYTES
    };
    let mut b = Vec::with_capacity(len);
    b.extend_from_slice(&WAVE_FORMAT_EXTENSIBLE.to_le_bytes());
    b.extend_from_slice(&channels.to_le_bytes());
    b.extend_from_slice(&rate.to_le_bytes());
    b.extend_from_slice(&(rate * block_bytes as u32).to_le_bytes());
    b.extend_from_slice(&(block_bytes as u16).to_le_bytes());
    b.extend_from_slice(&bits.to_le_bytes());
    b.extend_from_slice(&((len - WAVEFORMATEX_BYTES) as u16).to_le_bytes());
    b.extend_from_slice(&valid.to_le_bytes());
    b.extend_from_slice(&mask.to_le_bytes());
    b.extend_from_slice(&id.to_le_bytes());
    b.extend_from_slice(&family.to_le_bytes());
    b.extend_from_slice(&WAVE_GUID_TAIL);
    if let Some((encoded_rate, encoded_channels)) = encoded {
        b.extend_from_slice(&encoded_rate.to_le_bytes());
        b.extend_from_slice(&encoded_channels.to_le_bytes());
        // dwAverageBytesPerSec of the encoded stream: unknown here.
        b.extend_from_slice(&0u32.to_le_bytes());
    }
    debug_assert_eq!(b.len(), len);
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_formats() -> Vec<SourceFormat> {
        let mut v = vec![
            SourceFormat::Pcm(PcmFormat {
                channels: 8,
                sample_rate: 48_000,
                bits_per_sample: 32,
                valid_bits: 24,
                sample_type: SampleType::Int,
                channel_mask: 0x63f,
            }),
            SourceFormat::Pcm(PcmFormat {
                channels: 2,
                sample_rate: 44_100,
                bits_per_sample: 32,
                valid_bits: 32,
                sample_type: SampleType::Float,
                channel_mask: 0x3,
            }),
        ];
        for codec in [
            BitstreamCodec::Ac3,
            BitstreamCodec::Eac3,
            BitstreamCodec::Eac3Joc,
            BitstreamCodec::TrueHd,
            BitstreamCodec::Mat20,
            BitstreamCodec::Mat21,
            BitstreamCodec::Dts,
            BitstreamCodec::DtsHd,
            BitstreamCodec::Other(0x0001_10bf),
        ] {
            v.push(SourceFormat::Bitstream(BitstreamFormat {
                codec,
                container_rate: 192_000,
                container_channels: 8,
                encoded_rate: 48_000,
                encoded_channels: 16,
            }));
        }
        v
    }

    #[test]
    fn every_format_round_trips() {
        for f in all_formats() {
            assert_eq!(parse_wave_format(&wave_format_bytes(&f)), Ok(f), "{f:?}");
        }
    }

    #[test]
    fn plain_waveformatex_pcm_and_ac3_parse() {
        // WAVEFORMATEX for 16-bit stereo PCM at 48 kHz, cbSize 0.
        let mut b = vec![];
        for v in [1u16, 2] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.extend_from_slice(&48_000u32.to_le_bytes());
        b.extend_from_slice(&192_000u32.to_le_bytes());
        for v in [4u16, 16, 0] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        let SourceFormat::Pcm(p) = parse_wave_format(&b).unwrap() else {
            panic!("not PCM");
        };
        assert_eq!((p.channels, p.valid_bits, p.channel_mask), (2, 16, 3));
        b[0..2].copy_from_slice(&WAVE_FORMAT_DOLBY_AC3_SPDIF.to_le_bytes());
        assert!(matches!(
            parse_wave_format(&b),
            Ok(SourceFormat::Bitstream(BitstreamFormat {
                codec: BitstreamCodec::Ac3,
                ..
            }))
        ));
    }

    #[test]
    fn subtype_aliases_parse_as_their_container() {
        let mat21 = all_formats()
            .into_iter()
            .find(|f| matches!(f, SourceFormat::Bitstream(b) if b.codec == BitstreamCodec::Mat21))
            .unwrap();
        for (id, codec) in [
            (0x070c_u32, BitstreamCodec::Mat21),
            (0x010b, BitstreamCodec::DtsHd),
            (0x030b, BitstreamCodec::DtsHd),
        ] {
            let mut b = wave_format_bytes(&mat21);
            b[24..28].copy_from_slice(&id.to_le_bytes());
            let Ok(SourceFormat::Bitstream(parsed)) = parse_wave_format(&b) else {
                panic!("{id:#x} does not parse");
            };
            assert_eq!(parsed.codec, codec, "{id:#x}");
        }
    }

    #[test]
    fn malformed_formats_are_errors() {
        assert!(matches!(
            parse_wave_format(&[0; 10]),
            Err(WaveFormatError::Truncated { .. })
        ));
        let mut ext = wave_format_bytes(&all_formats()[0]);
        ext.truncate(30);
        assert!(matches!(
            parse_wave_format(&ext),
            Err(WaveFormatError::Truncated { .. })
        ));
        let mut odd = wave_format_bytes(&all_formats()[0]);
        odd[34] ^= 0xff;
        let expected_guid: [u8; 16] = odd[24..40].try_into().unwrap();
        let error = parse_wave_format(&odd).unwrap_err();
        assert_eq!(error, WaveFormatError::UnknownSubformat(expected_guid));
        assert_eq!(
            error.to_string(),
            "subformat GUID {00000001-0000-0010-8000-ffaa00389b71} is not one this app knows"
        );
        let mut tag = wave_format_bytes(&all_formats()[0]);
        tag[0] = 0x55;
        tag[1] = 0x00;
        assert_eq!(
            parse_wave_format(&tag),
            Err(WaveFormatError::UnknownTag(0x55))
        );
    }
}
