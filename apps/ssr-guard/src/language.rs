//! Language at the platform boundary. The supervisor owns the app's choice;
//! the separate guard reads the same versioned settings when it notifies.

use std::fs;
use std::path::Path;

use serde::Deserialize;
use ssr_types::app::Language;

/// Accept Windows language tags and POSIX locales without their encoding or
/// modifier. Korean without a region also denotes the Korean UI.
pub fn from_locale(locale: Option<&str>) -> Language {
    let tag = locale.unwrap_or_default().trim();
    let tag = tag.split(['.', '@']).next().unwrap_or_default();
    if tag.eq_ignore_ascii_case("ko")
        || tag.eq_ignore_ascii_case("ko-KR")
        || tag.eq_ignore_ascii_case("ko_KR")
    {
        Language::Ko
    } else {
        Language::En
    }
}

pub fn system() -> Language {
    from_locale(sys_locale::get_locale().as_deref())
}

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
    match language {
        Language::Ko => "공간 소프트리시버",
        Language::En => "Spatial SoftReceiver",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locales_are_normalized_and_unknown_defaults_to_english() {
        for locale in [
            "ko-KR",
            "KO-kr",
            "ko_KR.UTF-8",
            "ko_KR@variant",
            "ko",
            " ko-KR ",
        ] {
            assert_eq!(from_locale(Some(locale)), Language::Ko, "{locale}");
        }
        for locale in [
            None,
            Some(""),
            Some("en-US"),
            Some("ja-JP"),
            Some("C"),
            Some("kok-IN"),
            Some("ko-KP"),
        ] {
            assert_eq!(from_locale(locale), Language::En, "{locale:?}");
        }
    }

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
