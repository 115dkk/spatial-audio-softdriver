//! Channel order for PCM streams whose WAVE channel mask is zero.

use std::str::FromStr;

use ssr_types::Speaker;

use crate::pcm::MAX_PCM_CHANNELS;

const EMPTY_SPEAKERS: [Speaker; MAX_PCM_CHANNELS] = [Speaker::L; MAX_PCM_CHANNELS];

/// One allocation-free direct-output channel order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DirectLayout {
    len: u8,
    speakers: [Speaker; MAX_PCM_CHANNELS],
}

impl DirectLayout {
    /// Positions may repeat (two channels then sum at one place), but the
    /// scene has one LFE, so at most one of `Lfe` and `Lfe2` may appear.
    pub fn new(speakers: &[Speaker]) -> Result<Self, &'static str> {
        if speakers.is_empty() || speakers.len() > MAX_PCM_CHANNELS {
            return Err("direct layout must contain 1 to 32 speakers");
        }
        let lfe = speakers
            .iter()
            .filter(|speaker| matches!(speaker, Speaker::Lfe | Speaker::Lfe2))
            .count();
        if lfe > 1 {
            return Err("direct layout has more than one LFE channel");
        }
        let mut result = Self {
            len: speakers.len() as u8,
            speakers: EMPTY_SPEAKERS,
        };
        result.speakers[..speakers.len()].copy_from_slice(speakers);
        Ok(result)
    }

    pub const fn len(self) -> usize {
        self.len as usize
    }

    pub const fn is_empty(self) -> bool {
        self.len == 0
    }

    pub fn speakers(&self) -> &[Speaker] {
        &self.speakers[..self.len()]
    }
}

/// Direct-output orders indexed by channel count minus one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DirectLayouts {
    by_channels: [Option<DirectLayout>; MAX_PCM_CHANNELS],
}

impl DirectLayouts {
    pub const fn empty() -> Self {
        Self {
            by_channels: [None; MAX_PCM_CHANNELS],
        }
    }

    pub fn get(&self, channels: u16) -> Option<DirectLayout> {
        channels
            .checked_sub(1)
            .and_then(|index| self.by_channels.get(index as usize))
            .copied()
            .flatten()
    }

    pub fn iter(&self) -> impl Iterator<Item = (u16, DirectLayout)> + '_ {
        self.by_channels
            .iter()
            .enumerate()
            .filter_map(|(index, layout)| layout.map(|layout| (index as u16 + 1, layout)))
    }

    pub fn set(&mut self, channels: u16, layout: DirectLayout) -> Result<(), &'static str> {
        if usize::from(channels) != layout.len() {
            return Err("direct layout speaker count does not match its channel count");
        }
        let slot = channels
            .checked_sub(1)
            .and_then(|index| self.by_channels.get_mut(index as usize))
            .ok_or("direct layout channel count must be in 1..=32")?;
        *slot = Some(layout);
        Ok(())
    }
}

impl Default for DirectLayouts {
    fn default() -> Self {
        let mut layouts = Self::empty();
        // Counts 1..=6 and 8 retain the WAVE default-mask orders. For ambiguous
        // counts, FFmpeg's named native layouts choose one deterministic default:
        // 10 is 7.1.2 rather than 5.1.4, and 14 is 9.1.4. Users can override it.
        for name in [
            "mono", "stereo", "3.0", "quad", "5.0", "5.1", "7.1", "7.1.2", "7.1.4", "9.1.4",
            "9.1.6",
        ] {
            let layout = preset(name).expect("built-in direct layout exists");
            layouts
                .set(layout.len() as u16, layout)
                .expect("built-in direct layout count matches");
        }
        layouts
    }
}

/// One command-line override, written as `<channels>=<preset|speaker,...>`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DirectLayoutOverride {
    pub channels: u16,
    pub layout: DirectLayout,
}

impl FromStr for DirectLayoutOverride {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (channels, order) = value
            .split_once('=')
            .ok_or_else(|| "direct layout must be <channels>=<preset|speaker,...>".to_owned())?;
        let channels = channels
            .parse::<u16>()
            .map_err(|_| "direct layout channel count is not an integer".to_owned())?;
        if !(1..=MAX_PCM_CHANNELS as u16).contains(&channels) {
            return Err("direct layout channel count must be in 1..=32".to_owned());
        }
        let layout = if let Some(layout) = preset(order) {
            layout
        } else {
            let names = order.split(',').collect::<Vec<_>>();
            if names.len() != usize::from(channels) {
                return Err(format!(
                    "direct layout for {channels} channels names {} speakers",
                    names.len()
                ));
            }
            let mut speakers = EMPTY_SPEAKERS;
            for (index, name) in names.iter().enumerate() {
                let speaker = parse_speaker(name)
                    .ok_or_else(|| format!("unknown direct-layout speaker {name:?}"))?;
                if speakers[..index].contains(&speaker) {
                    return Err(format!("direct layout repeats speaker {name:?}"));
                }
                speakers[index] = speaker;
            }
            DirectLayout::new(&speakers[..names.len()])?
        };
        if layout.len() != usize::from(channels) {
            return Err(format!(
                "direct layout preset has {} channels, not {channels}",
                layout.len()
            ));
        }
        Ok(Self { channels, layout })
    }
}

/// Named orders. Counts Windows gives a default mask (1-6, 8) keep that
/// mask's bit order, so `5.0` and `5.1` use the back pair (Lrs/Rrs) like
/// Windows' 0x3f; the 5.1.x presets keep that pair and add heights, where
/// FFmpeg's 5.0, 5.1 and 5.1.4 use the side pair. 7.1.x and 9.1.x follow
/// FFmpeg's layouts of the same name in its native (bit) order.
pub fn preset(name: &str) -> Option<DirectLayout> {
    DirectLayout::new(ssr_types::channel_layout_preset(name)?).ok()
}

/// Reads a speaker name, ignoring case: one of [`speaker_name`]'s names, or
/// FFmpeg's channel name for the same position (`av_channel_name`: FL, FR,
/// FC, BL, BR, BC, SL, SR, FLC, FRC, WL, WR, TBL, TBR, TBC; the others are
/// already ours), which Impulcifer and HeSuVi users write.
pub fn parse_speaker(name: &str) -> Option<Speaker> {
    Speaker::from_name(name)
}

pub const fn speaker_name(speaker: Speaker) -> &'static str {
    speaker.name()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_direct_layout_channels_match_driver_contract() {
        let channels = DirectLayouts::default()
            .iter()
            .fold(0, |mask, (channels, _)| mask | 1 << (channels - 1));
        assert_eq!(channels, ssr_abi::DIRECT_DEFAULT_CHANNELS);
    }
}
