use engine::StopReport;
use serde_json::{Map, Value};
use ssr_types::{ChainErrorCounts, ClockStatus, OutputMode};

use crate::driver_stats::DriverStatsTotals;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectLayoutField {
    pub channels: u16,
    pub speakers: Vec<String>,
}

/// Values that belong to a particular caller request rather than the session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestFields {
    pub source: String,
    pub clock_requested: String,
    pub output_mode_requested: String,
    pub direct_layouts: Vec<DirectLayoutField>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RunFigures {
    pub overruns: u64,
    pub faults: Vec<String>,
    pub underruns_while_streaming: Option<u64>,
    pub ring_trough_min_ms: Option<f32>,
    pub clock_ppm_max_abs: f64,
}

impl RunFigures {
    pub(crate) fn observe(&mut self, status: &ssr_types::EngineStatus, seconds: u64) {
        if seconds < 5 || self.underruns_while_streaming.is_some() {
            return;
        }
        let trough = self.ring_trough_min_ms.get_or_insert(status.ring_level_ms);
        *trough = trough.min(status.ring_level_ms);
        self.clock_ppm_max_abs = self.clock_ppm_max_abs.max(status.clock_ppm.abs());
    }
}

#[derive(Debug)]
pub struct SessionReport {
    pub seconds: f64,
    pub engine: StopReport,
    pub figures: RunFigures,
    pub output_kind: engine::OutputKind,
    pub output_channels: usize,
    pub output_channel_mask: u32,
    pub recorder_lost_bytes: u64,
    pub driver: Option<DriverStatsTotals>,
    pub driver_stats_error: Option<String>,
}

pub fn chain_errors_json(counts: ChainErrorCounts) -> Value {
    counts
        .iter()
        .map(|(kind, count)| (kind.name().to_owned(), count.into()))
        .collect::<Map<_, _>>()
        .into()
}

impl SessionReport {
    /// Produces the stable `ssr-live --report` JSON contract.
    pub fn to_json(&self, request: &RequestFields) -> Value {
        let status = &self.engine.final_status;
        let mut faults = self.figures.faults.clone();
        if let Some(fault) = &status.fault {
            faults.push(format!("{fault:?}"));
        }
        let endpoints = status
            .endpoints
            .iter()
            .enumerate()
            .map(|(endpoint, status)| {
                serde_json::json!({
                    "endpoint": endpoint,
                    "format": status.format.map(|format| format!("{format:?}")),
                    "epoch": status.epoch,
                    "unsupported": status.unsupported.map(|reason| reason.to_string()),
                    "unsupported_reason": status.unsupported.map(ssr_types::UnsupportedReason::name),
                    "chain_errors": chain_errors_json(status.chain_errors),
                })
            })
            .collect::<Vec<_>>();
        let last_underrun = status.last_underrun.map(|value| {
            serde_json::json!({
                "ring_frames": value.ring_frames,
                "marker_frames": value.marker_frames,
                "layout_frames": value.layout_frames,
                "needed_frames": value.needed_frames,
                "produced_frames": value.produced_frames,
                "since_block_ms": value.since_block_ms,
            })
        });
        serde_json::json!({
            "output_mode": match status.output_mode {
                OutputMode::Exclusive => "exclusive",
                OutputMode::Shared => "shared",
            },
            "output_fell_back": status.output_fell_back,
            "output_kind": self.output_kind.as_str(),
            "output_channels": self.output_channels,
            "output_channel_mask": self.output_channel_mask,
            "direct_layouts": request.direct_layouts.iter().map(|value| serde_json::json!({
                "channels": value.channels,
                "speakers": value.speakers,
            })).collect::<Vec<_>>(),
            "clock_strategy": match status.clock {
                ClockStatus::Trim => "trim",
                ClockStatus::Asrc => "asrc",
            },
            "seconds": self.seconds,
            "blocks": self.engine.blocks,
            "underruns": self.engine.underruns,
            "underruns_while_streaming": self.figures.underruns_while_streaming.unwrap_or(self.engine.underruns),
            "last_underrun": last_underrun,
            "output_restarts": status.output_restarts,
            "output_reopens": status.output_reopens,
            "output_reconnects": status.output_reconnects,
            "output_reconnect_attempts": status.output_reconnect_attempts,
            "output_mix_channels": status.output_mix_channels,
            "output_cadence_ratio": status.output_cadence_ratio,
            "output_cadence_faults": status.output_cadence_faults,
            "lost_bytes": self.engine.lost_bytes,
            "lane_underrun_frames": self.engine.lane_underrun_frames,
            "dropped_frames": self.engine.dropped_frames,
            "volume_transitions_coalesced": status.volume_transitions_coalesced,
            "output_overflow_frames": self.engine.output_overflow_frames,
            "idle_blocks": self.engine.idle_blocks,
            "dropped_events": self.engine.dropped_events,
            "skipped_records": status.skipped_records,
            "endpoints": endpoints,
            "chain_errors": { "unattributed": chain_errors_json(status.unattributed_chain_errors) },
            "overruns": self.figures.overruns,
            "faults": faults,
            "latency_p50_ms": status.latency.p50_ms,
            "latency_p99_ms": status.latency.p99_ms,
            "ring_trough_min_ms": self.figures.ring_trough_min_ms,
            "clock_ppm": status.clock_ppm,
            "clock_ppm_max_abs": self.figures.clock_ppm_max_abs,
            "tee_dropped_bytes": self.recorder_lost_bytes,
            "recorder_lost_bytes": self.recorder_lost_bytes,
            "driver": self.driver.as_ref().map(DriverStatsTotals::to_json),
            "driver_stats_error": self.driver_stats_error,
            "source": request.source,
            "clock_requested": request.clock_requested,
            "output_mode_requested": request.output_mode_requested,
            "output_sample_rate": status.output_rate,
            "trim_available": status.trim_available,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use ssr_types::{ChainErrorCounts, ChainErrorKind};

    use super::chain_errors_json;

    #[test]
    fn chain_error_report_uses_all_stable_snake_case_names() {
        let counts = ChainErrorCounts::from_fn(|kind| kind.index() as u64 + 1);
        let report = chain_errors_json(counts);
        let errors = report.as_object().unwrap();
        assert_eq!(errors.len(), ChainErrorKind::COUNT);
        let names = errors.keys().map(String::as_str).collect::<BTreeSet<_>>();
        let expected = ChainErrorKind::ALL
            .iter()
            .map(|kind| kind.name())
            .collect::<BTreeSet<_>>();
        assert_eq!(names, expected);
        for kind in ChainErrorKind::ALL {
            let name = kind.name();
            assert!(
                name.bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte == b'_'),
                "{name} is not snake_case"
            );
            assert_eq!(errors[name], serde_json::json!(kind.index() as u64 + 1));
        }
    }
}
