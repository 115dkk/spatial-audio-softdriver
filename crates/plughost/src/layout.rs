//! Speaker order is ascending VST 3 speaker bit, never channel-count guessing.
use vst3::Steinberg::Vst::*;

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum LayoutError {
    #[error("unsupported speaker bit {0}")]
    UnknownBit(u32),
    #[error("unknown default layout for {0} channels")]
    UnknownChannels(u16),
    #[error("unsupported VST 2 speaker type {0}")]
    UnknownSpeaker(i32),
    #[error("duplicate or unordered speaker arrangement")]
    Order,
    #[error("empty speaker arrangement (0 channels)")]
    Empty,
    #[error("{0} main bus is missing or its channel count disagrees with arrangement {1:#x}")]
    Bus(&'static str, u64),
}

const SPEAKERS: [(u64, u32); 18] = [
    (kSpeakerL, 0x1),
    (kSpeakerR, 0x2),
    (kSpeakerC, 0x4),
    (kSpeakerLfe, 0x8),
    (kSpeakerLs, 0x10),
    (kSpeakerRs, 0x20),
    (kSpeakerLc, 0x40),
    (kSpeakerRc, 0x80),
    (kSpeakerCs, 0x100),
    (kSpeakerSl, 0x200),
    (kSpeakerSr, 0x400),
    (kSpeakerTc, 0x800),
    (kSpeakerTfl, 0x1000),
    (kSpeakerTfc, 0x2000),
    (kSpeakerTfr, 0x4000),
    (kSpeakerTrl, 0x8000),
    (kSpeakerTrc, 0x10000),
    (kSpeakerTrr, 0x20000),
];

pub fn wave_bits(arrangement: u64) -> Result<Vec<u32>, LayoutError> {
    if arrangement == 0 {
        return Err(LayoutError::Empty);
    }
    let known = SPEAKERS.iter().fold(0, |bits, (speaker, _)| bits | speaker);
    let unknown = arrangement & !known;
    if unknown != 0 {
        return Err(LayoutError::UnknownBit(unknown.trailing_zeros()));
    }
    Ok(SPEAKERS
        .iter()
        .filter_map(|(speaker, wave)| (arrangement & speaker != 0).then_some(*wave))
        .collect())
}

/// Validate the main bus, retaining its declared channel order for the engine.
pub fn main_bus(buses: &[crate::Bus], direction: &'static str) -> Result<Vec<u32>, LayoutError> {
    let bus = buses.first().ok_or(LayoutError::Bus(direction, 0))?;
    let bits = wave_bits(bus.arrangement)?;
    if bus.channels == 0 || bus.channels > 32 || bits.len() != usize::from(bus.channels) {
        return Err(LayoutError::Bus(direction, bus.arrangement));
    }
    Ok(bits)
}

pub fn default_arrangement(channels: u16) -> Result<u64, LayoutError> {
    match channels {
        1 => Ok(kSpeakerC),
        2 => Ok(kSpeakerL | kSpeakerR),
        6 => Ok(kSpeakerL | kSpeakerR | kSpeakerC | kSpeakerLfe | kSpeakerLs | kSpeakerRs),
        8 => Ok(kSpeakerL
            | kSpeakerR
            | kSpeakerC
            | kSpeakerLfe
            | kSpeakerLs
            | kSpeakerRs
            | kSpeakerSl
            | kSpeakerSr),
        _ => Err(LayoutError::UnknownChannels(channels)),
    }
}

/// Types confirmed in Xaymar's clean-room header. Reject reordered channels:
/// a bitmask must not silently lose the plugin's physical channel order.
pub fn vst2_arrangement(speakers: &[i32]) -> Result<u64, LayoutError> {
    let mut result = 0;
    let mut previous = 0;
    for speaker in speakers {
        let bit = match speaker {
            0 | 3 => kSpeakerC,
            1 => kSpeakerL,
            2 => kSpeakerR,
            4 => kSpeakerLfe,
            5 => kSpeakerLs,
            6 => kSpeakerRs,
            10 => kSpeakerSl,
            11 => kSpeakerSr,
            other => return Err(LayoutError::UnknownSpeaker(*other)),
        };
        if bit <= previous {
            return Err(LayoutError::Order);
        }
        previous = bit;
        result |= bit;
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn common_layouts() {
        for (channels, expected) in [
            (1, vec![4]),
            (2, vec![1, 2]),
            (6, vec![1, 2, 4, 8, 16, 32]),
            (8, vec![1, 2, 4, 8, 16, 32, 512, 1024]),
        ] {
            assert_eq!(
                wave_bits(default_arrangement(channels).unwrap()).unwrap(),
                expected
            );
        }
        assert_eq!(
            wave_bits(kSpeakerL | kSpeakerR | kSpeakerLs | kSpeakerRs).unwrap(),
            [1, 2, 16, 32]
        );
        assert_eq!(
            wave_bits(kSpeakerL | kSpeakerR | kSpeakerC | kSpeakerLs | kSpeakerRs).unwrap(),
            [1, 2, 4, 16, 32]
        );
        for (bit, wave) in SPEAKERS {
            assert_eq!(wave_bits(bit).unwrap(), [wave]);
        }
        assert_eq!(wave_bits((1 << 18) - 1).unwrap().len(), 18);
    }
    #[test]
    fn unknown_layouts_are_errors() {
        for bit in 18..64 {
            assert_eq!(wave_bits(1 << bit), Err(LayoutError::UnknownBit(bit)));
        }
        for count in [0, 3, 4, 5, 7, 9, 32] {
            assert!(default_arrangement(count).is_err());
        }
        assert!(vst2_arrangement(&[1, 1]).is_err());
        assert!(vst2_arrangement(&[2, 1]).is_err());
        assert!(vst2_arrangement(&[99]).is_err());
        assert_eq!(
            vst2_arrangement(&[1, 2, 3, 4, 5, 6]).unwrap(),
            default_arrangement(6).unwrap()
        );
    }
}
