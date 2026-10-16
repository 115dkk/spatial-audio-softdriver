//! Deterministic glue between decoded control messages and process supervision.
use plughost_proto::{Action, Frame, HostWatch, Message, Received, RequestKind, StateHistory};

#[derive(Debug)]
pub struct ControlWatch {
    watch: HostWatch,
    pub states: StateHistory,
    latency: u32,
}
impl ControlWatch {
    pub fn new(now_ms: u64) -> Self {
        Self {
            watch: HostWatch::new(now_ms),
            states: StateHistory::new(),
            latency: 0,
        }
    }
    pub fn latency(&self) -> u32 {
        self.latency
    }
    pub fn state(&self) -> plughost_proto::WatchState {
        self.watch.state()
    }
    pub fn received(&mut self, now_ms: u64, frame: &Frame) -> Action {
        self.watch.received(
            now_ms,
            Received::Response {
                request: frame.request,
            },
        );
        match &frame.message {
            Message::Hello { .. } => self.watch.received(now_ms, Received::Hello),
            Message::Loaded { latency_frames, .. } => {
                self.latency = *latency_frames;
                self.watch.received(now_ms, Received::Loaded)
            }
            Message::LatencyChanged { frames } => {
                self.latency = *frames;
                Action::None
            }
            Message::ParamsChanged => self.watch.received(now_ms, Received::ParamsChanged),
            Message::EditorClosed => self.watch.received(now_ms, Received::EditorClosed),
            Message::State { blob } => {
                self.states.save(blob.clone());
                Action::None
            }
            _ => Action::None,
        }
    }
    /// Called only after Configure, optional restore and Activate all succeed.
    pub fn activated(&mut self, now_ms: u64) -> Action {
        self.watch.received(now_ms, Received::Configured)
    }
    pub fn request(&mut self, now_ms: u64, id: u32, kind: RequestKind) {
        self.watch.request_sent(now_ms, id, kind);
    }
    pub fn shutdown(&mut self, now_ms: u64) -> Action {
        self.watch.shutdown(now_ms)
    }
    pub fn poll(&mut self, now_ms: u64, heartbeat: u64, exit: Option<i32>) -> Vec<Action> {
        self.watch.heartbeat(now_ms, heartbeat);
        let mut actions = Vec::new();
        if let Some(code) = exit {
            let action = self.watch.process_exited(now_ms, code);
            if action != Action::None {
                actions.push(action);
            }
        }
        let action = self.watch.tick(now_ms);
        if action != Action::None {
            actions.push(action);
        }
        actions
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plughost_proto::{KillReason, WatchState};
    fn running(watch: &mut ControlWatch, time: u64) {
        watch.received(
            time,
            &Frame {
                flags: 0,
                request: 0,
                message: Message::hello(),
            },
        );
        watch.received(
            time,
            &Frame {
                flags: 0,
                request: 1,
                message: Message::Loaded {
                    name: "test".into(),
                    vendor: "test".into(),
                    latency_frames: 64,
                    inputs: vec![],
                    outputs: vec![],
                },
            },
        );
        watch.activated(time);
    }
    #[test]
    fn watch_detects_hang_and_three_crashes() {
        let mut watch = ControlWatch::new(0);
        running(&mut watch, 0);
        assert_eq!(watch.latency(), 64);
        assert_eq!(
            watch.poll(500, 0, None),
            [Action::Kill {
                reason: KillReason::HeartbeatTimeout
            }]
        );
        assert_eq!(watch.poll(1000, 0, Some(1)), [Action::Spawn]);
        running(&mut watch, 1000);
        assert_eq!(watch.poll(1001, 1, Some(1)), [Action::EnterFallback]);
        assert_eq!(watch.poll(3001, 1, None), [Action::Spawn]);
        running(&mut watch, 3001);
        assert!(matches!(
            watch.poll(3002, 2, Some(1)).as_slice(),
            [Action::Disable { .. }]
        ));
        assert_eq!(watch.state(), WatchState::Disabled);
    }
    #[test]
    fn requested_shutdown_does_not_respawn() {
        let mut watch = ControlWatch::new(0);
        running(&mut watch, 0);
        watch.shutdown(1);
        assert!(watch.poll(2, 1, Some(0)).is_empty());
        assert!(watch.poll(10000, 1, None).is_empty());
        assert_eq!(watch.state(), WatchState::Stopped);
    }
}
