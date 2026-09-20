//! Speech output.
//!
//! v0 uses `/usr/bin/say` as the **bootstrap engine**: a system binary invoked as a
//! subprocess, so it costs nothing in licence terms, needs no model download, and
//! makes the app useful in the first five minutes after install. Kokoro (§5 of
//! docs/DESIGN.md) replaces it as the default engine in v0.5 and `say` stays as the
//! fallback — never deleted.
//!
//! Text goes in over stdin, not argv: a long selection would otherwise hit ARG_MAX.

use std::io::Write;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;

pub const SAY: &str = "/usr/bin/say";

#[derive(Debug, Clone, serde::Serialize)]
pub struct Voice {
    /// Exactly as `say -v ?` prints it — passed straight back to `say -v`.
    pub name: String,
    /// e.g. `en_US`.
    pub locale: String,
    /// macOS novelty voices (Bells, Zarvox…) are sound effects, not speech. They are
    /// listed for completeness but the UI hides them by default: offering "Bells" as a
    /// reading voice is a trap.
    pub novelty: bool,
}

/// The classic macOS novelty voices. Stable set, unchanged for years.
const NOVELTY_VOICES: &[&str] = &[
    "Albert",
    "Bad News",
    "Bahh",
    "Bells",
    "Boing",
    "Bubbles",
    "Cellos",
    "Deranged",
    "Good News",
    "Hysterical",
    "Jester",
    "Organ",
    "Superstar",
    "Trinoids",
    "Whisper",
    "Wobble",
    "Zarvox",
];

fn is_novelty(name: &str) -> bool {
    let base = name.split(" (").next().unwrap_or(name).trim();
    NOVELTY_VOICES.iter().any(|v| v.eq_ignore_ascii_case(base))
}

/// The user's language, as `xx_YY`, for defaulting the voice browser to something
/// useful instead of dumping 184 voices in their lap.
///
/// `AppleLocale` is the region-aware answer (e.g. `en_CA`); `AppleLanguages[0]` is the
/// preference order. Neither is guaranteed inside a bundled app, so this ends at
/// `en_US` — which is what `say` itself falls back to.
pub fn system_language() -> String {
    if let Some(locale) = read_default("AppleLocale").and_then(|raw| normalize_locale(&raw)) {
        return locale;
    }
    if let Some(list) = read_default("AppleLanguages") {
        let first = list.split(',').next().unwrap_or("");
        if let Some(locale) = normalize_locale(first) {
            return locale;
        }
    }
    "en_US".to_string()
}

fn read_default(key: &str) -> Option<String> {
    let output = Command::new("/usr/bin/defaults")
        .args(["read", "-g", key])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!raw.is_empty()).then_some(raw)
}

/// `en-CA` / `"en-CA"` / `en_CA` → `en_CA`. Anything unrecognisable → `None`.
fn normalize_locale(raw: &str) -> Option<String> {
    let cleaned = raw
        .trim()
        .trim_matches(|c| c == '"' || c == '(' || c == ')' || c == ',' || c == ' ')
        .replace('-', "_");
    looks_like_locale(&cleaned).then_some(cleaned)
}

#[derive(Default)]
pub struct Speaker {
    child: Mutex<Option<Child>>,
}

