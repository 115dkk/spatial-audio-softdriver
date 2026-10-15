#![deny(unsafe_code)]

//! The administrator tasks `ssr-setup` runs elevated (ADR 0032): install,
//! uninstall, set-formats, restart-device and repair. An isolation crate
//! (ADR 0022): only `apps/ssr-setup` depends on it, so the app itself never
//! links code that needs an administrator.
//!
//! * [`identities`] (every OS): the current and legacy driver packages, as
//!   `tools/ci/package-identities.ps1` lists them.
//! * [`driver_store`] (every OS): which driver store packages are ours, read
//!   from `pnputil /enum-drivers`.
//! * [`removal`] (every OS): what `uninstall` counts as staying, and the
//!   leftover endpoint entries it reports.
//! * `tasks` (Windows): `tasks::run`, one `TaskLine` to one `TaskResult`.
//! * `endpoints` (Windows): our endpoints by KS filter reference.
//! * `win` (Windows): every `unsafe` call, each in a small safe function
//!   named after its API.
//!
//! The exclusive-mode policy's keys and values are `ssr_abi::exclusive_policy`,
//! shared with the app's host, which reads them.

pub mod driver_store;
pub mod identities;
pub mod removal;

#[cfg(windows)]
mod endpoints;
#[cfg(windows)]
pub mod tasks;
#[cfg(windows)]
mod win;

#[cfg(test)]
mod tests {
    use ssr_abi::exclusive_policy::{PolicyValues, install_mask, policy_endpoints};

    /// What `install` and `repair` write for a default `EndpointMask`: the
    /// primary and music endpoints shared-only, the passthrough exclusive.
    #[test]
    fn only_the_passthrough_endpoint_allows_exclusive_mode() {
        let written: Vec<(u16, PolicyValues)> = policy_endpoints(install_mask(None))
            .map(|endpoint| (endpoint, PolicyValues::expected(endpoint)))
            .collect();
        let value = |n| PolicyValues {
            allow_exclusive: Some(n),
            exclusive_priority: Some(n),
        };
        assert_eq!(written, [(0, value(0)), (1, value(0)), (2, value(1))]);
    }
}
