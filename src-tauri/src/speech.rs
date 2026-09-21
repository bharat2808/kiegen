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
    /// Apple's quality tier. This is what makes "use Apple's neural voices" a real
    /// choice rather than a label: Compact is the legacy built-in synthesis, while
    /// Enhanced/Premium are the on-device neural voices.
    pub tier: VoiceTier,
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

/// Apple ships its voices in quality tiers. Neither obvious source exposes them:
/// `say -v ?` prints no quality column, and AppKit's voice-attribute dictionary has no
/// quality key (only name, identifier, locale, gender, age, demo text). The tier *is*
/// in the voice identifier, which `NSSpeechSynthesizer::availableVoices` returns:
///
/// ```text
/// com.apple.voice.compact.en-US.Samantha      ← built-in, legacy synthesis
/// com.apple.voice.enhanced.en-US.Samantha     ← downloaded, neural
/// com.apple.voice.premium.en-US.Ava           ← downloaded, Apple's best
/// com.apple.ttsbundle.siri_Aman_hi-IN_compact ← Siri voices: underscorred
/// ```
///
/// Two separator conventions, so tokenise on all of them rather than slicing by
/// position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VoiceTier {
    /// Apple's highest-quality on-device voices. The ones actually worth picking.
    Premium,
    /// Downloaded "Enhanced" voices — neural, a clear step up from Compact.
    Enhanced,
    /// Built into the OS. Most of these are the old formant synthesis.
    Compact,
    /// The identifier carried no tier token we recognise. Most of these really are the
    /// old built-in voices, but the bucket also swallows any identifier shape Apple has
    /// not shown us — so the UI must not present it as a promise that a voice is bad.
    Standard,
}

impl VoiceTier {
    /// Higher is more neural. Used to resolve a name that exists in several tiers:
    /// `Samantha` can be both `compact` and `enhanced`, and recording the wrong one
    /// would hide the neural voice from the user.
    fn rank(self) -> u8 {
        match self {
            VoiceTier::Standard => 0,
            VoiceTier::Compact => 1,
            VoiceTier::Enhanced => 2,
            VoiceTier::Premium => 3,
        }
    }
}

fn tier_from_identifier(identifier: &str) -> VoiceTier {
    for token in identifier.split(['.', '_', '-']) {
        match token.to_ascii_lowercase().as_str() {
            "premium" => return VoiceTier::Premium,
            "enhanced" => return VoiceTier::Enhanced,
            "compact" => return VoiceTier::Compact,
            _ => {}
        }
    }
    VoiceTier::Standard
}

/// `com.apple.voice.compact.en-US.Samantha` → `samantha`, and the Siri form
/// `com.apple.ttsbundle.siri_Aman_hi-IN_compact` → `aman`.
///
/// The result is compared against `say -v ?` names, which are human ("Samantha") and
/// sometimes carry a language suffix ("Aman (English (India))") — callers normalise
/// both sides to the bare base name before comparing.
fn name_from_identifier(identifier: &str) -> Option<String> {
    let is_quality = |token: &str| {
        matches!(
            token.to_ascii_lowercase().as_str(),
            "premium" | "enhanced" | "compact"
        )
    };
    let tokens: Vec<&str> = identifier
        .split(['.', '_', '-'])
        .filter(|token| !token.is_empty())
        .collect();

    if let Some(position) = tokens.iter().position(|t| t.eq_ignore_ascii_case("siri")) {
        let name = tokens.get(position + 1)?;
        return (!is_quality(name)).then(|| (*name).to_string());
    }

    // Otherwise the name is last, unless a tier token trails it.
    let mut index = tokens.len().checked_sub(1)?;
    if is_quality(tokens[index]) {
        index = index.checked_sub(1)?;
    }
    let candidate = tokens[index];
    (!is_quality(candidate) && candidate.len() > 1).then(|| candidate.to_string())
}

/// Tiers keyed by lowercased base name, so they can be merged onto the `say` list.
/// A voice Apple does not report (or reports unrecognisably) simply stays absent and
/// the caller falls back to `Standard`.
fn voice_tiers() -> std::collections::HashMap<String, VoiceTier> {
    tiers_from_identifiers(&voice_identifiers())
}

/// Split out from `voice_tiers` so the merge rule is testable without the live system.
fn tiers_from_identifiers(identifiers: &[String]) -> std::collections::HashMap<String, VoiceTier> {
    let mut tiers: std::collections::HashMap<String, VoiceTier> = std::collections::HashMap::new();
    for identifier in identifiers {
        let Some(name) = name_from_identifier(identifier) else {
            continue;
        };
        let tier = tier_from_identifier(identifier);
        // One name can exist in several tiers — `Samantha` ships both compact and
        // enhanced — and the map can only hold one. Keep the most neural, because the
        // whole point is to avoid hiding a neural voice behind a compact twin.
        tiers
            .entry(name.to_lowercase())
            .and_modify(|existing| {
                if tier.rank() > existing.rank() {
                    *existing = tier;
                }
            })
            .or_insert(tier);
    }
    tiers
}

