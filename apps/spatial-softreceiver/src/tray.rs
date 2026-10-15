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
        match (self, language) {
            (Self::Disable, Language::Ko) => "끄기",
            (Self::Enable | Self::Unavailable, Language::Ko) => "켜기",
            (Self::Disable, Language::En) => "Disable",
            (Self::Enable | Self::Unavailable, Language::En) => "Enable",
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
    match language {
        Language::Ko => "열기",
        Language::En => "Open",
    }
}

pub fn quit(language: Language) -> &'static str {
    match language {
        Language::Ko => "종료",
        Language::En => "Quit",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ssr_types::app::SetupStep;

    #[test]
    fn every_label_follows_language_and_actions_stay_the_same() {
        for (language, labels) in [
            (Language::Ko, ["열기", "켜기", "끄기", "종료"]),
            (Language::En, ["Open", "Enable", "Disable", "Quit"]),
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
