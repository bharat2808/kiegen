//! End-to-end checks for the Chatterbox engine, against the real 1.5 GB multilingual weights.
//!
//! This is the only test that answers "does a selection come out as speech?" for this engine.
//! The unit tests cover the language prefix, the Hangul and Cangjie conversions, the position
//! arithmetic and the sampler; none of those can tell you whether the four graphs chain, whether
//! the range of token ids the model actually emits is the range this code assumed, or whether
//! the audio is silence.
//!
//! Skipped, loudly, when the weights are not on disk — a machine without 1.5 GB of ONNX files is
//! a supported configuration, not a broken one:
//!
//! ```text
//! KIEGEN_MODELS_DIR=/some/dir cargo test --test chatterbox -- --nocapture
//! ```
//!
//! The directory must hold `models/chatterbox/` in the layout the download plan produces.
//! Every WAV it writes is printed with its size, duration, peak and RMS, because the artifacts
//! are the evidence — the assertions only guard against the obvious failures.

use std::path::{Path, PathBuf};

use kiegen_lib::chatterbox::{self, Chatterbox};
use kiegen_lib::config::{ChatterboxSettings, Engine, Settings};
use kiegen_lib::spoken::Spoken;
use kiegen_lib::voices;

/// The sentence the turbo spike and the Kokoro parity run both used, so durations are
/// comparable across engines.
const ENGLISH: &str = "The quick brown fox jumps over the lazy dog.";
const FRENCH: &str = "Le renard brun rapide saute par-dessus le chien paresseux.";

/// Where the artifacts go. `KIEGEN_CHATTERBOX_OUT` overrides it so a run can be re-read later.
fn out_dir() -> PathBuf {
    match std::env::var("KIEGEN_CHATTERBOX_OUT") {
        Ok(dir) => PathBuf::from(dir),
        Err(_) => std::env::temp_dir().join("kiegen-chatterbox-it"),
    }
}

/// The engine's own directory, or `None` when this machine has no weights.
fn weights_or_skip() -> Option<PathBuf> {
    let Ok(support) = std::env::var("KIEGEN_MODELS_DIR") else {
        eprintln!("skipping: KIEGEN_MODELS_DIR is not set (see the module note)");
        return None;
    };
    std::env::set_var("KIEGEN_MODELS_DIR", &support);
    if !kiegen_lib::engine_paths::chatterbox_installed() {
        eprintln!("skipping: no complete Chatterbox install under {support:?}");
        return None;
    }
    kiegen_lib::engine_paths::chatterbox_dir()
}

/// Reads a 16-bit mono PCM WAV back: `(sample_rate, samples)`. Parsed rather than trusted,
/// because a writer that reports a duration the file does not contain is exactly the kind of
/// bug these assertions exist to catch.
fn read_wav(path: &Path) -> (u32, Vec<f32>) {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
    assert!(bytes.len() > 44, "{path:?} is too short to be a wav");
    assert_eq!(&bytes[0..4], b"RIFF", "{path:?} is not a RIFF file");
    assert_eq!(&bytes[8..12], b"WAVE", "{path:?} is not a WAVE file");
    assert_eq!(u16::from_le_bytes([bytes[20], bytes[21]]), 1, "not PCM");
    assert_eq!(u16::from_le_bytes([bytes[22], bytes[23]]), 1, "not mono");
    let rate = u32::from_le_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]);
    assert_eq!(&bytes[36..40], b"data", "no data chunk in {path:?}");
    let samples = bytes[44..]
        .chunks_exact(2)
        .map(|pair| i16::from_le_bytes([pair[0], pair[1]]) as f32 / 32768.0)
        .collect();
    (rate, samples)
}