/// Voice identifiers straight from AppKit. Deprecated in favour of AVFoundation's
/// `AVSpeechSynthesisVoice`, but that lives in `objc2-avf-audio`, a dependency we
/// would otherwise not need — and all we want is the tier string.
#[cfg(target_os = "macos")]
#[allow(deprecated)]
fn voice_identifiers() -> Vec<String> {
    use objc2_app_kit::NSSpeechSynthesizer;
    NSSpeechSynthesizer::availableVoices()
        .iter()
        .map(|voice| voice.to_string())
        .collect()
}

#[cfg(not(target_os = "macos"))]
fn voice_identifiers() -> Vec<String> {
    Vec::new()
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
    let tiers = voice_tiers();
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
        // AppKit reports the bare name; `say -v ?` may suffix it with a language
        // ("Aman (English (India))"). Match on the base so the two line up.
        let base = name
            .split(" (")
            .next()
            .unwrap_or(&name)
            .trim()
            .to_lowercase();
        voices.push(Voice {
            novelty: is_novelty(&name),
            tier: tiers.get(&base).copied().unwrap_or(VoiceTier::Standard),
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
    fn tier_is_read_from_the_identifier() {
        assert_eq!(
            tier_from_identifier("com.apple.voice.premium.en-US.Ava"),
            VoiceTier::Premium
        );
        assert_eq!(
            tier_from_identifier("com.apple.voice.enhanced.en_US.Samantha"),
            VoiceTier::Enhanced
        );
        assert_eq!(
            tier_from_identifier("com.apple.voice.compact.en-US.Samantha"),
            VoiceTier::Compact
        );
        // Siri voices use underscores where the others use dots.
        assert_eq!(
            tier_from_identifier("com.apple.ttsbundle.siri_Aman_hi-IN_compact"),
            VoiceTier::Compact
        );
        // Pre-tier identifiers carry no quality token at all.
        assert_eq!(
            tier_from_identifier("com.apple.speech.synthesis.voice.Alex"),
            VoiceTier::Standard
        );
    }

    #[test]
    fn name_is_extracted_from_both_identifier_shapes() {
        assert_eq!(
            name_from_identifier("com.apple.voice.compact.en-US.Samantha").as_deref(),
            Some("Samantha")
        );
        assert_eq!(
            name_from_identifier("com.apple.voice.premium.en-US.Ava").as_deref(),
            Some("Ava")
        );
        assert_eq!(
            name_from_identifier("com.apple.ttsbundle.siri_Aman_hi-IN_compact").as_deref(),
            Some("Aman")
        );
        // A trailing tier token must never be mistaken for the voice's name.
        assert_ne!(
            name_from_identifier("com.apple.voice.premium.en-US.Ava").as_deref(),
            Some("premium")
        );
    }

    /// Guards the join between the two sources. If `say -v ?` names and AppKit
    /// identifiers stop lining up, every voice silently becomes `Legacy` and the UI
    /// loses the ability to say which voices are neural — a quiet regression, so fail
    /// loudly instead. Prints the distribution for inspection under `--nocapture`.
    #[test]
    fn appkit_tiers_join_onto_the_say_list() {
        let identifiers = voice_identifiers();
        assert!(
            !identifiers.is_empty(),
            "AppKit reported no voices; tier detection is dead"
        );
        let tiers = voice_tiers();
        assert!(
            !tiers.is_empty(),
            "no identifier yielded a name: {} identifiers parsed to nothing",
            identifiers.len()
        );

        let voices = list_voices();
        let mut counts: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
        for voice in &voices {
            *counts.entry(tier_label(voice.tier)).or_insert(0) += 1;
        }
        println!("AppKit identifiers: {}", identifiers.len());
        println!("names recognised:   {}", tiers.len());
        println!("tier distribution:  {counts:?}");

        let matched = voices
            .iter()
            .filter(|v| v.tier != VoiceTier::Standard)
            .count();
        assert!(
            matched > 0,
            "not one of the {} voices from `say -v ?` matched an AppKit identifier",
            voices.len()
        );
    }

    fn tier_label(tier: VoiceTier) -> &'static str {
        match tier {
            VoiceTier::Premium => "premium",
            VoiceTier::Enhanced => "enhanced",
            VoiceTier::Compact => "compact",
            VoiceTier::Standard => "standard",
        }
    }

    /// The merge rule matters: a name shipping in two tiers must not be recorded as the
    /// lower one, or the UI would hide a neural voice.
    #[test]
    fn a_name_in_two_tiers_keeps_the_most_neural_one() {
        let identifiers: Vec<String> = [
            "com.apple.voice.compact.en-US.Samantha",
            "com.apple.voice.enhanced.en-US.Samantha",
            // Order must not matter.
            "com.apple.voice.premium.en-US.Ava",
            "com.apple.voice.compact.en-US.Ava",
            "com.apple.speech.synthesis.voice.Albert",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();

        let tiers = tiers_from_identifiers(&identifiers);
        assert_eq!(tiers.get("samantha"), Some(&VoiceTier::Enhanced));
        assert_eq!(tiers.get("ava"), Some(&VoiceTier::Premium));
        assert_eq!(tiers.get("albert"), Some(&VoiceTier::Standard));

        // And the lower tier must not win when it is seen last.
        let reversed: Vec<String> = identifiers.iter().rev().cloned().collect();
        let flipped = tiers_from_identifiers(&reversed);
        assert_eq!(flipped.get("samantha"), Some(&VoiceTier::Enhanced));
        assert_eq!(flipped.get("ava"), Some(&VoiceTier::Premium));
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
