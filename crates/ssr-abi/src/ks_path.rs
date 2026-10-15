//! KS filter reference strings: which of our endpoints a render device is.
//!
//! Windows names an endpoint's KS filter in the device-topology id that
//! `IConnector::GetDeviceIdConnectedTo` returns, for example
//! `{2}.\\?\root#media#0000#{6994ad04-93ef-11d0-a3cc-00a0c9223196}\topologyhdmi0`.
//! The adapter's PnP instance id is in it with `#` for `\`, and the reference
//! string after the interface class GUID is the driver's (`minipairs.h`):
//! `TopologyHdmiN` or `WaveHdmiN` for endpoint N. Display names never select
//! an endpoint (CLAUDE.md); this rule does, and it is the one
//! `tools/ci/endpoints.ps1` (`MapFilter`) applies, so the app, `ssr-setup` and
//! the CI tools agree on every device.

/// Reference-string stems of our endpoints' KS filters, compared without case.
const REFERENCE_STEMS: [&str; 2] = ["TopologyHdmi", "WaveHdmi"];

/// Endpoint numbers a reference string may carry (`[0-2]` in `MapFilter`).
const HIGHEST_ENDPOINT: u8 = b'2';

/// Maps a device-topology id to our endpoint number.
///
/// `path` must be a complete reference: an optional `{n}.` prefix, `\\?\` or
/// `\??\`, the owning adapter's instance id with `#` for `\`, `#{guid}\`, then
/// `TopologyHdmi` or `WaveHdmi` and one digit 0 to 2, all without regard to
/// case. The adapter must be one of `owners` (PnP instance ids, compared
/// without case): another vendor's `TopologyHdmi0` is not our endpoint 0.
/// Anything else is `None`.
pub fn map_filter<S: AsRef<str>>(path: &str, owners: &[S]) -> Option<u16> {
    let rest = strip_topology_prefix(path)?;
    let rest = rest
        .strip_prefix(r"\\?\")
        .or_else(|| rest.strip_prefix(r"\??\"))?;
    // `([^{}]+)#\{`: the owner runs to the first brace, which must be the `{`
    // of the interface GUID, right after a `#`.
    let brace = rest.find(['{', '}'])?;
    if !rest[brace..].starts_with('{') {
        return None;
    }
    let owner = rest[..brace].strip_suffix('#')?;
    if owner.is_empty() {
        return None;
    }
    let rest = &rest[brace + 1..];
    let guid = rest.get(..36)?;
    if !guid
        .bytes()
        .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
    {
        return None;
    }
    let reference = rest[36..].strip_prefix("}\\")?;
    let endpoint = endpoint_of_reference(reference)?;
    let owner = owner.replace('#', "\\");
    owners
        .iter()
        .any(|expected| equal_ignoring_case(&owner, expected.as_ref()))
        .then_some(endpoint)
}

/// Drops the optional `{n}.` prefix. A leading `{` that does not open such a
/// prefix cannot start a reference either.
fn strip_topology_prefix(path: &str) -> Option<&str> {
    let Some(inner) = path.strip_prefix('{') else {
        return Some(path);
    };
    let close = inner.find('}')?;
    let digits = &inner[..close];
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    inner[close + 1..].strip_prefix('.')
}

/// `(?:TopologyHdmi|WaveHdmi)([0-2])$` without regard to case.
fn endpoint_of_reference(reference: &str) -> Option<u16> {
    REFERENCE_STEMS.iter().find_map(|stem| {
        let head = reference.get(..stem.len())?;
        if !head.eq_ignore_ascii_case(stem) {
            return None;
        }
        match &reference.as_bytes()[stem.len()..] {
            [digit @ b'0'..=HIGHEST_ENDPOINT] => Some(u16::from(digit - b'0')),
            _ => None,
        }
    })
}

/// Ordinal comparison without case, as .NET's `OrdinalIgnoreCase` does it for
/// the ids Windows produces.
fn equal_ignoring_case(left: &str, right: &str) -> bool {
    left.chars()
        .flat_map(char::to_uppercase)
        .eq(right.chars().flat_map(char::to_uppercase))
}

#[cfg(test)]
mod tests {
    use super::*;

    const OURS: [&str; 1] = [r"ROOT\MEDIA\0000"];
    const GUID: &str = "6994ad04-93ef-11d0-a3cc-00a0c9223196";

    fn reference(prefix: &str, owner: &str, tail: &str) -> String {
        format!("{prefix}{owner}#{{{GUID}}}\\{tail}")
    }

    #[test]
    fn map_filter_reads_the_endpoint_from_a_prefixed_topology_id() {
        let path = reference(r"{2}.\\?\", "root#media#0000", "topologyhdmi0");
        assert_eq!(map_filter(&path, &OURS), Some(0));
        let path = reference(r"{12}.\\?\", "ROOT#MEDIA#0000", "TopologyHdmi1");
        assert_eq!(map_filter(&path, &OURS), Some(1));
    }

    #[test]
    fn map_filter_accepts_both_slash_forms_and_the_wave_filter() {
        let path = reference(r"\\?\", "ROOT#MEDIA#0000", "WaveHdmi2");
        assert_eq!(map_filter(&path, &OURS), Some(2));
        let path = reference(r"\??\", "ROOT#MEDIA#0000", "WaveHdmi2");
        assert_eq!(map_filter(&path, &OURS), Some(2));
        let path = reference(r"{2}.\??\", "ROOT#MEDIA#0000", "TopologyHdmi0");
        assert_eq!(map_filter(&path, &OURS), Some(0));
    }

    #[test]
    fn map_filter_refuses_another_vendors_filter_with_our_reference_string() {
        let path = reference(
            r"{2}.\\?\",
            "HDAUDIO#FUNC_01&VEN_10DE&DEV_0084&SUBSYS_10DE0000&REV_1001#5&1a2b3c4d&0&0001",
            "TopologyHdmi0",
        );
        assert_eq!(map_filter(&path, &OURS), None);
        assert_eq!(
            map_filter(
                &path,
                &[r"HDAUDIO\FUNC_01&VEN_10DE&DEV_0084&SUBSYS_10DE0000&REV_1001\5&1a2b3c4d&0&0001"]
            ),
            Some(0),
            "the same path maps once its adapter is an owner"
        );
        let empty: [&str; 0] = [];
        let path = reference(r"\\?\", "ROOT#MEDIA#0000", "TopologyHdmi0");
        assert_eq!(map_filter(&path, &empty), None);
    }

    #[test]
    fn map_filter_refuses_endpoint_three_and_trailing_text() {
        for tail in [
            "TopologyHdmi3",
            "WaveHdmi3",
            "TopologyHdmi01",
            "TopologyHdmi",
            "TopologyHdmi0 ",
            "TopologySpeaker0",
            "WaveHdmi٠",
        ] {
            let path = reference(r"\\?\", "ROOT#MEDIA#0000", tail);
            assert_eq!(map_filter(&path, &OURS), None, "{tail}");
        }
    }

    #[test]
    fn map_filter_ignores_case_everywhere() {
        let path = format!(
            r"{{2}}.\\?\Root#Media#0000#{{{}}}\TOPOLOGYHDMI1",
            GUID.to_uppercase()
        );
        assert_eq!(map_filter(&path, &[r"root\media\0000"]), Some(1));
        let path = reference(r"\\?\", "root#media#0000", "wavehdmi0");
        assert_eq!(map_filter(&path, &OURS), Some(0));
    }

    #[test]
    fn map_filter_refuses_malformed_paths() {
        let good_tail = format!("#{{{GUID}}}\\TopologyHdmi0");
        for path in [
            String::new(),
            format!("ROOT#MEDIA#0000{good_tail}"),
            format!(r"\\.\ROOT#MEDIA#0000{good_tail}"),
            format!(r"{{x}}.\\?\ROOT#MEDIA#0000{good_tail}"),
            format!(r"{{}}.\\?\ROOT#MEDIA#0000{good_tail}"),
            format!(r"{{2}}\\?\ROOT#MEDIA#0000{good_tail}"),
            format!(r"\\?\{good_tail}"),
            format!(r"\\?\ROOT#MEDIA#0000{{{GUID}}}\TopologyHdmi0"),
            format!(r"\\?\ROOT#ME}}DIA#0000{good_tail}"),
            r"\\?\ROOT#MEDIA#0000#{6994ad04-93ef-11d0-a3cc-00a0c922319}\TopologyHdmi0".into(),
            r"\\?\ROOT#MEDIA#0000#{6994ad04-93ef-11d0-a3cc-00a0c92231966}\TopologyHdmi0".into(),
            r"\\?\ROOT#MEDIA#0000#{6994ad04-93ef-11d0-a3cc-00a0c922319g}\TopologyHdmi0".into(),
            format!(r"\\?\ROOT#MEDIA#0000#{{{GUID}}}TopologyHdmi0"),
        ] {
            assert_eq!(map_filter(&path, &OURS), None, "{path}");
        }
    }

    #[test]
    fn map_filter_compares_the_whole_owner() {
        let path = reference(r"\\?\", "ROOT#MEDIA#00001", "TopologyHdmi0");
        assert_eq!(map_filter(&path, &OURS), None);
        let path = reference(r"\\?\", "X#ROOT#MEDIA#0000", "TopologyHdmi0");
        assert_eq!(map_filter(&path, &OURS), None);
    }
}