/// `(bytes, seconds, peak, rms)`, printed for every artifact.
fn describe(path: &Path) -> (u64, f32, f32, f32) {
    let bytes = std::fs::metadata(path).expect("stat").len();
    let (rate, samples) = read_wav(path);
    let seconds = samples.len() as f32 / rate as f32;
    let peak = samples.iter().fold(0f32, |a, b| a.max(b.abs()));
    let rms = (samples
        .iter()
        .map(|s| (*s as f64) * (*s as f64))
        .sum::<f64>()
        / samples.len().max(1) as f64)
        .sqrt() as f32;
    println!(
        "  {:<28} {:>9} bytes  {:>6.2}s  peak {:.4}  rms {:.4} @ {rate} Hz",
        path.file_name().unwrap_or_default().to_string_lossy(),
        bytes,
        seconds,
        peak,
        rms
    );
    (bytes, seconds, peak, rms)
}

fn chatterbox_settings(language: &str, clip: Option<&str>) -> Settings {
    Settings {
        engine: Engine::Chatterbox,
        chatterbox: ChatterboxSettings {
            voice: language.to_string(),
            ref_audio: clip.map(str::to_string),
            keep_warm: false,
            ..Default::default()
        },
        ..Default::default()
    }
}

/// The whole pipeline, three times: English on the built-in clip, French on the same clip, and
/// English again on a *different* reference voice the user added.
///
/// That third one is the cloning feature end to end — the file is picked from disk, copied into
/// the app's own store, listed as a selectable voice, and used for the utterance.
#[test]
fn a_selection_becomes_audible_speech_in_two_languages_and_two_voices() {
    let Some(dir) = weights_or_skip() else {
        return;
    };
    let out = out_dir();
    std::fs::create_dir_all(&out).expect("create the output directory");

    println!("\nChatterbox end to end, weights at {dir:?}");

    // ── load ─────────────────────────────────────────────────────────────────────
    let started = std::time::Instant::now();
    let mut engine =
        Chatterbox::load(&dir, 0.5).expect("the four graphs must load in ONNX Runtime");
    let load_seconds = started.elapsed().as_secs_f64();

    // ── the built-in clip, twice ─────────────────────────────────────────────────
    let builtin = voices::reference_path(&dir, None);
    println!("built-in reference clip: {builtin:?}");

    let started = std::time::Instant::now();
    let english = engine
        .synthesize(ENGLISH, "en", &builtin)
        .expect("English must synthesize");
    let english_seconds = started.elapsed().as_secs_f64();
    report("en / built-in", &english, load_seconds, english_seconds);
    let en_path = out.join("en_builtin.wav");
    kiegen_lib::kokoro::write_wav(&en_path, &english.samples, chatterbox::SAMPLE_RATE).unwrap();
    let (_, en_audio_seconds, en_peak, en_rms) = describe(&en_path);

    let started = std::time::Instant::now();
    let french = engine
        .synthesize(FRENCH, "fr", &builtin)
        .expect("French must synthesize");
    let french_seconds = started.elapsed().as_secs_f64();
    report("fr / built-in", &french, load_seconds, french_seconds);
    let fr_path = out.join("fr_builtin.wav");
    kiegen_lib::kokoro::write_wav(&fr_path, &french.samples, chatterbox::SAMPLE_RATE).unwrap();
    let (_, fr_audio_seconds, fr_peak, fr_rms) = describe(&fr_path);

    // A sentence's worth of speech, not a click and not silence. The turbo port produced
    // 3.24 s for this sentence, and this model is larger and slower but not shorter.
    for (label, seconds, peak, rms) in [
        ("English", en_audio_seconds, en_peak, en_rms),
        ("French", fr_audio_seconds, fr_peak, fr_rms),
    ] {
        assert!(
            seconds > 1.5,
            "{label} produced {seconds:.2}s, which is too little to be the sentence"
        );
        assert!(
            peak > 0.05,
            "{label} peaked at {peak:.4} — that is silence, not speech"
        );
        assert!(
            rms > 0.005,
            "{label} has an RMS of {rms:.5} — that is silence, not speech"
        );
    }

    // The measurement behind the module's 8194-vocabulary note. The speech codes are at the
    // *bottom* of the vocabulary: what the loop generates is 0..6560 plus the two specials, and
    // nothing above STOP_SPEECH. That is the opposite of the "6561 is where speech starts"
    // reading, and it is the reading a mask would have to get right.
    for (label, utterance) in [("English", &english), ("French", &french)] {
        assert_eq!(
            utterance.above_stop, 0,
            "{label} generated {} id(s) above STOP_SPEECH (6562); the module doc says that \
             range is unused, so either it is wrong or this checkpoint uses it",
            utterance.above_stop
        );
        assert!(
            utterance.speech_code_high < chatterbox::START_SPEECH_TOKEN,
            "{label}'s highest speech code is {} — above START_SPEECH, so the codes are not \
             where the doc says they are ({}..{})",
            utterance.speech_code_high,
            utterance.speech_code_low,
            utterance.speech_code_high
        );
    }

    // The two languages must not produce the same audio: a language prefix that never reached
    // the tokenizer would give two identical utterances from two different sentences, and
    // nothing about "not silent" would notice.
    assert!(
        (en_audio_seconds - fr_audio_seconds).abs() > 0.05,
        "English and French came out the same length ({en_audio_seconds:.2}s vs \
         {fr_audio_seconds:.2}s); the language prefix may not be reaching the model"
    );

    // ── a second, different reference clip ───────────────────────────────────────
    match make_reference_clip(&out) {
        Some(source) => {
            let before = voices::list().len();
            let clip = voices::add(&source).expect("the clip must be accepted");
            assert_eq!(
                voices::list().len(),
                before + 1,
                "the new clip must appear in the voice list"
            );
            println!("added voice {:?} ({:.1}s)", clip.file, clip.seconds);

            // The stored clip is what synthesis reads, reached the way the config reaches it.
            let chosen = voices::reference_path(&dir, Some(&clip.file));
            assert!(chosen.is_file(), "{chosen:?} must exist after add()");
            let started = std::time::Instant::now();
            let cloned = engine
                .synthesize(ENGLISH, "en", &chosen)
                .expect("the cloned voice must synthesize");
            let cloned_seconds = started.elapsed().as_secs_f64();
            report("en / added clip", &cloned, load_seconds, cloned_seconds);
            let clone_path = out.join("en_cloned.wav");
            kiegen_lib::kokoro::write_wav(&clone_path, &cloned.samples, chatterbox::SAMPLE_RATE)
                .unwrap();
            let (_, clone_audio_seconds, clone_peak, _clone_rms) = describe(&clone_path);

            assert!(
                clone_audio_seconds > 1.5,
                "the cloned voice produced {clone_audio_seconds:.2}s"
            );
            assert!(
                clone_peak > 0.05,
                "the cloned voice peaked at {clone_peak:.4}"
            );

            // A different speaker has to change the waveform. Compare the first second of the
            // two English renders sample by sample: identical output would mean the reference
            // clip is being ignored, which is the whole feature.
            let (_, builtin_samples) = read_wav(&en_path);
            let (_, cloned_samples) = read_wav(&clone_path);
            let common = builtin_samples.len().min(cloned_samples.len()).min(24000);
            let difference: f32 = builtin_samples[..common]
                .iter()
                .zip(&cloned_samples[..common])
                .map(|(a, b)| (a - b).abs())
                .sum::<f32>()
                / common as f32;
            println!("  mean |built-in - cloned| over the first second: {difference:.4}");
            assert!(
                difference > 0.01,
                "the two reference clips produced near-identical audio ({difference:.5}); the \
                 reference clip is not reaching the model"
            );

            voices::remove(&clip.file).expect("the clip must be deletable");
            assert_eq!(
                voices::list().len(),
                before,
                "delete must remove exactly one"
            );
        }
        None => eprintln!("skipping the cloned-voice leg: no /usr/bin/say or /usr/bin/afconvert"),
    }
}

