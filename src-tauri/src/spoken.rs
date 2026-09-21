//! Which engine actually speaks, and what happens when one cannot.
//!
//! The app has two ways to make sound and they share almost nothing:
//!
//! * **Apple system voices** — `/usr/bin/say` on a pipe, text straight in.
//! * **Kokoro** — phonemes from the local front end, an ONNX graph, then a WAV.
//!
//! Kokoro does not accept text. It accepts phonemes, and the tables that produce them were
//! fetched alongside the graph ([`crate::g2p`]). That is why this module exists at all: the
//! piece that used to be missing was not a player, it was the text → phoneme step.
//!
//! Two deliberate choices worth naming:
//!
//! * **Playback is `afplay` on a rendered WAV, not a streaming audio graph.** Kokoro's RTF
//!   on this machine is about 0.2, so a three-second selection is synthesised in well under
//!   a second and there is nothing to hide behind a stream yet. Adding an audio crate would
//!   add a dependency tree and a callback-lifetime problem to buy latency the synthesis does
//!   not need. When utterances get long enough for this to show, the fix is chunked playback
//!   inside the synthesis loop, not a different player.
//! * **No fallback.** An engine that cannot speak reports why. A user who picks Kokoro must
//!   never hear Samantha and conclude that is what Kokoro sounds like.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;

use crate::config::{Engine, Settings};
use crate::engine_paths;
use crate::g2p::G2p;
use crate::kokoro::{self, Kokoro};
use crate::speech::Speaker;

/// macOS ships this. Playing a file needs no crate, and it is already how the app treats
/// `/usr/bin/say` — a system binary on a pipe rather than a library in the process.
const AFPLAY: &str = "/usr/bin/afplay";

/// What was said, so the caller can report something truthful about it.
#[derive(Debug, Clone)]
pub struct Report {
    pub chars: usize,
    /// Phonemes handed to the engine (Kokoro only; `say` takes text).
    pub phonemes: usize,
    pub seconds: f32,
    /// Phoneme symbols the vocabulary could not encode. Kokoro *silently* drops these, so
    /// they are surfaced rather than swallowed.
    pub dropped: Vec<char>,
}

impl Report {
    pub fn summary(&self) -> String {
        if self.dropped.is_empty() {
            format!("{:.1}s", self.seconds)
        } else {
            format!(
                "{:.1}s, {} symbol(s) unspoken: {}",
                self.seconds,
                self.dropped.len(),
                self.dropped.iter().collect::<String>()
            )
        }
    }
}

/// A loaded Kokoro session plus what it was loaded for, so a voice change reloads and a
/// repeat does not. Loading the 325 MB graph is worth caching; getting the voice wrong is
/// worse than reloading.
struct Loaded {
    engine: Kokoro,
    voice: String,
}

pub struct Spoken {
    /// The Apple path. Kept as its own type: it is the bootstrap engine and the default.
    speech: Speaker,
    kokoro: Mutex<Option<Loaded>>,
    /// The front end, loaded once. Re-reading 6 MB of JSON per utterance would be absurd.
    g2p: Mutex<Option<G2p>>,
    /// The player, so a second utterance cancels the first instead of talking over it.
    player: Mutex<Option<Child>>,
    /// Whether the loaded graph survives a stop. Reloading 325 MB costs seconds — 27 of them
    /// from a cold disk — so a user who is reading selections back to back should not pay it
    /// for pressing stop. This is the `keep_warm` setting, remembered from the last utterance
    /// because `stop` is not handed the settings.
    keep_warm: Mutex<bool>,
}

impl Default for Spoken {
    fn default() -> Self {
        Self::new()
    }
}

impl Spoken {
    pub fn new() -> Self {
        Self {
            speech: Speaker::new(),
            kokoro: Mutex::new(None),
            g2p: Mutex::new(None),
            player: Mutex::new(None),
            keep_warm: Mutex::new(true),
        }
    }

    /// Speak `text` with whichever engine the settings name.
    pub fn speak(&self, settings: &Settings, text: &str) -> Result<Report, String> {
        match settings.engine {
            Engine::Apple => self.speak_apple(text, settings.voice.as_deref(), settings.rate),
            Engine::Kokoro => {
                let wav = self.wav_path()?;
                let report = self.render(settings, text, &wav)?;
                self.play(&wav)?;
                Ok(report)
            }
            other => Err(format!(
                "{} cannot speak yet: it is installed by its own Python runtime and that \
                 sidecar is not implemented",
                engine_label(other)
            )),
        }
    }

