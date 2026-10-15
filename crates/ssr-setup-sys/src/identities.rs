//! The driver package's identities, current and legacy, as
//! `tools/ci/package-identities.ps1` lists them. The test kit's scripts and
//! `ssr-setup` must agree on every field, and
//! `identities_match_the_powershell_table` checks that they do.

use ssr_abi::setup_contract::{PACKAGE_HARDWARE_ID, PACKAGE_INF, PACKAGE_SERVICE};

/// One driver package: what identifies its device node, service, INF and
/// test-signing certificate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Package {
    pub hardware_id: &'static str,
    pub service: &'static str,
    pub inf: &'static str,
    /// Certificates whose subject starts with this (without case, as
    /// PowerShell's `-like` compares) are this package's.
    pub certificate_subject_prefix: &'static str,
}

/// The package `ssr-setup install` installs.
pub const CURRENT: Package = Package {
    hardware_id: PACKAGE_HARDWARE_ID,
    service: PACKAGE_SERVICE,
    inf: PACKAGE_INF,
    certificate_subject_prefix: "CN=Spatial SoftReceiver test signing",
};

/// Earlier test packages: `install` refuses while one is present and
/// `uninstall` removes them too.
pub const LEGACY: &[Package] = &[Package {
    hardware_id: r"ROOT\BioAtmos",
    service: "bioatmos",
    inf: "bioatmos.inf",
    certificate_subject_prefix: "CN=BIO Atmos test signing",
}];

/// The current package, then the legacy ones.
pub fn all() -> impl Iterator<Item = &'static Package> {
    std::iter::once(&CURRENT).chain(LEGACY)
}

impl Package {
    /// Whether a device node with these hardware ids is this package's.
    pub fn owns_device(&self, hardware_ids: &[String]) -> bool {
        hardware_ids
            .iter()
            .any(|id| id.eq_ignore_ascii_case(self.hardware_id))
    }

    /// Whether a driver package whose original INF is `inf` is this one's.
    pub fn owns_inf(&self, inf: &str) -> bool {
        inf.eq_ignore_ascii_case(self.inf)
    }

    /// Whether a certificate with this subject (`CN=…` first) is this
    /// package's test-signing certificate.
    pub fn owns_certificate(&self, subject: &str) -> bool {
        subject
            .get(..self.certificate_subject_prefix.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(self.certificate_subject_prefix))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::path::Path;

    /// The `Key = 'Value'` lines of each `[pscustomobject]@{ … }` in the file.
    fn powershell_objects(text: &str) -> Vec<BTreeMap<String, String>> {
        let mut objects = Vec::new();
        let mut current: Option<BTreeMap<String, String>> = None;
        for line in text.lines().map(str::trim) {
            if line.contains("[pscustomobject]@{") {
                current = Some(BTreeMap::new());
            } else if line.starts_with('}') {
                objects.extend(current.take());
            } else if let Some(object) = current.as_mut()
                && let Some((key, value)) = line.split_once('=')
            {
                let value = value.trim();
                let value = value
                    .strip_prefix('\'')
                    .and_then(|value| value.strip_suffix('\''))
                    .unwrap_or_else(|| panic!("not a single-quoted string: {line}"));
                object.insert(key.trim().to_owned(), value.to_owned());
            }
        }
        objects
    }

    fn assert_same(package: &Package, object: &BTreeMap<String, String>) {
        assert_eq!(object["HardwareId"], package.hardware_id);
        assert_eq!(object["Service"], package.service);
        assert_eq!(object["Inf"], package.inf);
        let subject = object["CertificateSubject"]
            .strip_suffix('*')
            .expect("the PowerShell subject is a -like pattern ending in *");
        assert_eq!(subject, package.certificate_subject_prefix);
        assert_eq!(
            object.len(),
            4,
            "a field this crate does not know: {object:?}"
        );
    }

    #[test]
    fn identities_match_the_powershell_table() {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/ci/package-identities.ps1");
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        let (current, legacy) = text
            .split_once("$SpatialLegacyPackages")
            .expect("package-identities.ps1 defines $SpatialLegacyPackages");
        let current = powershell_objects(current);
        let legacy = powershell_objects(legacy);
        assert_eq!(current.len(), 1, "one $SpatialPackage");
        assert_same(&CURRENT, &current[0]);
        assert_eq!(legacy.len(), LEGACY.len(), "legacy packages");
        for (package, object) in LEGACY.iter().zip(&legacy) {
            assert_same(package, object);
        }
    }

    #[test]
    fn packages_match_devices_infs_and_certificates_without_case() {
        assert!(CURRENT.owns_device(&[r"root\spatialsoftreceiver".to_owned()]));
        assert!(!CURRENT.owns_device(&[r"ROOT\SpatialSoftReceiver2".to_owned()]));
        assert!(LEGACY[0].owns_device(&["x".to_owned(), r"ROOT\BIOATMOS".to_owned()]));
        assert!(CURRENT.owns_inf("SpatialSoftReceiver.INF"));
        assert!(!CURRENT.owns_inf("oem12.inf"));
        assert!(CURRENT.owns_certificate("CN=Spatial SoftReceiver test signing 0.7.8"));
        assert!(CURRENT.owns_certificate("cn=spatial softreceiver TEST SIGNING"));
        assert!(!CURRENT.owns_certificate("CN=Spatial SoftReceiver"));
        assert!(!CURRENT.owns_certificate("O=x, CN=Spatial SoftReceiver test signing"));
        assert!(LEGACY[0].owns_certificate("CN=BIO Atmos test signing 0.6.0"));
        assert_eq!(all().count(), 1 + LEGACY.len());
        assert_eq!(all().next(), Some(&CURRENT));
    }
}
