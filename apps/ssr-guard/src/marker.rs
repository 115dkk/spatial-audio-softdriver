use std::fs;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

pub const FILE: &str = "guard-restored.json";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarkerReason {
    /// The app exited with its restore point still there.
    AppExited,
    /// The engine stalled; the app was restarted once.
    StallRestarted,
    /// It stalled again after the restart, or the restart did not start.
    RestartFailed,
    /// The app held the roles but never said it was Running (ADR 0033's
    /// takeover silence).
    Stuck,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Marker {
    pub version: u32,
    pub reason: MarkerReason,
    pub at_unix: u64,
    pub notes: Vec<String>,
}

impl Marker {
    pub const VERSION: u32 = 1;

    pub fn new(reason: MarkerReason, at_unix: u64, notes: Vec<String>) -> Self {
        Self {
            version: Self::VERSION,
            reason,
            at_unix,
            notes,
        }
    }
}

/// The marker stays language-neutral on disk; render it when it is shown.
pub fn sentence(marker: &Marker, language: ssr_types::app::Language) -> &'static str {
    match marker.reason {
        MarkerReason::AppExited => ssr_i18n::guard::marker_app_exited(language),
        MarkerReason::StallRestarted => ssr_i18n::guard::marker_stall_restarted(language),
        MarkerReason::RestartFailed => ssr_i18n::guard::marker_restart_failed(language),
        MarkerReason::Stuck => ssr_i18n::guard::marker_stuck(language),
    }
}

pub fn read(path: &Path) -> io::Result<Marker> {
    let text = fs::read_to_string(path)?;
    let marker: Marker = serde_json::from_str(&text).map_err(io::Error::other)?;
    if marker.version != Marker::VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported guard marker version {}", marker.version),
        ));
    }
    Ok(marker)
}

pub fn write(path: &Path, marker: &Marker) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("json.tmp");
    let text = serde_json::to_vec(marker).map_err(io::Error::other)?;
    fs::write(&temporary, text)?;
    if let Err(error) = fs::rename(&temporary, path) {
        if !path.exists() {
            return Err(error);
        }
        fs::remove_file(path)?;
        fs::rename(temporary, path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use ssr_types::app::Language;

    use super::*;

    #[test]
    fn marker_sentences_cover_every_reason_without_restoration_notes() {
        for (reason, expected) in [
            (
                MarkerReason::AppExited,
                "지난 실행이 비정상 종료되어 기본 장치를 되돌렸습니다.",
            ),
            (
                MarkerReason::StallRestarted,
                "엔진이 멈춰 앱을 다시 시작했습니다.",
            ),
            (
                MarkerReason::RestartFailed,
                "다시 시작한 뒤에도 엔진이 멈춰 기본 장치를 되돌렸습니다.",
            ),
            (
                MarkerReason::Stuck,
                "지난 실행이 기본 장치를 넘겨받은 채 멈춰 기본 장치를 되돌렸습니다.",
            ),
        ] {
            let marker = Marker::new(reason, 0, Vec::new());
            assert_eq!(sentence(&marker, Language::Ko), expected);
            let with_notes = Marker::new(reason, 42, vec!["복원 메모".to_owned()]);
            assert_eq!(sentence(&with_notes, Language::Ko), expected);
        }
    }
}
