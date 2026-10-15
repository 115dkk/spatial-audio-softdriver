//! Engine status, sampled by the control thread at 30 Hz.
//!
//! The engine publishes these through atomics; `Engine::status` copies them
//! and computes latency percentiles on its caller without blocking the
//! real-time threads.

use crate::{format::SourceFormat, ports::OutputMode};

/// Status slots mirror the driver's fixed endpoint range without making this
/// foundational crate depend on `ssr-abi`.
pub const ENGINE_ENDPOINTS: usize = 4;

/// Stable chain-error classifications published by the live engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ChainErrorKind {
    UnsupportedCodec,
    UnsupportedFormat,
    CorruptPayload,
    MissingFormat,
    WrongRoute,
    UnsupportedSceneRate,
    TruncatedRecord,
    BadRecordLength,
    BadFormatRecord,
    BadVolumeRecord,
    MissingDirectLayout,
    /// The decode thread was behind and a burst was dropped (ADR 0031).
    DecoderBehind,
}

/// Why an entrance's current source format cannot be decoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum UnsupportedReason {
    Format = 1,
    SourceRate = 2,
    DirectLayout = 3,
}

impl UnsupportedReason {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Format => "format",
            Self::SourceRate => "source_rate",
            Self::DirectLayout => "direct_layout",
        }
    }

    pub const fn code(self) -> u32 {
        self as u32
    }

    pub const fn from_code(code: u32) -> Option<Self> {
        match code {
            1 => Some(Self::Format),
            2 => Some(Self::SourceRate),
            3 => Some(Self::DirectLayout),
            _ => None,
        }
    }
}

impl core::fmt::Display for UnsupportedReason {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(match self {
            Self::Format => "source format is unsupported",
            Self::SourceRate => "the source sample rate cannot be converted to the 48 kHz mix rate",
            Self::DirectLayout => {
                "PCM with channel mask 0 has no direct layout for its channel count"
            }
        })
    }
}

impl ChainErrorKind {
    pub const ALL: [Self; 12] = [
        Self::UnsupportedCodec,
        Self::UnsupportedFormat,
        Self::CorruptPayload,
        Self::MissingFormat,
        Self::WrongRoute,
        Self::UnsupportedSceneRate,
        Self::TruncatedRecord,
        Self::BadRecordLength,
        Self::BadFormatRecord,
        Self::BadVolumeRecord,
        Self::MissingDirectLayout,
        Self::DecoderBehind,
    ];
    pub const COUNT: usize = Self::ALL.len();

    pub const fn name(self) -> &'static str {
        match self {
            Self::UnsupportedCodec => "unsupported_codec",
            Self::UnsupportedFormat => "unsupported_format",
            Self::CorruptPayload => "corrupt_payload",
            Self::MissingFormat => "missing_format",
            Self::WrongRoute => "wrong_route",
            Self::UnsupportedSceneRate => "unsupported_scene_rate",
            Self::TruncatedRecord => "truncated_record",
            Self::BadRecordLength => "bad_record_length",
            Self::BadFormatRecord => "bad_format_record",
            Self::BadVolumeRecord => "bad_volume_record",
            Self::MissingDirectLayout => "missing_direct_layout",
            Self::DecoderBehind => "decoder_behind",
        }
    }

    pub const fn index(self) -> usize {
        self as usize
    }

    pub const fn unsupported_reason(self) -> Option<UnsupportedReason> {
        match self {
            Self::UnsupportedCodec | Self::UnsupportedFormat => Some(UnsupportedReason::Format),
            Self::UnsupportedSceneRate => Some(UnsupportedReason::SourceRate),
            Self::MissingDirectLayout => Some(UnsupportedReason::DirectLayout),
            Self::CorruptPayload
            | Self::MissingFormat
            | Self::WrongRoute
            | Self::TruncatedRecord
            | Self::BadRecordLength
            | Self::BadFormatRecord
            | Self::BadVolumeRecord
            | Self::DecoderBehind => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChainErrorCounts {
    counts: [u64; ChainErrorKind::COUNT],
}

impl ChainErrorCounts {
    pub fn from_fn(mut f: impl FnMut(ChainErrorKind) -> u64) -> Self {
        Self {
            counts: ChainErrorKind::ALL.map(&mut f),
        }
    }

    pub const fn get(&self, kind: ChainErrorKind) -> u64 {
        self.counts[kind.index()]
    }

    pub fn total(&self) -> u64 {
        self.counts.iter().copied().sum()
    }

    pub fn iter(&self) -> impl Iterator<Item = (ChainErrorKind, u64)> + '_ {
        ChainErrorKind::ALL
            .into_iter()
            .map(|kind| (kind, self.get(kind)))
    }
}

/// Latency of the whole chain, from the driver's record QPC to the output
/// device's playback QPC (the time inside the source application is not part
/// of it).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LatencyStats {
    pub p50_ms: f32,
    pub p99_ms: f32,
    /// Blocks the statistics were computed from.
    pub samples: u32,
}

/// Why the engine stopped making sound on its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EngineFault {
    CaptureLost(String),
    OutputLost(String),
}