    /// Audition one voice. `voice` is an identifier belonging to the *active* engine: an
    /// Apple voice name when Apple is active, a Kokoro voice id when Kokoro is.
    pub fn preview(
        &self,
        settings: &Settings,
        voice: Option<&str>,
        rate: u32,
        text: &str,
    ) -> Result<Report, String> {
        match settings.engine {
            Engine::Apple => self.speak_apple(text, voice, rate),
            Engine::Kokoro => {
                let mut audition = settings.clone();
                if let Some(voice) = voice {
                    audition.kokoro.voice = voice.to_string();
                }
                self.speak(&audition, text)
            }
            other => Err(format!("{} cannot speak yet", engine_label(other))),
        }
    }

    fn speak_apple(&self, text: &str, voice: Option<&str>, rate: u32) -> Result<Report, String> {
        self.speech.speak(text, voice, rate)?;
        Ok(Report {
            chars: text.chars().count(),
            phonemes: 0,
            // `say` streams: it starts speaking well before the sentence is over, so any
            // duration here would be a guess. The phoneme count is the honest analogue.
            seconds: 0.0,
            dropped: Vec::new(),
        })
    }

    /// Text → phonemes → audio → WAV at `path`, without playing it. This is what the
    /// "speak to file" action will call, and what the integration test drives.
    pub fn render(&self, settings: &Settings, text: &str, path: &Path) -> Result<Report, String> {
        if settings.engine != Engine::Kokoro {
            return Err(format!(
                "rendering a file needs a local engine; {} writes audio itself",
                engine_label(settings.engine)
            ));
        }
        let dir = engine_paths::kokoro_dir().ok_or("cannot locate the app support directory")?;
        if !engine_paths::kokoro_installed() {
            return Err(
                "Kokoro's files are not installed. Use Download in its engine card first."
                    .to_string(),
            );
        }

        let (samples, phoneme_count, dropped) = self.synthesize(&dir, settings, text)?;
        let seconds = samples.len() as f32 / kokoro::SAMPLE_RATE as f32;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {parent:?}: {e}"))?;
        }
        kokoro::write_wav(path, &samples, kokoro::SAMPLE_RATE)?;

