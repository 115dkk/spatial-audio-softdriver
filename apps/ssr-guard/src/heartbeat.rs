use ssr_types::app::Heartbeat;

/// Parses one heartbeat line. Malformed and unrelated stdout is ignored.
pub fn parse(line: &str) -> Option<Heartbeat> {
    serde_json::from_str(line).ok()
}
