//! Platform-independent tray labels and actions, drawn from the snapshot.

use ssr_types::app::{Intent, Language, Phase};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Toggle {
    Disable,
    Enable,
    Unavailable,
}

impl Toggle {
    pub fn of(phase: &Phase) -> Self {
        match phase {
            Phase::Running => Self::Disable,
            Phase::Ready | Phase::Faulted => Self::Enable,
            Phase::Setup(_) | Phase::Quit => Self::Unavailable,
        }
    }

    pub fn text(self, language: Language) -> &'static str {
        match self {
            Self::Disable => ssr_i18n::common::turn_off(language),
            Self::Enable | Self::Unavailable => ssr_i18n::common::turn_on(language),
        }
    }

    pub fn intent(self) -> Option<Intent> {
        match self {
            Self::Disable => Some(Intent::Disable),
            Self::Enable => Some(Intent::Enable),
            Self::Unavailable => None,
        }
    }
}

pub fn open(language: Language) -> &'static str {
    ssr_i18n::shell::tray_open(language)
}

pub fn quit(language: Language) -> &'static str {
    ssr_i18n::shell::tray_quit(language)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ssr_types::app::SetupStep;

    #[test]
    fn every_label_follows_language_and_actions_stay_the_same() {
        for (language, labels) in [
            (Language::Ko, ["열기", "켜기", "끄기", "종료"]),
            (Language::En, ["Open", "Turn on", "Turn off", "Quit"]),
        ] {
            assert_eq!(open(language), labels[0]);
            assert_eq!(quit(language), labels[3]);
            for phase in [Phase::Ready, Phase::Faulted] {
                let toggle = Toggle::of(&phase);
                assert_eq!(toggle.text(language), labels[1]);
                assert_eq!(toggle.intent(), Some(Intent::Enable));
            }
            let toggle = Toggle::of(&Phase::Running);
            assert_eq!(toggle.text(language), labels[2]);
            assert_eq!(toggle.intent(), Some(Intent::Disable));
            for phase in [Phase::Quit, Phase::Setup(SetupStep::Welcome)] {
                let toggle = Toggle::of(&phase);
                assert_eq!(toggle.text(language), labels[1]);
                assert_eq!(toggle.intent(), None);
            }
        }
    }
}
