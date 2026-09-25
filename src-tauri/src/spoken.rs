//! Which engine actually speaks, and what happens when one cannot.
//!
//! The app has three ways to make sound and they share almost nothing:
//!
//! * **Apple system voices** — `/usr/bin/say` on a pipe, text straight in.
//! * **Kokoro** — phonemes from the local front end, an ONNX graph, then PCM samples.
//! * **Chatterbox** — text through its own tokenizer, four ONNX graphs and a kv-cache loop,
//!   with the speaker cloned from a reference clip.
//!
//! Kokoro does not accept text. It accepts phonemes, and the tables that produce them were
//! fetched alongside the graph ([`crate::g2p`]). Chatterbox does accept text — its whole front
//! end is a Llama BPE tokenizer — but it does not accept a *voice*: the speaker is a clip, so
//! what this module hands it is a path.
//!
//! Both local engines use one native PCM output queue per reading. An unavailable engine
//! reports an error instead of changing the chosen voice.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

use crate::chatterbox::{self, Chatterbox};
use crate::config::{Engine, Settings};
use crate::engine_paths;
use crate::g2p::G2p;
use crate::kokoro::{self, Kokoro};
use crate::speech::Speaker;
use crate::voices;

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

/// A loaded Chatterbox session plus the three things that would make it the wrong session.
///
/// Loading this engine is 1.5 GB and several seconds, so it is kept — but *every* input that
/// changes the output is part of the key, including the emotion knob, which is an input to
/// `embed_tokens` rather than a sampling parameter.
struct ChatterboxLoaded {
    engine: Chatterbox,
    voice: String,
    language: String,
    exaggeration: String,
}

