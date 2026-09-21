//! Does a selection actually come out as speech?
//!
//! `install.rs` answers "can the weights be fetched and loaded". `g2p_parity.rs` answers
//! "are the phonemes right". This is the one that answers the question the app exists for:
//! **text in, audio out, through the same code the shortcut calls** — not a copy of it, and
//! not a mock.
//!
//! It is `#[ignore]`d because it needs a real ~346 MB install. Point it at one:
//!
//! ```bash
//! KIEGEN_MODELS_DIR=/some/dir cargo test --test speak -- --ignored --nocapture
//! ```
//!
//! The WAV it writes is the artifact worth checking by hand; the transcript of that file is
//! the proof, not these assertions:
//!
//! ```bash
//! whisper-cli -m ~/.cache/whisper/ggml-base.en.bin -otxt <the wav it printed>
//! ```

use std::path::{Path, PathBuf};

use kiegen_lib::config::{Engine, Settings};
use kiegen_lib::engine_paths;
use kiegen_lib::spoken::Spoken;

/// The support directory holding `models/kokoro`. Required rather than defaulted, so the
/// test never quietly runs against a directory that happens to exist.
fn support_dir() -> PathBuf {
    match std::env::var("KIEGEN_MODELS_DIR") {
        Ok(dir) => PathBuf::from(dir),
        Err(_) => panic!(
            "set KIEGEN_MODELS_DIR to the app support directory holding models/kokoro \
             (a fresh install takes ~346 MB)"
        ),
    }
}

/// Reads a 16-bit mono PCM WAV back: `(sample_rate, samples)`.
///
/// Parsed rather than trusted, because a writer that reports a duration the file does not
/// contain is exactly the kind of bug this test is here to catch.
fn read_wav(path: &Path) -> (u32, Vec<f32>) {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
    assert!(bytes.len() > 44, "{path:?} is too short to be a wav");
    assert_eq!(&bytes[0..4], b"RIFF", "{path:?} is not a RIFF file");
    assert_eq!(&bytes[8..12], b"WAVE", "{path:?} is not a WAVE file");
    assert_eq!(&bytes[12..16], b"fmt ", "no fmt chunk in {path:?}");

    let format = u16::from_le_bytes([bytes[20], bytes[21]]);
    let channels = u16::from_le_bytes([bytes[22], bytes[23]]);
    let rate = u32::from_le_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]);
    let bits = u16::from_le_bytes([bytes[34], bytes[35]]);
    assert_eq!(format, 1, "expected PCM, got format {format}");
    assert_eq!(channels, 1, "expected mono, got {channels} channels");
    assert_eq!(bits, 16, "expected 16-bit, got {bits}");

    assert_eq!(&bytes[36..40], b"data", "no data chunk in {path:?}");
    let samples = bytes[44..]
        .chunks_exact(2)
        .map(|pair| i16::from_le_bytes([pair[0], pair[1]]) as f32 / 32768.0)
        .collect();
    (rate, samples)
}

fn kokoro_settings(voice: &str) -> Settings {
    Settings {
        engine: Engine::Kokoro,
        kokoro: kiegen_lib::config::KokoroSettings {
            voice: voice.to_string(),
            ..Default::default()
        },
        ..Default::default()
    }
}

