//! What `uninstall` judges, as `tools/testkit/uninstall.ps1` does: the counts
//! of what stayed, which of them fail the task, the leftover endpoint entries
//! of a removed device (never deleted, only reported), and the exit codes of
//! `sc.exe delete` that mean the service is gone or going.

/// The endpoints Windows keeps under HKEY_LOCAL_MACHINE, one subkey each.
pub const MMDEVICES_RENDER_KEY: &str =
    r"SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Render";

/// The value in an endpoint's `Properties` subkey that names the device
/// instance that owns it: pid 2 of the policy's property set (REG_SZ).
pub const ENDPOINT_OWNER_VALUE: &str = "{b3f8fa53-0004-438e-9003-51a46e139bfc},2";

/// Exit codes of `sc.exe delete` that leave no service behind: done, no such
/// service (1060), already marked for deletion (1072).
pub fn service_delete_succeeded(code: Option<i32>) -> bool {
    matches!(code, Some(0 | 1060 | 1072))
}

/// Whether an endpoint entry whose owner value is `owner` belongs to one of
/// `instance_ids`: equal to one, or ending with `\` and one, without regard
/// to case. The topology is gone once the device is removed, so this saved
/// ownership is all that identifies the entry; names never do.
pub fn endpoint_owned_by<S: AsRef<str>>(owner: &str, instance_ids: &[S]) -> bool {
    instance_ids.iter().any(|id| {
        let id = id.as_ref();
        if id.is_empty() {
            return false;
        }
        if owner.eq_ignore_ascii_case(id) {
            return true;
        }
        owner
            .len()
            .checked_sub(id.len() + 1)
            .and_then(|start| owner.get(start..))
            .and_then(|tail| tail.strip_prefix('\\'))
            .is_some_and(|tail| tail.eq_ignore_ascii_case(id))
    })
}

/// What stayed after an uninstall, without what only waits for a reboot.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Remaining {
    pub devices: usize,
    pub driver_packages: usize,
    pub certificates: usize,
    pub service_keys: usize,
    pub endpoints: usize,
}

impl Remaining {
    /// Whether the uninstall failed. Service keys do not count: Windows may
    /// keep them until the next boot after the devices and packages are gone.
    pub fn failed(&self) -> bool {
        self.devices + self.driver_packages + self.certificates + self.endpoints > 0
    }

    /// The counts as `uninstall.ps1` prints them.
    pub fn summary(&self) -> String {
        format!(
            "devices={}, driver packages={}, certificates={}, service keys={}, endpoints={}",
            self.devices,
            self.driver_packages,
            self.certificates,
            self.service_keys,
            self.endpoints
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ssr_abi::exclusive_policy::POLICY_FMTID;

    #[test]
    fn endpoint_entries_are_matched_by_their_saved_owner() {
        let owners = [r"ROOT\MEDIA\0000".to_owned()];
        assert!(endpoint_owned_by(r"ROOT\MEDIA\0000", &owners));
        assert!(endpoint_owned_by(r"root\media\0000", &owners));
        assert!(endpoint_owned_by(r"{1}.SWD\X\ROOT\MEDIA\0000", &owners));
        assert!(!endpoint_owned_by(r"ROOT\MEDIA\00001", &owners));
        assert!(!endpoint_owned_by(r"XROOT\MEDIA\0000", &owners));
        assert!(!endpoint_owned_by(r"ROOT\MEDIA\0001", &owners));
        assert!(!endpoint_owned_by("", &owners));
        assert!(!endpoint_owned_by(r"ROOT\MEDIA\0000", &[""]));
        let none: [&str; 0] = [];
        assert!(!endpoint_owned_by(r"ROOT\MEDIA\0000", &none));
        assert!(endpoint_owned_by(
            r"ROOT\MEDIA\0001",
            &[r"ROOT\MEDIA\0000", r"ROOT\MEDIA\0001"]
        ));
    }

    #[test]
    fn the_owner_value_is_pid_2_of_the_policy_set() {
        let hex = format!("{POLICY_FMTID:032x}");
        let set = format!(
            "{{{}-{}-{}-{}-{}}}",
            &hex[..8],
            &hex[8..12],
            &hex[12..16],
            &hex[16..20],
            &hex[20..]
        );
        assert_eq!(ENDPOINT_OWNER_VALUE, format!("{set},2"));
    }

    #[test]
    fn sc_delete_accepts_gone_and_going_services() {
        for code in [0, 1060, 1072] {
            assert!(service_delete_succeeded(Some(code)), "{code}");
        }
        for code in [Some(5), Some(1), None] {
            assert!(!service_delete_succeeded(code), "{code:?}");
        }
    }

    #[test]
    fn remaining_counts_print_like_the_script_and_service_keys_do_not_fail() {
        let remaining = Remaining {
            devices: 1,
            driver_packages: 2,
            certificates: 3,
            service_keys: 4,
            endpoints: 5,
        };
        assert_eq!(
            remaining.summary(),
            "devices=1, driver packages=2, certificates=3, service keys=4, endpoints=5"
        );
        assert!(remaining.failed());
        assert!(!Remaining::default().failed());
        let only_service_keys = Remaining {
            service_keys: 1,
            ..Remaining::default()
        };
        assert!(!only_service_keys.failed());
        for stayed in [
            Remaining {
                devices: 1,
                ..Remaining::default()
            },
            Remaining {
                driver_packages: 1,
                ..Remaining::default()
            },
            Remaining {
                certificates: 1,
                ..Remaining::default()
            },
            Remaining {
                endpoints: 1,
                ..Remaining::default()
            },
        ] {
            assert!(stayed.failed(), "{stayed:?}");
        }
    }
}