pub struct Spoken {
    /// The Apple path. Kept as its own type: it is the bootstrap engine and the default.
    speech: Speaker,
    kokoro: Mutex<Option<Loaded>>,
    chatterbox: Mutex<Option<ChatterboxLoaded>>,
    /// The front end, loaded once. Re-reading 6 MB of JSON per utterance would be absurd.
    g2p: Mutex<Option<G2p>>,
    /// The player, so a second utterance cancels the first instead of talking over it.
    player: Mutex<Option<Child>>,
    pcm: Mutex<Option<Arc<crate::pcm::Player>>>,
    /// Whether the loaded graph survives a stop. Reloading 325 MB costs seconds — 27 of them
    /// from a cold disk — so a user who is reading selections back to back should not pay it
    /// for pressing stop. This is the `keep_warm` setting, remembered from the last utterance
    /// because `stop` is not handed the settings.
    keep_warm: Mutex<bool>,
    /// Whether the Chatterbox sessions survive a stop. Separate from Kokoro's: this engine is
    /// 1.5 GB resident and both has to be the user's own decision.
    chatterbox_keep_warm: Mutex<bool>,
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
            chatterbox: Mutex::new(None),
            g2p: Mutex::new(None),
            player: Mutex::new(None),
            pcm: Mutex::new(None),
            keep_warm: Mutex::new(true),
            chatterbox_keep_warm: Mutex::new(false),
        }
    }

    /// Speak `text` with whichever engine the settings name.
    pub fn speak(&self, settings: &Settings, text: &str) -> Result<Report, String> {
        match settings.engine {
            Engine::Apple => self.speak_apple(text, settings.voice.as_deref(), settings.rate),
            Engine::Kokoro | Engine::Chatterbox => {
                let wav = self.wav_path()?;
                let report = self.render(settings, text, &wav)?;
                self.play(&wav)?;
                Ok(report)
            }
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
            // Chatterbox's rows are languages, so auditioning one is auditioning a language —
            // the reference clip stays whatever the user already chose.
            Engine::Chatterbox => {
                let mut audition = settings.clone();
                if let Some(voice) = voice {
                    audition.chatterbox.voice = voice.to_string();
                }
                self.speak(&audition, text)
            }
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

    /// Text → audio → WAV at `path`, without playing it. This is what the "speak to file"
    /// action will call, and what the integration tests drive.
    ///
    /// One path for both local engines: they differ in everything except the shape of this,
    /// which is "turn text into 24 kHz mono and write it down".
    pub fn render(&self, settings: &Settings, text: &str, path: &Path) -> Result<Report, String> {
        let (samples, report, rate) = self.synthesize_audio(settings, text)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {parent:?}: {e}"))?;
        }
        kokoro::write_wav(path, &samples, rate)?;
        Ok(report)
    }

    fn synthesize_audio(
        &self,
        settings: &Settings,
        text: &str,
    ) -> Result<(Vec<f32>, Report, u32), String> {
        let (samples, phoneme_count, dropped, rate) = match settings.engine {
            Engine::Kokoro => {
                let dir =
                    engine_paths::kokoro_dir().ok_or("cannot locate the app support directory")?;
                if !engine_paths::kokoro_installed() {
                    return Err(
                        "Kokoro's files are not installed. Use Download in its engine card first."
                            .to_string(),
                    );
                }
                let (samples, count, dropped) = self.synthesize_kokoro(&dir, settings, text)?;
                (samples, count, dropped, kokoro::SAMPLE_RATE)
            }
            Engine::Chatterbox => {
                let dir = engine_paths::chatterbox_dir()
                    .ok_or("cannot locate the app support directory")?;
                // The language gate is checked before the install check on purpose: it is a
                // statement about what this build can do, and it is just as true before the
                // weights are on disk. A user who picked Japanese should be told that, not
                // told to download 1.5 GB that would not help.
                chatterbox_language_guard(&settings.chatterbox.voice)?;
                if !engine_paths::chatterbox_installed() {
                    return Err(
                        "Chatterbox's weights are not installed. Use Download in its engine card \
                         first."
                            .to_string(),
                    );
                }
                let samples = self.synthesize_chatterbox(&dir, settings, text)?;
                (samples, 0, Vec::new(), chatterbox::SAMPLE_RATE)
            }
            other => {
                return Err(format!(
                    "rendering a file needs a local engine; {} writes audio itself",
                    engine_label(other)
                ))
            }
        };

        let report = Report {
            chars: text.chars().count(),
            phonemes: phoneme_count,
            seconds: samples.len() as f32 / rate as f32,
            dropped,
        };
        Ok((samples, report, rate))
    }

    /// Feed one persistent native output queue while the producer synthesizes ahead.
    /// The caller starts the player under the speech-job lock, so Stop cannot race start.
    pub(crate) fn stream_pcm<C, P>(
        &self,
        settings: &Settings,
        text: &str,
        cancelled: C,
        mut start: P,
    ) -> Result<(Report, f64, f64), String>
    where
        C: Fn() -> bool + Sync,
        P: FnMut(&Arc<crate::pcm::Player>) -> Result<(), String>,
    {
        use std::time::{Duration, Instant};
        let began = Instant::now();
        let chunks = match settings.engine {
            Engine::Kokoro => crate::streaming::kokoro_chunks(text),
            Engine::Chatterbox => crate::streaming::chatterbox_chunks(text),
            Engine::Apple => return Err("PCM playback needs a local speech engine".into()),
        };
        if chunks.is_empty() {
            return Err("nothing to say: the selection is empty".into());
        }
        let count = chunks.len();
        let first_chars = chunks[0].chars().count().max(1);
        let next_chars = chunks.get(1).map(|s| s.chars().count()).unwrap_or(0);
        let player = Arc::new(crate::pcm::Player::new()?);
        let mut total = Report {
            chars: 0,
            phonemes: 0,
            seconds: 0.,
            dropped: vec![],
        };
        let mut received = 0;
        let mut started = None;
        let mut startup_target = 0.5;
        let mut peak_buffer = 0.0f64;
        let result = crate::streaming::run(
            chunks,
            |chunk| {
                let now = Instant::now();
                let audio = self.synthesize_audio(settings, chunk)?;
                Ok((audio, now.elapsed().as_secs_f64()))
            },
            |((samples, report, rate), generation_seconds)| {
                if rate != crate::pcm::SAMPLE_RATE as u32 {
                    return Err("The speech engine returned an unsupported sample rate".into());
                }
                if received == 0 {
                    // Estimate how long the next chunk needs from observed generation speed.
                    // Short opening sentences may wait for another chunk; normal sentences
                    // already contain more than this much audio and start immediately.
                    startup_target = (generation_seconds * next_chars as f64 / first_chars as f64
                        * 1.25)
                        .clamp(0.5, 3.0);
                }
                // Bound ahead-of-playback storage to eight seconds plus one model chunk.
                while started.is_some() && player.buffered_seconds() >= 8.0 && !cancelled() {
                    player.is_playing()?;
                    std::thread::sleep(Duration::from_millis(10));
                }
                if cancelled() {
                    return Ok(());
                }
                received += 1;
                player.push(samples, received == count);
                peak_buffer = peak_buffer.max(player.buffered_seconds());
                if started.is_none()
                    && (player.buffered_seconds() >= startup_target || received == count)
                {
                    start(&player)?;
                    if !cancelled() {
                        started = Some(began.elapsed().as_secs_f64());
                    }
                }
                total.chars += report.chars;
                total.phonemes += report.phonemes;
                total.seconds += report.seconds;
                for symbol in report.dropped {
                    if !total.dropped.contains(&symbol) {
                        total.dropped.push(symbol);
                    }
                }
                Ok(())
            },
            &cancelled,
        );
        let result = result.and_then(|()| {
            while !cancelled() && player.is_playing()? {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(())
        });
        let underrun = player.underrun_seconds();
        player.stop();
        self.detach_pcm(&player);
        self.release_idle_models();
        let latency = started.unwrap_or(0.0);
        eprintln!("{:?} PCM: chunks={received} start={latency:.3}s audio={:.3}s underrun={underrun:.3}s startup_target={startup_target:.3}s peak_buffer={peak_buffer:.3}s", settings.engine, total.seconds);
        result.map(|()| (total, latency, underrun))
    }

    /// Chatterbox's synthesis core: check the language and the clip, load (or reuse) the
    /// sessions, run the graphs.
    ///
    /// The two refusals come *before* the load, deliberately. Both are cheap to detect and
    /// both would otherwise be paid for with several seconds of graph loading followed by
    /// garbage: an unsupported language, and a reference clip that is not on disk.
    fn synthesize_chatterbox(
        &self,
        dir: &Path,
        settings: &Settings,
        text: &str,
    ) -> Result<Vec<f32>, String> {
        let language = settings.chatterbox.voice.clone();

        let clip = voices::reference_path(dir, settings.chatterbox.ref_audio.as_deref());
        if !clip.is_file() {
            return Err(format!(
                "the reference voice {:?} is not on disk; add it again or pick the built-in voice",
                clip.file_name().unwrap_or_default()
            ));
        }

        let exaggeration = settings.chatterbox.exaggeration;
        *self.chatterbox_keep_warm.lock().unwrap() = settings.chatterbox.keep_warm;

        let mut guard = self.chatterbox.lock().unwrap();
        let voice = clip.to_string_lossy().into_owned();
        let exaggeration_key = format!("{exaggeration}");
        let stale = guard.as_ref().is_none_or(|current| {
            current.voice != voice
                || current.language != language
                || current.exaggeration != exaggeration_key
        });
        if stale {
            *guard = Some(ChatterboxLoaded {
                voice,
                language: language.clone(),
                exaggeration: exaggeration_key,
                engine: Chatterbox::load(dir, exaggeration)?,
            });
        }
        let engine = &mut guard.as_mut().expect("just loaded").engine;
        let utterance = engine.synthesize(text, &language, &clip)?;
        eprintln!(
            "[TextHalo] chatterbox {} {:?}: {} steps, {} speech tokens, {:.2}s of audio in {:.2}s \
             (encoder {:.2}s, loop {:.2}s, decoder {:.2}s)",
            language,
            clip.file_name().unwrap_or_default(),
            utterance.steps,
            utterance.speech_tokens,
            utterance.seconds(),
            utterance.encoder_seconds + utterance.loop_seconds + utterance.decoder_seconds,
            utterance.encoder_seconds,
            utterance.loop_seconds,
            utterance.decoder_seconds,
        );
        if utterance.hit_max {
            return Err(format!(
                "Chatterbox ran out of steps after {} tokens without finishing the sentence; try \
                 a shorter selection",
                chatterbox::MAX_NEW_TOKENS
            ));
        }
        Ok(utterance.samples)
    }

    /// The Kokoro synthesis core, separated from playback so it can be tested without a
    /// speaker.
    fn synthesize_kokoro(
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
            None => self.phonemize(dir, text, voice.starts_with('b'))?,
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

    fn phonemize(&self, dir: &Path, text: &str, british: bool) -> Result<String, String> {
        let mut guard = self.g2p.lock().unwrap();
        if guard.is_none() {
            *guard = Some(G2p::from_dir(&dir.join("lexicon"))?);
        }
        let espeak = crate::espeak::EspeakNg::detect();
        Ok(guard.as_ref().expect("just loaded").phonemize_with_espeak(
            text,
            espeak.as_ref(),
            british,
        ))
    }

    /// Where the spoken WAV lives. One path per process: a new utterance stops the player
    /// and overwrites it, so there is never an orphaned file and never a file being read
    /// while it is written.
    fn wav_path(&self) -> Result<PathBuf, String> {
        let dir =
            engine_paths::app_support_dir().ok_or("cannot locate the app support directory")?;
        Ok(dir.join("cache").join("spoken.wav"))
    }

    pub(crate) fn play(&self, wav: &Path) -> Result<(), String> {
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

    /// Called while the speech-job lock proves this is still the active request.
    pub(crate) fn activate_pcm(&self, player: Arc<crate::pcm::Player>) {
        *self.pcm.lock().unwrap() = Some(player);
    }

    fn detach_pcm(&self, player: &Arc<crate::pcm::Player>) {
        let mut current = self.pcm.lock().unwrap();
        if current.as_ref().is_some_and(|p| Arc::ptr_eq(p, player)) {
            current.take();
        }
    }

    /// Silence whatever is playing. Safe to call when idle.
    pub fn stop(&self) {
        self.speech.stop();
        if let Some(player) = self.pcm.lock().unwrap().as_ref() {
            player.stop();
        }
        let mut guard = self.player.lock().unwrap();
        if let Some(mut child) = guard.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        drop(guard);
        self.release_idle_models();
    }

    fn release_idle_models(&self) {
        // Release the graph unless the user asked to keep it. Stopping is not the same as
        // unloading: with `keep_warm` on, the next selection speaks immediately instead of
        // waiting for 325 MB to be read off disk again.
        if !*self.keep_warm.lock().unwrap() {
            if let Ok(mut engine) = self.kokoro.try_lock() {
                *engine = None;
            }
        }
        // Chatterbox's sessions are 1.5 GB resident, so this is its own switch: a user may
        // well want Kokoro kept warm and Chatterbox released.
        if !*self.chatterbox_keep_warm.lock().unwrap() {
            if let Ok(mut engine) = self.chatterbox.try_lock() {
                *engine = None;
            }
        }
    }

    pub fn is_speaking(&self) -> bool {
        if self
            .pcm
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|p| p.is_playing().unwrap_or(false))
        {
            return true;
        }
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
        Engine::Chatterbox => "Chatterbox",
    }
}

/// Is this language one Chatterbox could read *here*?
///
/// Two of the 23 have no normaliser in this build, so the refusal names the language rather
/// than handing the checkpoint text it was never trained to read. Checked before loading
/// anything: the answer does not depend on what is on disk, so it must not cost 1.5 GB to
/// find out.
fn chatterbox_language_guard(language: &str) -> Result<(), String> {
    if crate::engines::chatterbox_language(language).is_none() {
        return Err(format!(
            "'{language}' is not one of Chatterbox's 23 languages"
        ));
    }
    if let Some(reason) = crate::engines::chatterbox_language_blocked(language) {
        return Err(format!(
            "Chatterbox cannot read {} yet: {reason}",
            crate::engines::chatterbox_language(language).unwrap_or(language)
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Settings;

    #[test]
    fn cancelled_stream_cleanup_preserves_current_player() {
        let spoken = Spoken::new();
        let old = Arc::new(crate::pcm::Player::new().unwrap());
        let current = Arc::new(crate::pcm::Player::new().unwrap());
        spoken.activate_pcm(old.clone());
        spoken.activate_pcm(current.clone());
        spoken.detach_pcm(&old);
        assert!(Arc::ptr_eq(
            spoken.pcm.lock().unwrap().as_ref().unwrap(),
            &current
        ));
        spoken.detach_pcm(&current);
        assert!(spoken.pcm.lock().unwrap().is_none());
    }

    #[test]
    fn an_engine_that_cannot_work_refuses_instead_of_falling_back() {
        // Chatterbox is routed, not skipped. Two different refusals have to reach the user:
        // the weights may be missing, and two of its languages may have no normaliser here.
        let settings = Settings {
            engine: Engine::Chatterbox,
            ..Default::default()
        };
        let spoken = Spoken::new();
        let error = spoken
            .speak(&settings, "hello")
            .expect_err("with no weights on disk this must not return success");
        assert!(
            error.contains("Chatterbox"),
            "the refusal should name the engine, got: {error}"
        );
        assert!(
            error.contains("not installed"),
            "the refusal should name the missing piece, got: {error}"
        );

        // A language code travels as far as the refusal: a user who picked Japanese is told
        // about Japanese, not about a generic failure.
        let japanese = Settings {
            engine: Engine::Chatterbox,
            chatterbox: crate::config::ChatterboxSettings {
                voice: "ja".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };
        let error = Spoken::new()
            .speak(&japanese, "hello")
            .expect_err("ja has no front end");
        assert!(error.contains("Japanese"), "got: {error}");

        let hebrew = Settings {
            engine: Engine::Chatterbox,
            chatterbox: crate::config::ChatterboxSettings {
                voice: "he".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };
        let error = Spoken::new()
            .speak(&hebrew, "hello")
            .expect_err("he has no front end");
        assert!(error.contains("Hebrew"), "got: {error}");
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

    fn assert_real_stream(engine: Engine) {
        let mut settings = Settings {
            engine,
            ..Settings::default()
        };
        settings.kokoro.keep_warm = false;
        settings.chatterbox.keep_warm = false;
        let spoken = Spoken::new();
        let mut starts = 0;
        let (report, latency, underrun) = spoken
            .stream_pcm(
                &settings,
                "Hello there. Good morning.",
                || false,
                |player| {
                    starts += 1;
                    player.start()
                },
            )
            .expect("stream speech");
        println!(
            "{engine:?} PCM start={latency:.3}s audio={:.3}s underrun={underrun:.3}s",
            report.seconds
        );
        assert_eq!(starts, 1, "a reading must start one continuous player");
        assert_eq!(report.chars, "Hello there. Good morning.".chars().count());
        assert!(report.seconds > 0.5);
        assert!(report.dropped.is_empty());
        assert!(!spoken.is_speaking());
        assert!(
            spoken.player.lock().unwrap().is_none(),
            "must not spawn afplay"
        );
        assert!(spoken.kokoro.lock().unwrap().is_none());
        assert!(spoken.chatterbox.lock().unwrap().is_none());
    }

    #[test]
    #[ignore = "plays real Kokoro through the continuous PCM output"]
    fn kokoro_pcm_latency_and_stop() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let spoken = Spoken::new();
        let settings = Settings {
            engine: Engine::Kokoro,
            ..Settings::default()
        };
        let text = "The next sentence is ready while you listen to this one. Reading should feel natural, with enough context for the voice to carry a thought from the beginning of a sentence to its end. A continuous audio buffer lets the application prepare the next sentence while the current sentence is playing, so the listener hears a steady voice instead of a series of separate recordings.";
        for pass in 0..2 {
            let began = std::time::Instant::now();
            let mut starts = 0;
            let (report, latency, underrun) = spoken
                .stream_pcm(
                    &settings,
                    text,
                    || false,
                    |player| {
                        starts += 1;
                        player.start()
                    },
                )
                .unwrap();
            println!("PCM BENCH pass={pass} start={latency:.3}s underrun={underrun:.3}s audio={:.3}s elapsed={:.3}s", report.seconds, began.elapsed().as_secs_f64());
            assert_eq!(starts, 1, "one native start per reading");
            assert_eq!(report.chars, text.chars().count());
            assert!(report.dropped.is_empty());
            assert_eq!(underrun, 0., "the buffer ran dry during playback");
            assert!(
                began.elapsed().as_secs_f64() >= latency + report.seconds as f64 - 0.1,
                "must drain audible audio, not just wait for buffer callbacks"
            );
            assert!(!spoken.is_speaking());
            assert!(
                spoken.player.lock().unwrap().is_none(),
                "PCM must not spawn afplay"
            );
        }
        let cancelled = AtomicBool::new(false);
        let mut starts = 0;
        spoken
            .stream_pcm(
                &settings,
                text,
                || cancelled.load(Ordering::SeqCst),
                |player| {
                    starts += 1;
                    player.start()?;
                    let now = std::time::Instant::now();
                    spoken.stop();
                    cancelled.store(true, Ordering::SeqCst);
                    assert!(!player.is_playing()?);
                    assert_eq!(player.buffered_seconds(), 0.);
                    println!("PCM STOP {:.3}s", now.elapsed().as_secs_f64());
                    Ok(())
                },
            )
            .unwrap();
        assert_eq!(starts, 1);
        assert!(!spoken.is_speaking());
    }

    #[test]
    #[ignore = "plays real Chatterbox through the continuous PCM output"]
    fn chatterbox_pcm_latency_and_stop() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let spoken = Spoken::new();
        let mut settings = Settings {
            engine: Engine::Chatterbox,
            ..Settings::default()
        };
        settings.chatterbox.keep_warm = true;
        let text = "Hello there. Good morning. Welcome back. How are you? Have a nice day.";
        for pass in 0..2 {
            let mut starts = 0;
            let began = std::time::Instant::now();
            let (report, latency, underrun) = spoken
                .stream_pcm(
                    &settings,
                    text,
                    || false,
                    |player| {
                        starts += 1;
                        player.start()
                    },
                )
                .unwrap();
            println!("CHATTERBOX PCM pass={pass} start={latency:.3}s audio={:.3}s underrun={underrun:.3}s elapsed={:.3}s", report.seconds, began.elapsed().as_secs_f64());
            assert_eq!(starts, 1);
            assert_eq!(report.chars, text.chars().count());
            assert!(report.seconds > 3.);
            assert!(
                spoken.player.lock().unwrap().is_none(),
                "must not spawn afplay"
            );
            assert!(
                spoken.chatterbox.lock().unwrap().is_some(),
                "keep-warm retains model"
            );
            assert!(!spoken.is_speaking());
        }
        let cancelled = AtomicBool::new(false);
        let mut starts = 0;
        spoken
            .stream_pcm(
                &settings,
                text,
                || cancelled.load(Ordering::SeqCst),
                |player| {
                    starts += 1;
                    player.start()?;
                    let began = std::time::Instant::now();
                    spoken.stop();
                    cancelled.store(true, Ordering::SeqCst);
                    assert!(!player.is_playing()?);
                    assert_eq!(player.buffered_seconds(), 0.);
                    println!("CHATTERBOX PCM STOP {:.3}s", began.elapsed().as_secs_f64());
                    Ok(())
                },
            )
            .unwrap();
        assert_eq!(starts, 1);
        assert!(!spoken.is_speaking());
        *spoken.chatterbox_keep_warm.lock().unwrap() = false;
        spoken.stop();
        assert!(spoken.chatterbox.lock().unwrap().is_none());
    }

    #[test]
    #[ignore = "measures real installed Kokoro latency"]
    fn benchmark_kokoro_sentence_lengths() {
        let spoken = Spoken::new();
        let settings = Settings {
            engine: Engine::Kokoro,
            ..Settings::default()
        };
        let cases = [
            "The next sentence is ready while you listen to this one.",
            "Reading should feel natural, with enough context for the voice to carry a thought from the beginning of a sentence to its end.",
            "A continuous audio buffer lets the application prepare the next sentence while the current sentence is playing, so the listener hears a steady voice instead of a series of separate recordings.",
            "When a paragraph contains a longer sentence, keeping its clauses together gives the speech model more context for pronunciation and rhythm, while a bounded audio buffer allows the next part to be generated ahead of playback without storing the entire document in memory.",
        ];
        for pass in 0..3 {
            for text in cases {
                let started = std::time::Instant::now();
                let (_, report, _) = spoken.synthesize_audio(&settings, text).unwrap();
                let elapsed = started.elapsed().as_secs_f64();
                println!(
                    "BENCH pass={pass} chars={} synthesis={elapsed:.3}s audio={:.3}s rtf={:.3}",
                    text.chars().count(),
                    report.seconds,
                    elapsed / report.seconds as f64
                );
            }
        }
    }

    #[test]
    #[ignore = "needs installed Kokoro weights and espeak-ng"]
    fn real_kokoro_pronounces_unknown_word_with_optional_cli() {
        assert!(crate::espeak::EspeakNg::detect().is_some());
        let dir = engine_paths::kokoro_dir().unwrap();
        let spoken = Spoken::new();
        assert_eq!(spoken.phonemize(&dir, "kiegen", false).unwrap(), "kˈiʤən");
        let settings = Settings {
            engine: Engine::Kokoro,
            ..Settings::default()
        };
        let (samples, _, dropped) = spoken
            .synthesize_kokoro(&dir, &settings, "Hello kiegen.")
            .unwrap();
        assert!(dropped.is_empty(), "unsupported phonemes: {dropped:?}");
        assert!(samples.len() > 12000);
        assert!(samples.iter().any(|s| s.abs() > 0.01));
    }

    #[test]
    #[ignore = "needs Kokoro weights and plays test audio"]
    fn streams_real_kokoro_audio() {
        assert_real_stream(Engine::Kokoro);
    }

    #[test]
    #[ignore = "needs Chatterbox weights and plays test audio"]
    fn streams_real_chatterbox_audio() {
        assert_real_stream(Engine::Chatterbox);
    }

    #[test]
    #[ignore = "needs Kokoro weights and briefly starts test audio"]
    fn stopping_a_real_stream_discards_later_chunks() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let settings = Settings {
            engine: Engine::Kokoro,
            ..Settings::default()
        };
        let spoken = Spoken::new();
        let stopped = AtomicBool::new(false);
        let mut count = 0;
        spoken
            .stream_pcm(
                &settings,
                "Hello there. Good morning. Have a nice day.",
                || stopped.load(Ordering::SeqCst),
                |player| {
                    spoken.activate_pcm(player.clone());
                    player.start()?;
                    count += 1;
                    spoken.stop();
                    stopped.store(true, Ordering::SeqCst);
                    Ok(())
                },
            )
            .unwrap();
        assert_eq!(count, 1);
        assert!(!spoken.is_speaking());
    }

    #[test]
    #[ignore = "needs both installed engines and plays test audio"]
    fn switching_real_engines_keeps_playback_usable() {
        let spoken = Spoken::new();
        for engine in [
            Engine::Kokoro,
            Engine::Chatterbox,
            Engine::Kokoro,
            Engine::Chatterbox,
        ] {
            spoken.stop();
            let settings = Settings {
                engine,
                ..Settings::default()
            };
            let mut started = false;
            let (report, _, _) = spoken
                .stream_pcm(
                    &settings,
                    "Hello.",
                    || false,
                    |player| {
                        spoken.activate_pcm(player.clone());
                        player.start()?;
                        started = true;
                        Ok(())
                    },
                )
                .unwrap();
            assert!(started, "{engine:?} did not start");
            assert!(report.seconds > 0.0);
            assert!(spoken.pcm.lock().unwrap().is_none());
        }
    }

    /// Real-model cache regression: remove only our temporary link to the graphs
    /// after the first call. A cache hit must not try opening them again.
    #[test]
    #[ignore = "needs the installed Chatterbox weights"]
    fn chatterbox_reuses_loaded_graphs_on_repeated_calls() {
        use std::os::unix::fs::symlink;
        use std::time::Instant;
        let source = engine_paths::chatterbox_dir().expect("app support directory");
        assert!(
            engine_paths::chatterbox_installed(),
            "install Chatterbox first"
        );
        let dir =
            std::env::temp_dir().join(format!("kiegen-chatterbox-cache-{}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        for entry in std::fs::read_dir(&source).unwrap() {
            let entry = entry.unwrap();
            symlink(entry.path(), dir.join(entry.file_name())).unwrap();
        }
        let mut settings = Settings {
            engine: Engine::Chatterbox,
            ..Settings::default()
        };
        settings.chatterbox.keep_warm = true;
        let spoken = Spoken::new();
        let started = Instant::now();
        let first = spoken
            .synthesize_chatterbox(&dir, &settings, "Hello.")
            .expect("first call");
        println!(
            "cold call: {:.2}s, {} samples",
            started.elapsed().as_secs_f64(),
            first.len()
        );
        assert!(!first.is_empty());
        spoken.stop();
        assert!(
            spoken.chatterbox.lock().unwrap().is_some(),
            "keep warm must survive Stop"
        );
        std::fs::remove_file(dir.join("onnx")).unwrap();
        let started = Instant::now();
        let second = spoken.synthesize_chatterbox(&dir, &settings, "Hello.");
        println!("warm call: {:.2}s", started.elapsed().as_secs_f64());
        // Remove only this test's symlink tree, including on a failed cache lookup.
        std::fs::remove_dir_all(&dir).unwrap();
        let second = second.expect("a cached call must not reopen the model files");
        assert_eq!(first.len(), second.len());
        assert!(
            second.iter().any(|s| s.abs() > 0.01),
            "cached output is silent"
        );
        *spoken.chatterbox_keep_warm.lock().unwrap() = false;
        spoken.stop();
        assert!(
            spoken.chatterbox.lock().unwrap().is_none(),
            "disabling retention must unload"
        );
    }

    #[test]
    fn stop_does_not_wait_for_an_in_flight_model() {
        use std::sync::{mpsc, Arc};
        use std::time::Duration;
        let spoken = Arc::new(Spoken::new());
        *spoken.keep_warm.lock().unwrap() = false;
        *spoken.chatterbox_keep_warm.lock().unwrap() = false;
        let kokoro = spoken.kokoro.lock().unwrap();
        let chatterbox = spoken.chatterbox.lock().unwrap();
        let (tx, rx) = mpsc::channel();
        let worker = spoken.clone();
        let thread = std::thread::spawn(move || {
            worker.stop();
            tx.send(()).unwrap();
        });
        let stopped = rx.recv_timeout(Duration::from_millis(250));
        drop(kokoro);
        drop(chatterbox);
        thread.join().unwrap();
        assert!(stopped.is_ok(), "Stop waited for model synthesis to finish");
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