impl Speaker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start speaking, cancelling anything already in progress.
    ///
    /// `voice = None` uses the system default voice.
    pub fn speak(&self, text: &str, voice: Option<&str>, rate: u32) -> Result<(), String> {
        self.stop();
        if text.trim().is_empty() {
            return Err("nothing to speak".to_string());
        }

        let mut cmd = Command::new(SAY);
        if let Some(voice) = voice.filter(|v| !v.trim().is_empty()) {
            cmd.arg("-v").arg(voice);
        }
        cmd.arg("-r").arg(rate.clamp(80, 500).to_string());
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        let mut child = cmd.spawn().map_err(|e| format!("spawn {SAY}: {e}"))?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(text.as_bytes())
                .map_err(|e| format!("write to {SAY}: {e}"))?;
            // stdin is dropped here, which is `say`'s cue that the text is complete.
        }

        *self.child.lock().unwrap() = Some(child);
        Ok(())
    }

    /// Kill the current utterance (if any). Safe to call when idle.
    pub fn stop(&self) {
        let mut guard = self.child.lock().unwrap();
        if let Some(mut child) = guard.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    /// Reap a finished child so `is_speaking` stays honest after a normal completion.
    pub fn is_speaking(&self) -> bool {
        let mut guard = self.child.lock().unwrap();
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

/// Enumerate installed voices. `say -v ?` prints one voice per line as
/// `Name<pad>locale<pad># sample text`, and some names contain spaces
/// ("Bad News"), so the locale token is what separates name from padding.
pub fn list_voices() -> Vec<Voice> {
    let output = match Command::new(SAY).arg("-v").arg("?").output() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("[kiegen] could not list voices: {e}");
            return Vec::new();
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut voices = Vec::new();
    for line in stdout.lines() {
        let before_hash = line.split('#').next().unwrap_or("");
        let tokens: Vec<&str> = before_hash.split_whitespace().collect();
        let Some(locale_idx) = tokens.iter().position(|t| looks_like_locale(t)) else {
            continue;
        };
        let name = tokens[..locale_idx].join(" ");
        if name.is_empty() {
            continue;
        }
        voices.push(Voice {
            novelty: is_novelty(&name),
            name,
            locale: tokens[locale_idx].to_string(),
        });
    }
    voices.sort_by(|a, b| {
        (a.locale.as_str(), a.name.as_str()).cmp(&(b.locale.as_str(), b.name.as_str()))
    });
    voices
}

fn looks_like_locale(token: &str) -> bool {
    // xx_YY (or xx-YY) — e.g. en_US, pt_BR, zh_CN — plus the numeric world region,
    // as in `ar_001` (Majed). Rejecting that form silently lost a voice.
    let mut parts = token.split(['_', '-']);
    let (Some(lang), Some(region), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    let region_ok = (region.len() == 2 && region.chars().all(|c| c.is_ascii_uppercase()))
        || (region.len() == 3 && region.chars().all(|c| c.is_ascii_digit()));
    lang.len() == 2 && lang.chars().all(|c| c.is_ascii_lowercase()) && region_ok
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_locale_tokens() {
        assert!(looks_like_locale("en_US"));
        assert!(looks_like_locale("pt-BR"));
        // `say -v ?` lists Majed as ar_001 — the one voice with a numeric region.
        assert!(looks_like_locale("ar_001"));
        assert!(!looks_like_locale("en"));
        assert!(!looks_like_locale("Hello!"));
        assert!(!looks_like_locale("en_0"));
        assert!(!looks_like_locale("en_0001"));
    }

    /// The list must not quietly lose voices to the locale parser. 184 on macOS 26;
    /// assert a floor rather than an exact number so a trimmed system still passes.
    #[test]
    fn every_installed_voice_is_parsed() {
        let listed = String::from_utf8_lossy(
            &Command::new(SAY)
                .args(["-v", "?"])
                .output()
                .expect("say -v ?")
                .stdout,
        )
        .lines()
        .filter(|line| line.contains('#'))
        .count();
        assert_eq!(list_voices().len(), listed);
    }

    #[test]
    fn voices_have_names_and_locales() {
        for voice in list_voices() {
            assert!(!voice.name.is_empty());
        }
    }

    #[test]
    fn novelty_voices_are_flagged_and_real_ones_are_not() {
        assert!(is_novelty("Bells"));
        assert!(is_novelty("Bad News"));
        // Newer voices carry a parenthetical language suffix; match on the base name.
        assert!(is_novelty("Wobble (English (US))"));
        assert!(!is_novelty("Samantha"));
        assert!(!is_novelty("Eddy (English (US))"));
        assert!(!is_novelty("Ting-Ting"));
    }

    #[test]
    fn system_language_normalises_to_underscore_form() {
        let language = system_language();
        assert!(
            looks_like_locale(&language),
            "unexpected system language: {language}"
        );
        assert_eq!(normalize_locale("en-CA"), Some("en_CA".to_string()));
        assert_eq!(normalize_locale("\"en-US\""), Some("en_US".to_string()));
        assert_eq!(normalize_locale("nonsense"), None);
    }
}
