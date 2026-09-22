//! The user's own reference voices for Chatterbox.
//!
//! Chatterbox clones its speaker from a clip, so "adding a voice" is adding a file. This
//! module is the whole of that: a clip is validated, normalised to what the graphs want,
//! and copied into `models/chatterbox/voices` under a name derived from the source file.
//! Nothing here is fetched — the built-in clip arrives with the weights, and everything
//! else comes off the user's own disk.
//!
//! Two rules make the rest of the engine simple:
//!
//! * **A stored clip is always 24 kHz mono f32.** Stereo is averaged down, any other sample
//!   rate is linearly resampled, and the result is written back as 16-bit PCM. The graphs
//!   accept a buffer at the wrong rate silently and produce confident, wrong speech, so the
//!   conversion happens once at the edge rather than being hoped for at every synthesis.
//! * **A stored clip is always substantial.** Too short and there is nothing to clone from;
//!   too long and a podcast is pushed through a 591 MB encoder before a word is spoken.

use std::path::{Path, PathBuf};

use crate::chatterbox;
use crate::engine_paths;

/// One reference clip the user added.
#[derive(Debug, Clone, PartialEq)]
pub struct VoiceClip {
    /// File name inside the voices directory. This is what `chatterbox.ref_audio` stores.
    pub file: String,
    pub seconds: f32,
}

/// The built-in clip's id, so the catalogue and the config agree on what "the default" is
/// without a sentinel value: it is simply the name of a file that is always there.
pub fn builtin_file() -> &'static str {
    engine_paths::CHATTERBOX_DEFAULT_VOICE_FILE
}

/// Every clip the user has added, in a stable order. An unreadable directory is an empty
/// list, not an error: a fresh install has no voices and that is not a fault.
pub fn list() -> Vec<VoiceClip> {
    let Some(dir) = engine_paths::chatterbox_voices_dir() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut clips: Vec<VoiceClip> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().is_none_or(|extension| extension != "wav") {
                return None;
            }
            let file = path.file_name()?.to_string_lossy().into_owned();
            let seconds = chatterbox::wav_seconds(&path)?;
            Some(VoiceClip { file, seconds })
        })
        .collect();
    clips.sort_by(|a, b| a.file.cmp(&b.file));
    clips
}

/// The path a stored clip lives at, or `None` when `file` is not a plain file name.
///
/// Rejects separators and `..`: the value comes from a config file that a user can edit, and
/// a path escape there would let synthesis read any file on the machine.
pub fn clip_path(file: &str) -> Option<PathBuf> {
    if file.is_empty()
        || file.contains('/')
        || file.contains('\\')
        || file == "."
        || file == ".."
        || file.starts_with('.')
    {
        return None;
    }
    Some(engine_paths::chatterbox_voices_dir()?.join(file))
}

/// Where a synthesis should read its reference audio from.
///
/// `None` — the default in every config that has never had a voice chosen — means the clip
/// that ships with the weights. Anything else is a file name inside the voices directory.
pub fn reference_path(dir: &Path, selected: Option<&str>) -> PathBuf {
    match selected {
        None => dir.join(engine_paths::CHATTERBOX_DEFAULT_VOICE_FILE),
        Some(file) if file == builtin_file() => {
            dir.join(engine_paths::CHATTERBOX_DEFAULT_VOICE_FILE)
        }
        Some(file) => match clip_path(file) {
            Some(path) => path,
            // A name that cannot be a stored clip is not silently reinterpreted as the
            // default: the caller's existence check reports it, naming the value.
            None => dir.join(file),
        },
    }
}