/// Print the numbers a reader needs to judge the run.
fn report(label: &str, utterance: &chatterbox::Utterance, load_seconds: f64, seconds: f64) {
    let audio = utterance.seconds();
    println!(
        "{label}: {} steps, {} speech tokens, {audio:.2}s of audio in {seconds:.2}s \
         (rtf {:.3}, load {load_seconds:.2}s; ids {}..{}, codes {}..{}, {} above stop){}",
        utterance.steps,
        utterance.speech_tokens,
        seconds / audio as f64,
        utterance.generated_low,
        utterance.generated_high,
        utterance.speech_code_low,
        utterance.speech_code_high,
        utterance.above_stop,
        if utterance.hit_max {
            " [hit max_new_tokens]"
        } else {
            ""
        }
    );
}

/// A second speaker, made on this machine: macOS `say` renders one Apple voice to an AIFF, and
/// `afconvert` turns it into a 44.1 kHz **stereo** WAV — deliberately unlike the built-in clip,
/// so `voices::add` has to downmix and resample rather than pass it through.
fn make_reference_clip(out: &Path) -> Option<PathBuf> {
    let say = Path::new("/usr/bin/say");
    let convert = Path::new("/usr/bin/afconvert");
    if !say.is_file() || !convert.is_file() {
        return None;
    }
    let aiff = out.join("second_voice.aiff");
    let wav = out.join("second_voice_44k_stereo.wav");
    let script = "Reading this sentence gives the model a different voice to copy. \
                  The quick brown fox jumps over the lazy dog.";
    let status = std::process::Command::new(say)
        .args(["-v", "Daniel", "-o"])
        .arg(&aiff)
        .arg(script)
        .status()
        .ok()?;
    if !status.success() {
        eprintln!("say failed; skipping the cloned-voice leg");
        return None;
    }
    let status = std::process::Command::new(convert)
        .arg("-f")
        .arg("WAVE")
        .arg("-d")
        .arg("LEI16@44100")
        .arg("-c")
        .arg("2")
        .arg(&aiff)
        .arg(&wav)
        .status()
        .ok()?;
    if !status.success() {
        eprintln!("afconvert failed; skipping the cloned-voice leg");
        return None;
    }
    println!(
        "second reference clip: {wav:?} ({} bytes)",
        std::fs::metadata(&wav).map(|m| m.len()).unwrap_or(0)
    );
    Some(wav)
}

