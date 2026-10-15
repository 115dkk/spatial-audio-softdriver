//! Reading `pnputil /enum-drivers`: which published driver packages
//! (`oemN.inf`) came from our INFs, so `uninstall` can delete them with
//! `pnputil /delete-driver <published> /uninstall /force` (as
//! `tools/testkit/uninstall.ps1` does with `Get-WindowsDriver`).
//!
//! pnputil translates its labels ("Published Name:", "Original Name:") into
//! the display language, so they are not matched. Every package is a block of
//! `label: value` lines, separated by blank lines, whose first two values are
//! the published name and the original name; the published name is found by
//! its shape (`oem<digits>.inf`) and the original name is the value after it.

/// One package of the driver store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DriverPackage {
    /// `oemN.inf`, the name `/delete-driver` takes.
    pub published: String,
    /// The INF's file name as the package shipped it.
    pub original: String,
}

/// Every package in `pnputil /enum-drivers` output.
pub fn parse_enum_drivers(output: &str) -> Vec<DriverPackage> {
    let mut packages = Vec::new();
    let mut values = Vec::new();
    for line in output.lines().chain(std::iter::once("")) {
        if line.trim().is_empty() {
            packages.extend(package_of_block(&values));
            values.clear();
        } else if let Some((_, value)) = line.split_once([':', '：']) {
            values.push(value.trim());
        }
    }
    packages
}

/// The published names of the packages whose original INF satisfies `ours`.
pub fn published_names(output: &str, ours: impl Fn(&str) -> bool) -> Vec<String> {
    parse_enum_drivers(output)
        .into_iter()
        .filter(|package| ours(&package.original))
        .map(|package| package.published)
        .collect()
}

fn package_of_block(values: &[&str]) -> Option<DriverPackage> {
    let position = values.iter().position(|value| is_published_name(value))?;
    let original = values.get(position + 1)?;
    let is_inf = original
        .len()
        .checked_sub(4)
        .and_then(|start| original.get(start..))
        .is_some_and(|extension| extension.eq_ignore_ascii_case(".inf"));
    is_inf.then(|| DriverPackage {
        published: values[position].to_owned(),
        original: (*original).to_owned(),
    })
}

/// `oem<digits>.inf`, without regard to case.
fn is_published_name(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower
        .strip_prefix("oem")
        .and_then(|rest| rest.strip_suffix(".inf"))
        .is_some_and(|digits| {
            !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ENGLISH: &str = "Microsoft PnP Utility\r
\r
Published Name:     oem3.inf\r
Original Name:      prnms001.inf\r
Provider Name:      Microsoft\r
Class Name:         Printers\r
Class GUID:         {4d36e979-e325-11ce-bfc1-08002be10318}\r
Driver Version:     06/21/2006 10.0.22621.1\r
Signer Name:        Microsoft Windows\r
\r
Published Name:     oem12.inf\r
Original Name:      spatialsoftreceiver.inf\r
Provider Name:      Spatial SoftReceiver\r
Class Name:         Sound, video and game controllers\r
Class GUID:         {4d36e96c-e325-11ce-bfc1-08002be10318}\r
Driver Version:     10/14/2026 0.7.8.0\r
Signer Name:        Spatial SoftReceiver test signing 0.7.8\r
\r
Published Name:     OEM40.INF\r
Original Name:      BioAtmos.inf\r
Provider Name:      BIO\r
\r
";

    const KOREAN: &str = "Microsoft PnP 유틸리티

게시된 이름:        oem12.inf
원래 이름:          spatialsoftreceiver.inf
공급자 이름:        Spatial SoftReceiver
클래스 이름:        사운드, 비디오 및 게임 컨트롤러
";

    #[test]
    fn enum_drivers_lists_every_package_with_its_original_inf() {
        assert_eq!(
            parse_enum_drivers(ENGLISH),
            [
                DriverPackage {
                    published: "oem3.inf".into(),
                    original: "prnms001.inf".into()
                },
                DriverPackage {
                    published: "oem12.inf".into(),
                    original: "spatialsoftreceiver.inf".into()
                },
                DriverPackage {
                    published: "OEM40.INF".into(),
                    original: "BioAtmos.inf".into()
                },
            ]
        );
    }

    #[test]
    fn enum_drivers_does_not_depend_on_the_label_language() {
        assert_eq!(
            published_names(KOREAN, |inf| inf
                .eq_ignore_ascii_case("spatialsoftreceiver.inf")),
            ["oem12.inf"]
        );
        let full_width = "게시된 이름： oem7.inf\n원래 이름： bioatmos.inf\n";
        assert_eq!(published_names(full_width, |_| true), ["oem7.inf"]);
    }

    #[test]
    fn published_names_keep_only_our_infs() {
        let ours = |inf: &str| {
            inf.eq_ignore_ascii_case("spatialsoftreceiver.inf")
                || inf.eq_ignore_ascii_case("bioatmos.inf")
        };
        assert_eq!(published_names(ENGLISH, ours), ["oem12.inf", "OEM40.INF"]);
        assert!(published_names("", ours).is_empty());
        assert!(published_names("Microsoft PnP Utility\n\nNo driver packages", ours).is_empty());
    }

    #[test]
    fn blocks_without_a_published_and_original_inf_are_skipped() {
        for block in [
            "Published Name: oem.inf\nOriginal Name: spatialsoftreceiver.inf\n",
            "Published Name: oem1x.inf\nOriginal Name: spatialsoftreceiver.inf\n",
            "Published Name: oem1.inf\n",
            "Published Name: oem1.inf\nOriginal Name: spatialsoftreceiver\n",
            "Original Name: spatialsoftreceiver.inf\n",
        ] {
            assert!(parse_enum_drivers(block).is_empty(), "{block}");
        }
    }
}