/// Copy `source` in as a new voice, returning the stored clip.
///
/// The clip is decoded, downmixed, resampled and re-encoded, so what lands on disk is
/// exactly what the graphs will read. A file that is not a WAV, is silent, or falls outside
/// the length window is refused with the reason — never stored and discovered later.
pub fn add(source: &Path) -> Result<VoiceClip, String> {
    if !source.is_file() {
        return Err(format!("{source:?} is not a file"));
    }
    let samples = chatterbox::read_reference_mono_24k(source)?;
    let seconds = samples.len() as f32 / chatterbox::SAMPLE_RATE as f32;
    if seconds < chatterbox::MIN_REF_SECONDS {
        return Err(format!(
            "that clip is {seconds:.1}s; a voice needs at least {:.0}s of speech",
            chatterbox::MIN_REF_SECONDS
        ));
    }
    if seconds > chatterbox::MAX_REF_SECONDS {
        return Err(format!(
            "that clip is {seconds:.0}s; a voice must be under {:.0}s",
            chatterbox::MAX_REF_SECONDS
        ));
    }
    let peak = samples.iter().fold(0f32, |a, b| a.max(b.abs()));
    if !peak.is_finite() || peak < 1e-4 {
        return Err("that clip is silent, so there is nothing to clone".to_string());
    }

    let dir =
        engine_paths::chatterbox_voices_dir().ok_or("cannot locate the app support directory")?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir {dir:?}: {e}"))?;

    let stem = source
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    let file = unique_name(&dir, &slug(&stem));
    let path = dir.join(&file);
    // Re-encoded rather than copied: the stored file must be the normalised buffer, and a
    // 44.1 kHz stereo original would otherwise be read as 24 kHz mono by everything
    // downstream that only looks at the sample count.
    write_mono_24k(&path, &samples)?;
    Ok(VoiceClip { file, seconds })
}

/// Delete a stored clip. Refuses anything that is not a plain file name.
pub fn remove(file: &str) -> Result<(), String> {
    if file == builtin_file() {
        return Err("the built-in voice cannot be deleted".to_string());
    }
    let path = clip_path(file).ok_or_else(|| format!("{file:?} is not a stored voice"))?;
    if !path.is_file() {
        return Err(format!("there is no voice called {file:?}"));
    }
    std::fs::remove_file(&path).map_err(|e| format!("remove {path:?}: {e}"))
}

