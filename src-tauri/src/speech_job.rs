use crate::{Phase, StatusEvent};
use std::time::Instant;

pub struct SpeechJob {
    pub id: u64,
    pub streaming: bool,
    pub status: StatusEvent,
    pub changed: Instant,
}

impl Default for SpeechJob {
    fn default() -> Self {
        Self {
            id: 0,
            streaming: false,
            status: StatusEvent {
                phase: Phase::Idle,
                message: None,
                chars: None,
            },
            changed: Instant::now(),
        }
    }
}

impl SpeechJob {
    pub fn begin(&mut self, phase: Phase) -> u64 {
        self.id += 1;
        self.streaming = false;
        self.set(phase, None, None);
        self.id
    }
    pub fn is_current(&self, id: u64) -> bool {
        self.id == id
    }
    pub fn cancel(&mut self) {
        self.id += 1;
        self.streaming = false;
        self.set(Phase::Idle, None, None);
    }
    /// A gap in streamed playback means more audio is being prepared, not completion.
    pub fn observe_playback(&mut self, playing: bool) -> bool {
        if !playing && matches!(self.status.phase, Phase::Speaking) {
            self.set(
                if self.streaming {
                    Phase::Preparing
                } else {
                    Phase::Idle
                },
                None,
                None,
            );
            return true;
        }
        false
    }

    pub fn set(&mut self, phase: Phase, message: Option<String>, chars: Option<usize>) {
        self.status = StatusEvent {
            phase,
            message,
            chars,
        };
        self.changed = Instant::now();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stop_during_processing_invalidates_pending_playback() {
        let mut job = SpeechJob::default();
        let pending = job.begin(Phase::Preparing);
        job.cancel();
        assert!(!job.is_current(pending));
        assert!(matches!(job.status.phase, Phase::Idle));
    }
    #[test]
    fn a_gap_between_chunks_keeps_the_job_preparing_until_stopped() {
        let mut job = SpeechJob::default();
        job.begin(Phase::Preparing);
        job.streaming = true;
        job.set(Phase::Speaking, None, None);
        assert!(job.observe_playback(false));
        assert!(matches!(job.status.phase, Phase::Preparing));
        job.cancel();
        assert!(!job.streaming);
        assert!(matches!(job.status.phase, Phase::Idle));
    }

    #[test]
    fn completed_non_streaming_playback_becomes_idle() {
        let mut job = SpeechJob::default();
        job.begin(Phase::Speaking);
        assert!(job.observe_playback(false));
        assert!(matches!(job.status.phase, Phase::Idle));
    }

    #[test]
    fn a_new_request_invalidates_the_previous_request() {
        let mut job = SpeechJob::default();
        let old = job.begin(Phase::Capturing);
        let new = job.begin(Phase::Preparing);
        assert!(!job.is_current(old));
        assert!(job.is_current(new));
    }
}
