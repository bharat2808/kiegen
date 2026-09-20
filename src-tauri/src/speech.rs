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
    pub name: String,
    /// e.g. `en_US`, or empty for novelty voices.
    pub locale: String,
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
    // xx_YY (or xx-YY) — e.g. en_US, pt_BR, zh_CN
    let mut parts = token.split(['_', '-']);
    let (Some(lang), Some(region), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    lang.len() == 2
        && region.len() == 2
        && lang.chars().all(|c| c.is_ascii_lowercase())
        && region.chars().all(|c| c.is_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_locale_tokens() {
        assert!(looks_like_locale("en_US"));
        assert!(looks_like_locale("pt-BR"));
        assert!(!looks_like_locale("en"));
        assert!(!looks_like_locale("Hello!"));
    }

    #[test]
    fn voices_have_names_and_locales() {
        for voice in list_voices() {
            assert!(!voice.name.is_empty());
        }
    }
}