        Ok(Report {
            chars: text.chars().count(),
            phonemes: phoneme_count,
            seconds,
            dropped,
        })
    }

    /// The synthesis core, separated from playback so it can be tested without a speaker.
    fn synthesize(
        &self,
        dir: &Path,
        settings: &Settings,
        text: &str,
    ) -> Result<(Vec<f32>, usize, Vec<char>), String> {
        let voice = settings.kokoro.voice.clone();
        // An espeak-backed voice has no front end in this repo to fall back on: its phonemes
        // exist only via espeak-ng, which is GPL-3.0 and therefore never bundled. If the
        // install has gone missing, say so plainly rather than synthesising from an English
        // front end and producing confident nonsense.
        let phonemes = match crate::engines::espeak_language_for(&voice) {
            Some(language) => {
                let espeak = crate::espeak::EspeakNg::detect().ok_or_else(|| {
                    format!("the voice {voice} needs espeak-ng, which is not installed")
                })?;
                espeak.phonemize(text, language)?
            }
            None => self.phonemize(dir, text)?,
        };
        if phonemes.trim().is_empty() {
            return Err("nothing to say: that text produced no phonemes".to_string());
        }
        let count = phonemes.chars().count();
        *self.keep_warm.lock().unwrap() = settings.kokoro.keep_warm;

        let mut guard = self.kokoro.lock().unwrap();
        let stale = guard.as_ref().is_none_or(|loaded| loaded.voice != voice);
        if stale {
            let voice_file = dir.join("voices").join(format!("{voice}.bin"));
            if !voice_file.is_file() {
                return Err(format!(
                    "the voice '{voice}' is not on disk; reinstall Kokoro to fetch it"
                ));
            }
            let engine = Kokoro::load(
                &dir.join(engine_paths::KOKORO_MODEL_FILE),
                &dir.join(engine_paths::KOKORO_TOKENIZER_FILE),
                &voice_file,
                settings.kokoro.speed,
            )?;
            *guard = Some(Loaded { engine, voice });
        }
        let loaded = guard.as_mut().expect("just loaded");
        let (samples, dropped) = loaded.engine.synthesize(&phonemes)?;
        Ok((samples, count, dropped))
    }

    fn phonemize(&self, dir: &Path, text: &str) -> Result<String, String> {
        let mut guard = self.g2p.lock().unwrap();
        if guard.is_none() {
            *guard = Some(G2p::from_dir(&dir.join("lexicon"))?);
        }
        Ok(guard.as_ref().expect("just loaded").phonemize(text))
    }

    /// Where the spoken WAV lives. One path per process: a new utterance stops the player
    /// and overwrites it, so there is never an orphaned file and never a file being read
    /// while it is written.
    fn wav_path(&self) -> Result<PathBuf, String> {
        let dir =
            engine_paths::app_support_dir().ok_or("cannot locate the app support directory")?;
        Ok(dir.join("cache").join("spoken.wav"))
    }

    fn play(&self, wav: &Path) -> Result<(), String> {
        self.stop();
        let child = Command::new(AFPLAY)
            .arg(wav)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("spawn {AFPLAY}: {e}"))?;
        *self.player.lock().unwrap() = Some(child);
        Ok(())
    }

    /// Silence whatever is playing. Safe to call when idle.
    pub fn stop(&self) {
        self.speech.stop();
        let mut guard = self.player.lock().unwrap();
        if let Some(mut child) = guard.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        drop(guard);
        // Release the graph unless the user asked to keep it. Stopping is not the same as
        // unloading: with `keep_warm` on, the next selection speaks immediately instead of
        // waiting for 325 MB to be read off disk again.
        if !*self.keep_warm.lock().unwrap() {
            *self.kokoro.lock().unwrap() = None;
        }
    }

    pub fn is_speaking(&self) -> bool {
        if self.speech.is_speaking() {
            return true;
        }
        let mut guard = self.player.lock().unwrap();
        match guard.as_mut() {
            Some(child) => match child.try_wait() {
                Ok(Some(_)) => {
                    *guard = None;
                    false
                }
                Ok(None) => true,
                Err(_) => false,
            },
            None => false,
        }
    }
}

fn engine_label(engine: Engine) -> &'static str {
    match engine {
        Engine::Apple => "the Apple system voices",
        Engine::Kokoro => "Kokoro",
        Engine::Qwen => "Qwen3-TTS",
        Engine::Chatterbox => "Chatterbox",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Settings;

    #[test]
    fn an_unimplemented_engine_refuses_instead_of_falling_back() {
        let settings = Settings {
            engine: Engine::Qwen,
            ..Default::default()
        };
        let spoken = Spoken::new();
        let error = spoken
            .speak(&settings, "hello")
            .expect_err("Qwen has no sidecar yet, so this must not return success");
        assert!(
            error.contains("sidecar"),
            "the refusal should name the missing piece, got: {error}"
        );
    }

    #[test]
    fn rendering_a_file_refuses_for_a_non_local_engine() {
        let settings = Settings {
            engine: Engine::Apple,
            ..Default::default()
        };
        let spoken = Spoken::new();
        let error = spoken
            .render(
                &settings,
                "hello",
                Path::new("/tmp/kiegen-never-written.wav"),
            )
            .expect_err("`say` writes no file, so this must not claim to have written one");
        assert!(error.contains("local engine"), "got: {error}");
    }

    #[test]
    fn an_empty_selection_is_refused_rather_than_silently_ignored() {
        // `say` on empty input exits 0 without a sound; the caller needs to know.
        let settings = Settings::default();
        let spoken = Spoken::new();
        assert!(spoken.speak(&settings, "   ").is_err());
    }

    #[test]
    fn a_report_with_dropped_symbols_says_so() {
        let report = Report {
            chars: 10,
            phonemes: 12,
            seconds: 1.25,
            dropped: vec!['❓'],
        };
        assert!(report.summary().contains("unspoken"));
        let clean = Report {
            chars: 10,
            phonemes: 12,
            seconds: 1.25,
            dropped: Vec::new(),
        };
        assert_eq!(clean.summary(), "1.2s");
    }
}