#[test]
#[ignore = "needs a real Kokoro install; see the module note"]
fn a_selection_becomes_audible_speech() {
    let support = support_dir();
    assert!(
        engine_paths::kokoro_installed(),
        "no complete Kokoro install under {support:?} — run the installer first"
    );

    // Deliberately not a pangram. This is the text a user would actually select: a sentence
    // with a currency amount, a decimal temperature and a month, because those are the paths
    // that go through the number and currency rules rather than the dictionary.
    let text = "The quick brown fox jumps over the lazy dog. \
                It costs $3.50 and the record was 21 degrees in November 2005.";

    let out = support.join("cache").join("speak-it.wav");
    let spoken = Spoken::new();
    let report = spoken
        .render(&kokoro_settings("af_heart"), text, &out)
        .expect("render the selection to a wav");

    println!("text      {text}");
    println!(
        "report    {} chars -> {} phonemes, {}",
        report.chars,
        report.phonemes,
        report.summary()
    );

    // 1. The text reached the engine as phonemes at all. An empty front end would otherwise
    //    "succeed" and produce a zero-length file.
    assert!(
        report.phonemes > 40,
        "{} phonemes is too few for {} characters of text",
        report.phonemes,
        report.chars
    );

    // 2. Nothing was silently dropped. Kokoro discards symbols it cannot encode, which is
    //    how a word disappears from the audio without any error anywhere.
    assert!(
        report.dropped.is_empty(),
        "the vocabulary could not encode {:?}",
        report.dropped
    );

    // 3. The file on disk is what the report claimed: same duration, right format.
    let (rate, samples) = read_wav(&out);
    assert_eq!(rate, 24_000, "Kokoro is a 24 kHz model");
    let measured = samples.len() as f32 / rate as f32;
    assert!(
        (measured - report.seconds).abs() < 0.02,
        "the report says {:.2}s, the file holds {measured:.2}s",
        report.seconds
    );

    // 4. It is speech-length audio, not a click and not a runaway. ~110 characters at a
    //    normal reading rate lands around 7 s; the bounds are wide on purpose.
    assert!(
        (4.0..25.0).contains(&measured),
        "{measured:.2}s for {} characters is not a plausible reading",
        report.chars
    );

    // 5. There is actual signal in it. `Ok` plus a valid header is also what a silent file
    //    looks like.
    let peak = samples.iter().fold(0f32, |top, s| top.max(s.abs()));
    let rms = (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt();
    let mean = samples.iter().sum::<f32>() / samples.len() as f32;
    println!("audio     peak {peak:.3}, rms {rms:.4}, mean {mean:+.5}");
    assert!(
        peak > 0.02,
        "the file is effectively silent (peak {peak:.4})"
    );
    assert!(rms > 0.005, "almost no energy in the file (rms {rms:.5})");
    assert!(
        mean.abs() < 0.02,
        "the signal sits on a DC offset ({mean:+.4}), which is not speech"
    );

    // 6. The routing the shortcut uses: `speak` plays through the player, and `is_speaking`
    //    becomes true. The player is stopped again immediately so the test does not talk
    //    over whatever the machine is doing — the assertion is about the spawn, not the
    //    sound (the sound is what the transcript below is for).
    spoken
        .speak(&kokoro_settings("af_heart"), text)
        .expect("speak the selection");
    assert!(
        spoken.is_speaking(),
        "speak returned Ok but nothing is playing"
    );
    spoken.stop();
    assert!(
        !spoken.is_speaking(),
        "stop left the player running, so the next utterance would talk over this one"
    );

    // 7. A different voice reloads instead of speaking the previous one's style table.
    //    A separate path: reusing `out` here would overwrite the artifact above, and the
    //    file left on disk would no longer be the one this test measured.
    let second = support.join("cache").join("speak-it-other-voice.wav");
    let other = spoken
        .render(&kokoro_settings("am_michael"), &text[..40], &second)
        .expect("render with a second voice");
    println!("second voice af_heart -> am_michael: {}", other.summary());
    assert!(other.phonemes > 0);
    let (_, second_samples) = read_wav(&second);
    let second_seconds = second_samples.len() as f32 / 24_000.0;
    assert!(
        (second_seconds - other.seconds).abs() < 0.02,
        "the second voice's report says {:.2}s, the file holds {second_seconds:.2}s",
        other.seconds
    );

    println!("\nwav       {}", out.display());
    println!("check it: whisper-cli -m ~/.cache/whisper/ggml-base.en.bin -otxt {out:?}");
}
