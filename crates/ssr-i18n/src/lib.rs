#![forbid(unsafe_code)]
//! Generated, compile-time localization functions for Spatial SoftReceiver.
//!
//! The source catalogs live in `i18n/ko.toml` and `i18n/en.toml`; applications
//! do not parse them at runtime.

mod catalog;
#[cfg(feature = "clap")]
pub mod clap_help;

pub use catalog::*;

use ssr_types::app::Language;

/// Accept Windows language tags and POSIX locales without their encoding or
/// modifier. Korean without a region also denotes the Korean UI.
pub fn language_from_locale(locale: Option<&str>) -> Language {
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

/// Chooses the language from the platform UI locale.
pub fn system_language() -> Language {
    language_from_locale(sys_locale::get_locale().as_deref())
}

/// Chooses the command-line language, allowing an explicit `SSR_LANG`
/// override before falling back to the platform UI locale.
pub fn cli_language() -> Language {
    cli_language_from(
        std::env::var("SSR_LANG").ok().as_deref(),
        sys_locale::get_locale().as_deref(),
    )
}

fn cli_language_from(override_language: Option<&str>, locale: Option<&str>) -> Language {
    match override_language {
        Some("ko") => Language::Ko,
        Some("en") => Language::En,
        _ => language_from_locale(locale),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_override_accepts_only_ko_and_en_then_uses_the_locale() {
        assert_eq!(cli_language_from(Some("ko"), Some("en-US")), Language::Ko);
        assert_eq!(cli_language_from(Some("en"), Some("ko-KR")), Language::En);
        assert_eq!(cli_language_from(Some("KO"), Some("ko-KR")), Language::Ko);
        assert_eq!(cli_language_from(Some("ja"), Some("en-US")), Language::En);
        assert_eq!(cli_language_from(None, Some("ko_KR.UTF-8")), Language::Ko);
    }

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
            assert_eq!(language_from_locale(Some(locale)), Language::Ko, "{locale}");
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
            assert_eq!(language_from_locale(locale), Language::En, "{locale:?}");
        }
    }
}
