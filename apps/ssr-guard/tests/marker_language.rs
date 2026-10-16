#![forbid(unsafe_code)]

use ssr_guard::marker::{self, Marker, MarkerReason};
use ssr_types::app::Language;

#[test]
fn every_marker_reason_has_both_languages_without_changing_the_record() {
    for (reason, korean, english) in [
        (
            MarkerReason::AppExited,
            "지난번에 앱이 예기치 않게 종료되어 기본 오디오 장치를 되돌렸습니다.",
            "The app exited unexpectedly. The default audio devices have been restored.",
        ),
        (
            MarkerReason::StallRestarted,
            "소리 처리가 멈춰 앱을 다시 시작했습니다.",
            "Audio processing stopped responding, so the app was restarted.",
        ),
        (
            MarkerReason::RestartFailed,
            "다시 시작한 뒤에도 소리 처리가 멈춰 기본 오디오 장치를 되돌렸습니다.",
            "Audio processing stopped responding after restarting. The default audio devices have been restored.",
        ),
        (
            MarkerReason::Stuck,
            "앱을 기본 오디오 장치로 사용하던 중 응답이 없어 이전 장치로 되돌렸습니다.",
            "The app stopped responding while it was the default audio device. The previous devices have been restored.",
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
