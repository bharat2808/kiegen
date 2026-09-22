//! Bounded, ordered producer/consumer playback for models that return audio buffers.
use std::sync::mpsc::{sync_channel, RecvTimeoutError};
use std::time::Duration;

/// Split at natural boundaries, with a short first phrase for early playback.
pub fn chunks(text: &str) -> Vec<String> {
    if text.trim().is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let limit = if out.is_empty() { 100 } else { 160 };
        let window: Vec<(usize, char)> = rest.char_indices().take(limit).collect();
        let hard_end = window.last().map(|(i, c)| i + c.len_utf8()).unwrap();
        let sentence = window.iter().find_map(|(i, c)| {
            let end = i + c.len_utf8();
            let next = rest[end..].chars().next();
            let boundary = "。！？\n".contains(*c)
                || (".!?".contains(*c) && next.is_none_or(char::is_whitespace));
            boundary.then_some(end)
        });
        let end = if let Some(end) = sentence {
            end
        } else if hard_end == rest.len() {
            hard_end
        } else {
            window
                .iter()
                .rev()
                .find(|(i, c)| *i > 0 && (c.is_whitespace() || ",;:，；：".contains(*c)))
                .map(|(i, c)| i + c.len_utf8())
                .unwrap_or(hard_end)
        };
        out.push(rest[..end].to_string());
        rest = &rest[end..];
        // Keep whitespace with the preceding chunk when it fits the same budget.
        let room = limit - out.last().unwrap().chars().count();
        let spaces = rest
            .char_indices()
            .take(room)
            .take_while(|(_, c)| c.is_whitespace())
            .last()
            .map(|(i, c)| i + c.len_utf8())
            .unwrap_or(0);
        out.last_mut().unwrap().push_str(&rest[..spaces]);
        rest = &rest[spaces..];
    }
    out
}

pub fn run<T: Send, R, P, C>(
    chunks: Vec<String>,
    mut render: R,
    mut play: P,
    cancelled: C,
) -> Result<(), String>
where
    R: FnMut(&str) -> Result<T, String> + Send,
    P: FnMut(T) -> Result<(), String>,
    C: Fn() -> bool + Sync,
{
    std::thread::scope(|scope| {
        // One ready chunk may wait while the next is synthesized and the current
        // chunk plays. Backpressure bounds memory independently of selection length.
        let (tx, rx) = sync_channel(1);
        let cancel = &cancelled;
        let producer = scope.spawn(move || {
            for chunk in chunks {
                if cancel() {
                    break;
                }
                let audio = render(&chunk);
                let failed = audio.is_err();
                if cancel() || tx.send(audio).is_err() || failed {
                    break;
                }
            }
        });
        let result = loop {
            if cancelled() {
                break Ok(());
            }
            match rx.recv_timeout(Duration::from_millis(20)) {
                Ok(Ok(audio)) => {
                    if cancelled() {
                        break Ok(());
                    }
                    if let Err(error) = play(audio) {
                        break Err(error);
                    }
                }
                Ok(Err(error)) => break Err(error),
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => break Ok(()),
            }
        };
        // Unblock a producer waiting on a full queue after Stop or a player error.
        drop(rx);
        producer
            .join()
            .map_err(|_| "speech synthesis worker failed".to_string())?;
        result
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    };

    #[test]
    fn first_audio_plays_before_later_audio_finishes_rendering() {
        let (played, wait_for_play) = mpsc::channel();
        let mut heard = Vec::new();
        run(
            vec!["first".into(), "second".into(), "third".into()],
            move |text| {
                if text == "second" {
                    wait_for_play
                        .recv_timeout(Duration::from_secs(1))
                        .map_err(|_| "waited for all synthesis before playback".to_string())?;
                }
                Ok(text.to_string())
            },
            |audio| {
                if audio == "first" {
                    played.send(()).unwrap();
                }
                heard.push(audio);
                Ok(())
            },
            || false,
        )
        .unwrap();
        assert_eq!(heard, ["first", "second", "third"]);
    }

    #[test]
    fn backpressure_bounds_synthesis_ahead_of_a_slow_player() {
        use std::sync::{atomic::AtomicUsize, Arc};
        let rendered = Arc::new(AtomicUsize::new(0));
        let count = rendered.clone();
        let (third, waiting_for_third) = mpsc::channel();
        let error = run(
            (0..100).map(|i| i.to_string()).collect(),
            move |text| {
                let n = count.fetch_add(1, Ordering::SeqCst) + 1;
                if n == 3 {
                    third.send(()).unwrap();
                }
                Ok(text.to_string())
            },
            |_| {
                waiting_for_third
                    .recv_timeout(Duration::from_secs(1))
                    .unwrap();
                // Playing one, one ready, and one waiting to enter the bounded queue.
                assert_eq!(rendered.load(Ordering::SeqCst), 3);
                Err("player stopped".to_string())
            },
            || false,
        )
        .unwrap_err();
        assert_eq!(error, "player stopped");
        assert_eq!(rendered.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn cancellation_discards_queued_audio() {
        let stopped = AtomicBool::new(false);
        let mut heard = Vec::new();
        run(
            vec!["first".into(), "second".into(), "third".into()],
            |text| Ok(text.to_string()),
            |audio| {
                heard.push(audio);
                stopped.store(true, Ordering::SeqCst);
                Ok(())
            },
            || stopped.load(Ordering::SeqCst),
        )
        .unwrap();
        assert_eq!(heard, ["first"]);
    }

    #[test]
    fn a_later_synthesis_failure_preserves_already_played_audio_and_reports_failure() {
        let mut heard = Vec::new();
        let error = run(
            vec!["first".into(), "broken".into()],
            |text| {
                if text == "broken" {
                    Err("decoder failed".to_string())
                } else {
                    Ok(text.to_string())
                }
            },
            |audio| {
                heard.push(audio);
                Ok(())
            },
            || false,
        )
        .unwrap_err();
        assert_eq!(heard, ["first"]);
        assert_eq!(error, "decoder failed");
    }

    #[test]
    fn text_is_preserved_and_the_first_sentence_can_start_early() {
        let text = "Hello there. This is the next sentence. And another one follows.";
        let parts = chunks(text);
        assert!(parts.len() > 1);
        assert_eq!(parts.concat(), text);
        assert_eq!(parts[0].trim(), "Hello there.");
    }

    #[test]
    fn long_unpunctuated_and_unicode_text_is_bounded_without_lost_characters() {
        for text in ["word ".repeat(100), "世界你好".repeat(100)] {
            let parts = chunks(&text);
            assert!(parts.iter().all(|part| part.chars().count() <= 160));
            assert!(parts[0].chars().count() <= 100);
            assert_eq!(parts.concat(), text);
        }
    }

    #[test]
    fn decimals_are_not_sentence_boundaries() {
        assert_eq!(
            chunks("It costs 3.50 dollars. Next sentence.")[0].trim(),
            "It costs 3.50 dollars."
        );
    }
}