/// The clock-domain strategy selected when the engine starts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ClockStatus {
    Trim,
    #[default]
    Asrc,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct EndpointStatus {
    pub format: Option<SourceFormat>,
    pub epoch: u32,
    /// Endpoint gain after combining its channel controls. Muting is carried
    /// separately so status never needs a `-inf` floating-point sentinel.
    pub volume_db: f32,
    pub muted: bool,
    /// Why this entrance's current format cannot be decoded. This is local to
    /// the entrance and never faults the other entrances.
    pub unsupported: Option<UnsupportedReason>,
    /// Every chain error attributed to this entrance, by stable kind.
    pub chain_errors: ChainErrorCounts,
}

/// What the output saw in the period that last found the crossing ring short,
/// to tell an empty ring from one the output could not take from. Frame
/// counts are in scene frames.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct UnderrunSnapshot {
    /// Frames in the ring's audio samples when the period began.
    pub ring_frames: u32,
    /// Frames the timing markers accounted for (what priming and the clock
    /// controller see).
    pub marker_frames: u32,
    /// Frames of the current output layout the period could take.
    pub layout_frames: u32,
    /// Frames the period needed (for the resampler, its whole take).
    pub needed_frames: u32,
    /// Output frames the period produced before it ran short.
    pub produced_frames: u32,
    /// Milliseconds since the capture side last put a block into the ring.
    pub since_block_ms: f32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct EngineStatus {
    pub endpoints: [EndpointStatus; ENGINE_ENDPOINTS],
    /// Chain errors whose malformed record did not identify an entrance.
    pub unattributed_chain_errors: ChainErrorCounts,
    /// Records naming an endpoint beyond the driver's range.
    pub skipped_records: u64,
    pub latency: LatencyStats,
    /// Lowest crossing-ring level over the last control window, in
    /// milliseconds of rendered signal: the margin the clock controller holds
    /// at the configured target.
    pub ring_level_ms: f32,
    /// Times the output found the crossing ring empty.
    pub underruns: u64,
    /// The last of those times, as the output saw it.
    pub last_underrun: Option<UnderrunSnapshot>,
    /// Times the output device's stream was restarted after it stalled.
    pub output_restarts: u64,
    /// Sample rate of the currently open output sink, or zero before open.
    pub output_rate: u32,
    /// Whether the capture source accepted rate trim when the engine started.
    pub trim_available: bool,
    /// Layout of the currently open output sink.
    pub output_channels: u16,
    pub output_channel_mask: u32,
    /// Times the output sink was reopened for a changed layout.
    pub output_reopens: u64,
    /// Successful output reopens after an I/O or device error.
    pub output_reconnects: u64,
    /// Output-open attempts made while recovering from an I/O or device error.
    pub output_reconnect_attempts: u64,
    pub output_mode: OutputMode,
    pub output_fell_back: bool,
    /// Channels in the shared engine's mix format when fallback occurred.
    pub output_mix_channels: u16,
    /// Observed output periods per second divided by the nominal period rate.
    pub output_cadence_ratio: f32,
    pub output_cadence_faults: u64,
    /// Bytes the driver reported lost to overruns.
    pub lost_bytes: u64,
    /// Timeline frames emitted without data from an active entrance.
    pub lane_underrun_frames: u64,
    /// Source frames discarded because an entrance outran its bounded queue.
    pub dropped_frames: u64,
    /// E-AC-3 bursts dropped because the decode thread had not returned the
    /// deliveries queued before them (ADR 0031).
    pub decode_refused: u64,
    /// Volume transitions coalesced after an entrance's bounded queue filled.
    pub volume_transitions_coalesced: u64,
    /// Rendered frames that could not enter the crossing ring.
    pub output_overflow_frames: u64,
    /// Idle-time blocks generated to advance tails and state.
    pub idle_blocks: u64,
    /// Real-time events that could not enter an event queue.
    pub dropped_events: u64,
    pub clock: ClockStatus,
    /// Current clock correction in ppm (trim or resampling ratio).
    pub clock_ppm: f64,
    pub fault: Option<EngineFault>,
}

#[cfg(test)]
mod tests {
    use super::{ChainErrorKind, UnsupportedReason};

    #[test]
    fn unsupported_reason_codes_names_and_messages_are_stable() {
        for (reason, code, name, message) in [
            (
                UnsupportedReason::Format,
                1,
                "format",
                "source format is unsupported",
            ),
            (
                UnsupportedReason::SourceRate,
                2,
                "source_rate",
                "the source sample rate cannot be converted to the 48 kHz mix rate",
            ),
            (
                UnsupportedReason::DirectLayout,
                3,
                "direct_layout",
                "PCM with channel mask 0 has no direct layout for its channel count",
            ),
        ] {
            assert_eq!(reason.code(), code);
            assert_eq!(UnsupportedReason::from_code(code), Some(reason));
            assert_eq!(reason.name(), name);
            assert_eq!(reason.to_string(), message);
        }
        assert_eq!(UnsupportedReason::from_code(0), None);
        assert_eq!(UnsupportedReason::from_code(4), None);
    }

    #[test]
    fn every_chain_error_has_the_expected_unsupported_reason() {
        for kind in ChainErrorKind::ALL {
            let expected = match kind {
                ChainErrorKind::UnsupportedCodec | ChainErrorKind::UnsupportedFormat => {
                    Some(UnsupportedReason::Format)
                }
                ChainErrorKind::UnsupportedSceneRate => Some(UnsupportedReason::SourceRate),
                ChainErrorKind::MissingDirectLayout => Some(UnsupportedReason::DirectLayout),
                _ => None,
            };
            assert_eq!(kind.unsupported_reason(), expected, "{kind:?}");
        }
    }
}