/// `My Voice 2.wav` → `my_voice_2`. Anything that is not a letter, digit, dash or
/// underscore is dropped, because this becomes a file name that a config then stores.
pub fn slug(stem: &str) -> String {
    let mut out = String::with_capacity(stem.len());
    let mut last_dash = false;
    for character in stem.chars() {
        if character.is_ascii_alphanumeric() {
            out.push(character.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('_');
            last_dash = true;
        }
    }
    let trimmed = out.trim_matches('_').to_string();
    let capped: String = trimmed.chars().take(48).collect();
    if capped.is_empty() {
        "voice".to_string()
    } else {
        capped
    }
}

/// `voice.wav`, `voice_2.wav`, `voice_3.wav`… Adding the same file twice makes a second
/// voice rather than silently replacing the first.
fn unique_name(dir: &Path, slug: &str) -> String {
    let candidate = format!("{slug}.wav");
    if !dir.join(&candidate).exists() {
        return candidate;
    }
    for index in 2..1000 {
        let candidate = format!("{slug}_{index}.wav");
        if !dir.join(&candidate).exists() {
            return candidate;
        }
    }
    format!("{slug}_{}.wav", std::process::id())
}

/// 16-bit mono PCM at 24 kHz. Bit depth is where this differs from what the graphs read
/// (`f32`): the file is the durable copy, and 16-bit is what every other tool on the
/// machine can open. `read_reference_mono_24k` converts it back on the way in.
fn write_mono_24k(path: &Path, samples: &[f32]) -> Result<(), String> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: chatterbox::SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer =
        hound::WavWriter::create(path, spec).map_err(|e| format!("create {path:?}: {e}"))?;
    for sample in samples {
        writer
            .write_sample((sample.clamp(-1.0, 1.0) * 32767.0) as i16)
            .map_err(|e| format!("write {path:?}: {e}"))?;
    }
    writer
        .finalize()
        .map_err(|e| format!("finish {path:?}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Names become file names and then config values, so the sanitiser has to be total.
    #[test]
    fn a_slug_is_always_a_safe_file_stem() {
        assert_eq!(slug("My Voice 2"), "my_voice_2");
        assert_eq!(slug("../../etc/passwd"), "etc_passwd");
        assert_eq!(slug("  "), "voice");
        assert_eq!(slug(""), "voice");
        assert_eq!(slug("!!!"), "voice");
        assert_eq!(slug("a/b\\c"), "a_b_c");
        // Long names are capped, and the cap cannot leave a trailing separator.
        assert_eq!(slug(&"x".repeat(80)).len(), 48);
        assert!(!slug("ab---").ends_with('_'));
    }

    /// A stored voice id is read from a config file, so anything that could escape the
    /// voices directory must not resolve to a path at all.
    #[test]
    fn a_voice_id_that_is_a_path_is_refused() {
        assert!(clip_path("../../../etc/passwd").is_none());
        assert!(clip_path("nested/voice.wav").is_none());
        assert!(clip_path("..").is_none());
        assert!(clip_path(".hidden").is_none());
        assert!(clip_path("").is_none());
        // A plain name is fine, including one with spaces.
        assert!(clip_path("my voice.wav").is_some());
    }

    /// The default is the shipped clip, and it is reachable from both spellings a config
    /// can hold: absent, and the default's own file name.
    #[test]
    fn no_choice_and_the_builtin_name_both_resolve_to_the_shipped_clip() {
        let dir = Path::new("/models/chatterbox");
        assert_eq!(
            reference_path(dir, None),
            dir.join(engine_paths::CHATTERBOX_DEFAULT_VOICE_FILE)
        );
        assert_eq!(
            reference_path(dir, Some(builtin_file())),
            dir.join(engine_paths::CHATTERBOX_DEFAULT_VOICE_FILE)
        );
        // A user clip resolves under the voices directory, not beside the graph.
        let chosen = reference_path(dir, Some("grandma.wav"));
        assert!(chosen.ends_with("voices/grandma.wav"), "{chosen:?}");
    }

    /// The length window is a refusal, not a clamp: a 0.2 s clip has no speaker in it.
    #[test]
    fn a_clip_outside_the_window_is_refused_by_length() {
        let dir = std::env::temp_dir().join("kiegen-voices-window");
        std::fs::create_dir_all(&dir).unwrap();
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: chatterbox::SAMPLE_RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };

        let short = dir.join("short.wav");
        let mut writer = hound::WavWriter::create(&short, spec).unwrap();
        for _ in 0..(chatterbox::SAMPLE_RATE as usize / 10) {
            writer.write_sample(9000i16).unwrap();
        }
        writer.finalize().unwrap();
        let error = add(&short).expect_err("0.1 s is not a voice");
        assert!(error.contains("at least"), "got: {error}");

        // Silence of the right length is refused for a different, equally specific reason.
        let silent = dir.join("silent.wav");
        let mut writer = hound::WavWriter::create(&silent, spec).unwrap();
        for _ in 0..(chatterbox::SAMPLE_RATE as usize * 3) {
            writer.write_sample(0i16).unwrap();
        }
        writer.finalize().unwrap();
        let error = add(&silent).expect_err("silence clones nothing");
        assert!(error.contains("silent"), "got: {error}");

        // And a file that is not a WAV at all names itself rather than panicking.
        let junk = dir.join("junk.wav");
        std::fs::write(&junk, b"not a wav").unwrap();
        let error = add(&junk).expect_err("junk cannot be a voice");
        assert!(error.contains("junk.wav"), "got: {error}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The built-in clip is part of the install, not something the user may delete.
    #[test]
    fn the_builtin_voice_cannot_be_deleted() {
        let error = remove(builtin_file()).expect_err("must refuse");
        assert!(error.contains("built-in"), "got: {error}");
        assert!(remove("not-a-voice.wav").is_err());
    }
}
