#![forbid(unsafe_code)]

use ssr_guard::marker::{self, Marker, MarkerReason};
use ssr_types::app::Language;

#[test]
fn every_marker_reason_has_both_languages_without_changing_the_record() {
    for (reason, korean, english) in [
        (
            MarkerReason::AppExited,
            "지난 실행이 비정상 종료되어 기본 장치를 되돌렸습니다.",
            "The app exited unexpectedly. The default audio devices have been restored.",
        ),
        (
            MarkerReason::StallRestarted,
            "엔진이 멈춰 앱을 다시 시작했습니다.",
            "The engine stopped responding, so the app was restarted.",
        ),
        (
            MarkerReason::RestartFailed,
            "다시 시작한 뒤에도 엔진이 멈춰 기본 장치를 되돌렸습니다.",
            "The engine stopped responding after restarting. The default audio devices have been restored.",
        ),
        (
            MarkerReason::Stuck,
            "지난 실행이 기본 장치를 넘겨받은 채 멈춰 기본 장치를 되돌렸습니다.",
            "The app stopped responding while holding the default audio devices. The defaults have been restored.",
        ),
    ] {
        let marker = Marker::new(reason, 123, vec!["diagnostic detail".to_owned()]);
        let before = serde_json::to_string(&marker).expect("marker encodes");
        assert_eq!(marker::sentence(&marker, Language::Ko), korean);
        assert_eq!(marker::sentence(&marker, Language::En), english);
        assert_eq!(
            serde_json::to_string(&marker).expect("marker encodes"),
            before
        );
    }
}