/// The same routing the shortcut uses, through `Spoken`: text in, a WAV on disk, in a language
/// that is not English. This is the path that proves `can_speak` was honest.
#[test]
fn the_router_writes_french_through_the_same_code_the_shortcut_calls() {
    let Some(_dir) = weights_or_skip() else {
        return;
    };
    let out = out_dir();
    std::fs::create_dir_all(&out).expect("create the output directory");
    let target = out.join("router_french.wav");

    let settings = chatterbox_settings("fr", None);
    let report = Spoken::new()
        .render(&settings, FRENCH, &target)
        .expect("the router must render French");

    let (_, seconds, peak, rms) = describe(&target);
    println!(
        "router: {} chars, {:.2}s of audio (report says {:.2}s)",
        report.chars, seconds, report.seconds
    );
    assert_eq!(report.chars, FRENCH.chars().count());
    assert!(seconds > 1.5, "only {seconds:.2}s came out of the router");
    assert!(peak > 0.05, "the router produced silence (peak {peak:.4})");
    assert!(rms > 0.005, "the router produced silence (rms {rms:.5})");
}

/// Two of the 23 languages have no normaliser in this build. They must refuse *before* any
/// graph is loaded, naming the language — not after 1.5 GB has been read, and never by
/// silently synthesising unnormalised text.
#[test]
fn an_unnormalised_language_refuses_without_touching_the_weights() {
    let Some(_dir) = weights_or_skip() else {
        return;
    };
    let out = out_dir();
    std::fs::create_dir_all(&out).expect("create the output directory");
    for (code, name) in [("ja", "Japanese"), ("he", "Hebrew")] {
        let started = std::time::Instant::now();
        let error = Spoken::new()
            .render(
                &chatterbox_settings(code, None),
                "テスト",
                &out.join("never-written.wav"),
            )
            .expect_err("this must refuse");
        println!("{code} refused in {:?}: {error}", started.elapsed());
        assert!(error.contains(name), "{code} said: {error}");
        assert!(
            !out.join("never-written.wav").exists(),
            "{code} wrote a file despite refusing"
        );
    }
}
