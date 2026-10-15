//! The exclusive-mode policy of our endpoints (ADR 0028), shared by the app's
//! host, which reads it, and `ssr-setup`, which writes it (install, repair,
//! set-formats) and clears it (uninstall). Ported from
//! `tools/ci/exclusive-policy.ps1` (`Set-SsrExclusivePolicy`,
//! `Clear-SsrExclusivePolicy`).
//!
//! The policy is two private endpoint properties under
//! `{b3f8fa53-0004-438e-9003-51a46e139bfc}`, not published Windows SDK keys:
//! pid 3 allows exclusive mode and pid 4 gives exclusive streams priority, both
//! VT_UI4. The primary and music endpoints are shared-only (0) and the
//! passthrough endpoint is exclusive (1). This module holds the keys and the
//! decisions; the Windows crates do the property store calls.

use ssr_types::ports::PolicyState;

use crate::ENDPOINT_PASSTHROUGH;

/// The property set of the two policy values.
pub const POLICY_FMTID: u128 = 0xb3f8fa53_0004_438e_9003_51a46e139bfc;
/// "Allow applications to take exclusive control of this device".
pub const ALLOW_EXCLUSIVE_PID: u32 = 3;
/// "Give exclusive mode applications priority".
pub const EXCLUSIVE_PRIORITY_PID: u32 = 4;
/// The driver's `Parameters` value that enables endpoints, bit n for
/// endpoint n (see `setup_contract::parameters_key`).
pub const ENDPOINT_MASK_VALUE: &str = "EndpointMask";

/// The endpoints the package exposes: one KS mini-pair each.
pub const POLICY_MASK: u8 = 0x7;

/// The mask the policy covers for the driver's `EndpointMask` parameter:
/// limited to the endpoints the package has, and all of them when the
/// parameter is missing or selects none (`tools/testkit/install.ps1`).
pub fn install_mask(parameter: Option<u32>) -> u8 {
    let mask = match parameter {
        None | Some(0) => u32::from(POLICY_MASK),
        Some(mask) => mask,
    } & u32::from(POLICY_MASK);
    if mask == 0 { POLICY_MASK } else { mask as u8 }
}

/// The endpoints of `mask` the policy is set for, in order.
pub fn policy_endpoints(mask: u8) -> impl Iterator<Item = u16> {
    (0..3_u16).filter(move |endpoint| mask & (1 << endpoint) != 0)
}

/// The value of both properties for `endpoint`: 1 on the passthrough
/// endpoint, 0 on the shared-only ones.
pub fn policy_value(endpoint: u16) -> u32 {
    u32::from(endpoint == ENDPOINT_PASSTHROUGH)
}

/// What one endpoint's property store holds; `None` is an absent value
/// (VT_EMPTY).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PolicyValues {
    pub allow_exclusive: Option<u32>,
    pub exclusive_priority: Option<u32>,
}

impl PolicyValues {
    /// Both values removed, as `uninstall` leaves them.
    pub const CLEARED: Self = Self {
        allow_exclusive: None,
        exclusive_priority: None,
    };

    /// What `ssr-setup` writes on `endpoint`.
    pub fn expected(endpoint: u16) -> Self {
        let value = Some(policy_value(endpoint));
        Self {
            allow_exclusive: value,
            exclusive_priority: value,
        }
    }
}

/// The host's verdict from our active endpoints: each one's number and what
/// its store held, or `None` when the read failed. Only the endpoints `mask`
/// covers count. `Missing` when one of them holds other values than
/// [`PolicyValues::expected`]; otherwise `Unknown` when none of them is active
/// or a read failed; otherwise `Applied`.
pub fn policy_state(mask: u8, active: &[(u16, Option<PolicyValues>)]) -> PolicyState {
    let covered: Vec<_> = active
        .iter()
        .filter(|(endpoint, _)| policy_endpoints(mask).any(|covered| covered == *endpoint))
        .collect();
    let missing = covered.iter().any(|(endpoint, values)| {
        values.is_some_and(|values| values != PolicyValues::expected(*endpoint))
    });
    if missing {
        PolicyState::Missing
    } else if covered.is_empty() || covered.iter().any(|(_, values)| values.is_none()) {
        PolicyState::Unknown
    } else {
        PolicyState::Applied
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_mask_defaults_to_every_packaged_endpoint() {
        assert_eq!(install_mask(None), 7);
        assert_eq!(install_mask(Some(0)), 7);
        assert_eq!(install_mask(Some(5)), 5);
        assert_eq!(install_mask(Some(0xF)), 7);
        assert_eq!(
            install_mask(Some(8)),
            7,
            "only an endpoint without a mini-pair"
        );
    }

    #[test]
    fn policy_values_open_only_the_passthrough_endpoint() {
        assert_eq!(policy_endpoints(7).collect::<Vec<_>>(), [0, 1, 2]);
        assert_eq!(policy_endpoints(5).collect::<Vec<_>>(), [0, 2]);
        assert_eq!(policy_endpoints(8).count(), 0);
        assert_eq!(policy_value(0), 0);
        assert_eq!(policy_value(1), 0);
        assert_eq!(policy_value(2), 1);
        assert_eq!(
            PolicyValues::expected(2),
            PolicyValues {
                allow_exclusive: Some(1),
                exclusive_priority: Some(1)
            }
        );
    }

    #[test]
    fn the_policy_is_applied_when_every_covered_active_endpoint_has_it() {
        let all = [
            (0, Some(PolicyValues::expected(0))),
            (1, Some(PolicyValues::expected(1))),
            (2, Some(PolicyValues::expected(2))),
        ];
        assert_eq!(policy_state(7, &all), PolicyState::Applied);
        // An endpoint that is not active yet is not judged.
        assert_eq!(policy_state(7, &all[..2]), PolicyState::Applied);
        // An endpoint the mask does not cover is not judged either.
        let extra = [
            (0, Some(PolicyValues::expected(0))),
            (2, Some(PolicyValues::CLEARED)),
        ];
        assert_eq!(policy_state(1, &extra), PolicyState::Applied);
    }

    #[test]
    fn an_endpoint_without_the_values_makes_the_policy_missing() {
        let cleared = [
            (0, Some(PolicyValues::expected(0))),
            (2, Some(PolicyValues::CLEARED)),
        ];
        assert_eq!(policy_state(7, &cleared), PolicyState::Missing);
        let half = [(
            1,
            Some(PolicyValues {
                allow_exclusive: Some(0),
                exclusive_priority: None,
            }),
        )];
        assert_eq!(policy_state(7, &half), PolicyState::Missing);
        let swapped = [(0, Some(PolicyValues::expected(2)))];
        assert_eq!(policy_state(7, &swapped), PolicyState::Missing);
        // A failed read elsewhere does not hide what is known to be missing.
        let with_failure = [(0, None), (1, Some(PolicyValues::CLEARED))];
        assert_eq!(policy_state(7, &with_failure), PolicyState::Missing);
    }

    #[test]
    fn nothing_active_or_a_failed_read_leaves_the_policy_unknown() {
        assert_eq!(policy_state(7, &[]), PolicyState::Unknown);
        assert_eq!(
            policy_state(1, &[(2, Some(PolicyValues::expected(2)))]),
            PolicyState::Unknown
        );
        let failed = [(0, Some(PolicyValues::expected(0))), (1, None)];
        assert_eq!(policy_state(7, &failed), PolicyState::Unknown);
    }
}
