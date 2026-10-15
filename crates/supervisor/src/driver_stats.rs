use std::sync::{Arc, Mutex};

use ssr_abi::DriverStats;

/// Supplies one generation-tagged driver statistics snapshot.
pub trait DriverStatsSource: Send {
    fn sample(&self) -> Result<(u64, DriverStats), String>;
}

/// Driver counters accumulated across reconnect generations.
#[derive(Clone, Copy, Debug)]
pub struct DriverStatsTotals {
    generation: u64,
    pub first: DriverStats,
    pub last: DriverStats,
    pub streamed_bytes: [u64; ssr_abi::MAX_ENDPOINTS],
    pub overruns: [u64; ssr_abi::MAX_ENDPOINTS],
    pub lost_bytes: [u64; ssr_abi::MAX_ENDPOINTS],
    pub max_timer_lateness_us: u32,
}

impl DriverStatsTotals {
    pub fn new(generation: u64, stats: DriverStats) -> Self {
        Self {
            generation,
            first: stats,
            last: stats,
            streamed_bytes: [0; ssr_abi::MAX_ENDPOINTS],
            overruns: [0; ssr_abi::MAX_ENDPOINTS],
            lost_bytes: [0; ssr_abi::MAX_ENDPOINTS],
            max_timer_lateness_us: stats.max_timer_lateness_us,
        }
    }

    pub fn observe(&mut self, generation: u64, stats: DriverStats) {
        if generation != self.generation {
            return;
        }
        for endpoint in 0..ssr_abi::MAX_ENDPOINTS {
            let before = self.last.endpoints[endpoint];
            let after = stats.endpoints[endpoint];
            self.streamed_bytes[endpoint] = self.streamed_bytes[endpoint]
                .saturating_add(after.total_bytes.saturating_sub(before.total_bytes));
            self.overruns[endpoint] = self.overruns[endpoint]
                .saturating_add(after.overruns.saturating_sub(before.overruns));
            self.lost_bytes[endpoint] = self.lost_bytes[endpoint]
                .saturating_add(after.lost_bytes.saturating_sub(before.lost_bytes));
        }
        self.max_timer_lateness_us = self.max_timer_lateness_us.max(stats.max_timer_lateness_us);
        self.last = stats;
    }

    pub fn begin_generation(&mut self, generation: u64, stats: DriverStats) {
        self.generation = generation;
        self.last = stats;
        self.max_timer_lateness_us = self.max_timer_lateness_us.max(stats.max_timer_lateness_us);
    }

    pub fn to_json(&self) -> serde_json::Value {
        let endpoints = self
            .first
            .endpoints
            .iter()
            .zip(self.last.endpoints.iter())
            .enumerate()
            .map(|(endpoint, (before, after))| {
                serde_json::json!({
                    "endpoint": endpoint,
                    "start_total_bytes": before.total_bytes,
                    "end_total_bytes": after.total_bytes,
                    "streamed_bytes": self.streamed_bytes[endpoint],
                    "start_overruns": before.overruns,
                    "end_overruns": after.overruns,
                    "overruns": self.overruns[endpoint],
                    "start_lost_bytes": before.lost_bytes,
                    "end_lost_bytes": after.lost_bytes,
                    "lost_bytes": self.lost_bytes[endpoint],
                    "epoch": after.epoch,
                    "state": after.state,
                })
            })
            .collect::<Vec<_>>();
        serde_json::json!({
            "start_open_count": self.first.open_count,
            "end_open_count": self.last.open_count,
            "max_timer_lateness_us": self.max_timer_lateness_us,
            "endpoints": endpoints,
        })
    }
}

pub(crate) type SharedStatsSource = Arc<Mutex<Box<dyn DriverStatsSource>>>;
pub(crate) type SharedStatsTotals = Arc<Mutex<DriverStatsTotals>>;

#[cfg(test)]
mod tests {
    use std::mem::size_of;

    use ssr_abi::EndpointStats;

    use super::*;

    fn stats(total_bytes: u64) -> DriverStats {
        DriverStats {
            struct_size: size_of::<DriverStats>() as u32,
            open_count: 1,
            max_timer_lateness_us: total_bytes as u32,
            reserved: 0,
            endpoints: [EndpointStats {
                total_bytes,
                overruns: total_bytes / 10,
                lost_bytes: total_bytes / 5,
                epoch: 1,
                state: 1,
            }; ssr_abi::MAX_ENDPOINTS],
        }
    }

    #[test]
    fn driver_stats_json_keeps_stable_keys() {
        let report = DriverStatsTotals::new(0, stats(100)).to_json();
        let keys = report
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            keys,
            [
                "start_open_count",
                "end_open_count",
                "max_timer_lateness_us",
                "endpoints",
            ]
            .into_iter()
            .collect()
        );
        let endpoint_keys = report["endpoints"][0]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            endpoint_keys,
            [
                "endpoint",
                "start_total_bytes",
                "end_total_bytes",
                "streamed_bytes",
                "start_overruns",
                "end_overruns",
                "overruns",
                "start_lost_bytes",
                "end_lost_bytes",
                "lost_bytes",
                "epoch",
                "state",
            ]
            .into_iter()
            .collect()
        );
    }

    #[test]
    fn totals_span_generations_and_ignore_stale_samples() {
        let mut totals = DriverStatsTotals::new(0, stats(100));
        totals.observe(0, stats(150));
        totals.begin_generation(1, stats(20));
        totals.observe(0, stats(200));
        totals.observe(1, stats(50));

        assert_eq!(totals.last, stats(50));
        assert_eq!(totals.streamed_bytes, [80; ssr_abi::MAX_ENDPOINTS]);
        assert_eq!(totals.overruns, [8; ssr_abi::MAX_ENDPOINTS]);
        assert_eq!(totals.lost_bytes, [16; ssr_abi::MAX_ENDPOINTS]);
        assert_eq!(
            totals.to_json()["endpoints"][0]["lost_bytes"],
            serde_json::json!(16)
        );
    }
}
