//! Language at the platform boundary. The supervisor owns the app's choice;
//! the separate guard reads the same versioned settings when it notifies.

use std::fs;
use std::path::Path;

use serde::Deserialize;
use ssr_types::app::Language;

/// Rereads the envelope on every call: a running guard follows changes made
/// by the app, rather than keeping the language it launched with.
pub fn resolve_in(state_dir: &Path, default: Language) -> Language {
    fs::read_to_string(state_dir.join("settings.json"))
        .ok()
        .and_then(|text| explicit(&text))
        .unwrap_or(default)
}

fn explicit(text: &str) -> Option<Language> {
    #[derive(Deserialize)]
    struct Envelope {
        version: u64,
        settings: supervisor::Settings,
    }
    let envelope: Envelope = serde_json::from_str(text).ok()?;
    (envelope.version == 1)
        .then_some(envelope.settings.language)
        .flatten()
}

pub fn title(language: Language) -> &'static str {
    ssr_i18n::names::product(language)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_supported_envelope_with_explicit_language_overrides_the_os() {
        for (text, expected) in [
            (
                r#"{"version":1,"settings":{"language":"ko"}}"#,
                Some(Language::Ko),
            ),
            (
                r#"{"version":1,"settings":{"language":"en"}}"#,
                Some(Language::En),
            ),
            (r#"{"version":1,"settings":{"language":null}}"#, None),
            (r#"{"version":1,"settings":{}}"#, None),
            (r#"{"version":2,"settings":{"language":"ko"}}"#, None),
            (r#"{"version":1,"settings":{"language":"fr"}}"#, None),
            ("broken", None),
        ] {
            assert_eq!(explicit(text), expected, "{text}");
        }
        assert_eq!(title(Language::Ko), "공간 소프트리시버");
        assert_eq!(title(Language::En), "Spatial SoftReceiver");
    }
}
