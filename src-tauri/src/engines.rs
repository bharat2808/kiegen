//! The engine catalogue: every synthesis backend, its voices, and — stated plainly —
//! whether it can actually speak yet.
//!
//! This is the only place that knows what a "Kokoro voice" or a "Chatterbox language" is,
//! so the settings UI can render every engine from data instead of hard-coding lists in
//! TypeScript. The `can_speak` flag is deliberately honest rather than aspirational: an
//! engine whose front end does not exist is selectable and its choice persists, but the UI
//! is told it cannot speak, so it can say why instead of failing at the shortcut.

use serde::Serialize;

use crate::config::{Engine, Settings};

/// Weights are fetched from HuggingFace, never bundled — see docs/DESIGN.md §4.
pub const KOKORO_REPO: &str = "onnx-community/Kokoro-82M-v1.0-ONNX";

/// Chatterbox **Multilingual**, ONNX, MIT and ungated. One repository, where the
/// English-only MLX pairing of `chatterbox-fp16` + `S3TokenizerV2` used to be two — the
/// tokenizer ships in this one. Upstream (`ResembleAI/chatterbox`) is MIT and — unlike
/// Kokoro — carries no phonemiser at all: its text path is a Llama BPE tokenizer, so there
/// is no espeak in it to avoid, and no Python to run it.
pub const CHATTERBOX_REPO: &str = "onnx-community/chatterbox-multilingual-ONNX";

/// Bytes Kokoro needs on disk, derived from the download plan so the figure the UI shows is
/// the figure actually fetched (the 325.5 MB graph, the tokenizer, and 28 voice tables).
fn kokoro_weights_bytes() -> u64 {
    crate::download::kokoro_bytes()
}

/// Chatterbox's total, derived the same way. It used to be a hand-written constant summing
/// two MLX repositories, which is exactly the kind of figure that drifts once the plan
/// changes — as it has.
fn chatterbox_weights_bytes() -> u64 {
    crate::download::chatterbox_bytes()
}

/// One selectable voice, whatever the engine calls it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct EngineVoice {
    /// What gets written to the config: a `say` name, a Kokoro voice id, a Chatterbox
    /// language code.
    pub id: String,
    /// Display name with the engine's own prefixes stripped.
    pub label: String,
    pub language: String,
    /// `Some(why)` = shown, but not selectable, and the UI says why.
    pub unavailable: Option<String>,
    /// Voice metadata the engine happens to know (Kokoro encodes gender in the id).
    pub note: Option<String>,
}

/// One reference clip a user can speak in, for an engine that clones rather than selects.
///
/// Separate from `EngineVoice` on purpose. For Chatterbox, `EngineVoice.id` is a *language*
/// and `RefVoice.id` is a *file*: they are chosen independently, they persist in different
/// fields, and collapsing them into one list would make a language look like a speaker.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct RefVoice {
    /// What `chatterbox.ref_audio` stores. The built-in clip's own file name.
    pub id: String,
    /// Shown in the row. A user's file name, capped to fit one line.
    pub label: String,
    /// Short metadata — how long the clip is.
    pub note: String,
    /// The shipped clip: shown, selectable, and not deletable.
    pub builtin: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct EngineInfo {
    pub id: Engine,
    pub label: &'static str,
    /// One short line for the picker row. Not a paragraph: this is UI chrome.
    pub summary: &'static str,
    /// True only when this engine could speak *today*, right now. Drives the badge.
    pub can_speak: bool,
    /// A few words for the status line, e.g. `Front end missing`.
    pub status: String,
    /// Why the engine cannot speak, in a full sentence. **Never rendered as UI chrome** —
    /// it is only used to explain a failed shortcut, where the user asked for speech and
    /// is owed a reason.
    pub blocked_reason: Option<String>,
    pub needs_download: bool,
    pub download_bytes: u64,
    pub repo: &'static str,
    pub voices: Vec<EngineVoice>,
    /// Cloning engines only; empty for the rest. Chatterbox's own voice list.
    pub ref_voices: Vec<RefVoice>,
    /// The id currently chosen for this engine, so the UI can mark the row.
    pub selected_voice: String,
}

// ──────────────────────────────── Kokoro ────────────────────────────────

/// Kokoro v1.0's voice ids, as published in the ONNX export.
///
/// The repo also carries `voices/af.bin`, which is **excluded deliberately**: it is
/// 524,288 bytes (512 style rows) where every real voice is 522,240 (510 rows, see
/// `kokoro::MAX_TOKENS`), it is absent from Kokoro's documented voice list, and 512 rows
/// would push the token limit past the model's own `MAX_PHONEME_LENGTH`. It is a stray,
/// not a voice.
const KOKORO_VOICE_IDS: &[&str] = &[
    // lang_code `a`
    "af_alloy",
    "af_aoede",
    "af_bella",
    "af_heart",
    "af_jessica",
    "af_kore",
    "af_nicole",
    "af_nova",
    "af_river",
    "af_sarah",
    "af_sky",
    "am_adam",
    "am_echo",
    "am_eric",
    "am_fenrir",
    "am_liam",
    "am_michael",
    "am_onyx",
    "am_puck",
    "am_santa",
    // lang_code `b`
    "bf_alice",
    "bf_emma",
    "bf_isabella",
    "bf_lily",
    "bm_daniel",
    "bm_fable",
    "bm_george",
    "bm_lewis",
    // lang_code `e` — espeak-ng
    "ef_dora",
    "em_alex",
    "em_santa",
    // lang_code `f` — espeak-ng
    "ff_siwis",
    // lang_code `h` — espeak-ng
    "hf_alpha",
    "hf_beta",
    "hm_omega",
    "hm_psi",
    // lang_code `i` — espeak-ng
    "if_sara",
    "im_nicola",
    // lang_code `j`
    "jf_alpha",
    "jf_gongitsune",
    "jf_nezumi",
    "jf_tebukuro",
    "jm_kumo",
    // lang_code `p` — espeak-ng
    "pf_dora",
    "pm_alex",
    "pm_santa",
    // lang_code `z`
    "zf_xiaobei",
    "zf_xiaoni",
    "zf_xiaoxiao",
    "zf_xiaoyi",
    "zm_yunjian",
    "zm_yunxi",
    "zm_yunxia",
    "zm_yunyang",
];

/// Kokoro's voice families. The letter is the model's `lang_code`; the mapping and the
/// "which G2P does this use" column come from `hexgrad/kokoro`'s own `LANG_CODES` table,
/// where `e`, `f`, `h`, `i`, `p` are commented `# espeak-ng`.
///
/// That column is the whole reason this table exists: espeak-ng is GPL-3.0 and cannot
/// enter this repo, so every voice whose front end is espeak carries the espeak voice name
/// it needs and is offered as present-but-unavailable until the user installs espeak-ng
/// themselves. The reason string is kept short on purpose — it is rendered in a list row,
/// not a dialog.
struct Family {
    letter: &'static str,
    language: &'static str,
    /// The espeak-ng voice this family's front end must call, when the front end *is*
    /// espeak. It doubles as the flag for "this family is reachable iff espeak-ng is
    /// installed", which is why there is no separate boolean. The strings are Kokoro's own
    /// `LANG_CODES` values, passed straight through to espeak.
    espeak: Option<&'static str>,
    /// Set when no front end exists at all, so no user action could enable the voice.
    blocked: Option<&'static str>,
}

const KOKORO_FAMILIES: &[Family] = &[
    Family {
        letter: "a",
        language: "American English",
        espeak: None,
        blocked: None,
    },
    Family {
        letter: "b",
        language: "British English",
        espeak: None,
        blocked: None,
    },
    Family {
        letter: "e",
        language: "Spanish",
        espeak: Some("es"),
        blocked: None,
    },
    Family {
        letter: "f",
        language: "French",
        espeak: Some("fr-fr"),
        blocked: None,
    },
    Family {
        letter: "h",
        language: "Hindi",
        espeak: Some("hi"),
        blocked: None,
    },
    Family {
        letter: "i",
        language: "Italian",
        espeak: Some("it"),
        blocked: None,
    },
    Family {
        letter: "p",
        language: "Portuguese (Brazil)",
        espeak: Some("pt-br"),
        blocked: None,
    },
    Family {
        letter: "j",
        language: "Japanese",
        espeak: None,
        blocked: Some("Needs a Japanese front end"),
    },
    Family {
        letter: "z",
        language: "Mandarin Chinese",
        espeak: None,
        blocked: Some("Needs a Chinese front end"),
    },
];

fn family_for(id: &str) -> Option<&'static Family> {
    let letter = id.split('_').next()?.chars().next()?;
    KOKORO_FAMILIES
        .iter()
        .find(|family| family.letter.starts_with(letter))
}

/// `af_heart` → `Heart`, `am_onyx` → `Onyx`. The prefix carries only language and gender,
/// which are shown in their own columns, so it is stripped here.
fn kokoro_label(id: &str) -> String {
    match id.split_once('_') {
        Some((_, name)) => {
            let mut chars = name.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => id.to_string(),
            }
        }
        None => id.to_uppercase(),
    }
}

/// Kokoro encodes gender as the second letter of the prefix: `af_`/`bf_` female, `am_`/`bm_`
/// male. Worth surfacing because it is the only thing that distinguishes two voices named
/// almost the same.
fn kokoro_gender(id: &str) -> Option<&'static str> {
    match id.chars().nth(1) {
        Some('f') => Some("Female"),
        Some('m') => Some("Male"),
        _ => None,
    }
}

/// `espeak_ready` says whether an espeak-ng install was found. It is injected rather than
/// probed here so both states are assertable on any machine: the catalogue is what a user
/// reasons about when a voice is greyed out, so "why" has to be testable either way.
pub fn kokoro_voices(espeak_ready: bool) -> Vec<EngineVoice> {
    KOKORO_VOICE_IDS
        .iter()
        .filter_map(|id| {
            let family = family_for(id)?;
            Some(EngineVoice {
                id: (*id).to_string(),
                label: kokoro_label(id),
                language: family.language.to_string(),
                // An espeak-backed family is unusable until the user installs espeak-ng —
                // a condition they can fix, unlike `blocked`, which no install resolves.
                unavailable: match (family.espeak, espeak_ready) {
                    (Some(_), false) => Some("Needs espeak-ng (GPL-3.0)".to_string()),
                    _ => family.blocked.map(str::to_string),
                },
                note: kokoro_gender(id).map(str::to_string),
            })
        })
        .collect()
}

/// The espeak-ng voice a Kokoro voice id needs, or `None` when it uses the built-in English
/// front end. English can optionally use an installed CLI for unknown words, but never
/// requires it for readiness. This mapping describes voices that require espeak-ng.
pub fn espeak_language_for(voice: &str) -> Option<&'static str> {
    family_for(voice)?.espeak
}

// ─────────────────────────────── Chatterbox ──────────────────────────────

/// Chatterbox Multilingual's language table, taken verbatim from Resemble AI's own
/// `SUPPORTED_LANGUAGES` dict in `src/chatterbox/mtl_tts.py` — the codes and names exactly
/// as published, in that order.
///
/// Languages rather than voices, because Chatterbox is a **zero-shot voice-cloning** model:
/// the speaker comes from a reference clip, not from a speaker table. Offering a list of
/// names here would be inventing voices the checkpoint does not have.
pub const CHATTERBOX_LANGUAGES: &[(&str, &str)] = &[
    ("ar", "Arabic"),
    ("da", "Danish"),
    ("de", "German"),
    ("el", "Greek"),
    ("en", "English"),
    ("es", "Spanish"),
    ("fi", "Finnish"),
    ("fr", "French"),
    ("he", "Hebrew"),
    ("hi", "Hindi"),
    ("it", "Italian"),
    ("ja", "Japanese"),
    ("ko", "Korean"),
    ("ms", "Malay"),
    ("nl", "Dutch"),
    ("no", "Norwegian"),
    ("pl", "Polish"),
    ("pt", "Portuguese"),
    ("ru", "Russian"),
    ("sv", "Swedish"),
    ("sw", "Swahili"),
    ("tr", "Turkish"),
    ("zh", "Chinese"),
];

/// `en` → `English`. The synthesis path reports the language it was handed in words, and the
/// settings pane shows a name for a stored code, so both need this lookup rather than a
/// second copy of the table.
pub fn chatterbox_language(code: &str) -> Option<&'static str> {
    CHATTERBOX_LANGUAGES
        .iter()
        .find(|(candidate, _)| *candidate == code)
        .map(|(_, name)| *name)
}

/// A language this build cannot read *correctly*, with the reason.
///
/// The reference normalises four languages before tokenizing: `zh` through a Cangjie
/// conversion, `ja` through a kanji-to-hiragana reading, `he` through a diacritiser, and
/// `ko` by decomposing syllables. Two of those are Python packages with trained models
/// (`pykakasi`, `dicta_onnx`) and are not ported, so those two languages are **refused**
/// rather than fed raw text the checkpoint was never trained to read. A model that accepts
/// the text and produces plausible nonsense is worse than one that says it cannot.
pub fn chatterbox_language_blocked(code: &str) -> Option<&'static str> {
    match code {
        "ja" => Some("Needs a Japanese reading front end"),
        "he" => Some("Needs a Hebrew diacritiser"),
        _ => None,
    }
}

/// One selectable entry per language: the id is the code the runtime is handed, and the
/// label is that language's own name, so a row reads as a language and a choice stores a
/// code with no mapping in the UI.
pub fn chatterbox_voices() -> Vec<EngineVoice> {
    CHATTERBOX_LANGUAGES
        .iter()
        .map(|(code, name)| EngineVoice {
            id: (*code).to_string(),
            label: (*name).to_string(),
            language: (*name).to_string(),
            unavailable: chatterbox_language_blocked(code).map(str::to_string),
            // `zh` works, but through the reference's own no-segmenter path. Saying so is the
            // difference between a degradation the user can see and one they cannot.
            note: (*code == "zh").then(|| "No word segmentation".to_string()),
        })
        .collect()
}

/// The reference-clip list: the shipped clip first, then whatever the user has added.
///
/// Injected like the espeak flag rather than probed, so both states are assertable without
/// a models directory on the machine running the test.
pub fn chatterbox_ref_voices(clips: &[crate::voices::VoiceClip]) -> Vec<RefVoice> {
    let mut voices = vec![RefVoice {
        id: crate::voices::builtin_file().to_string(),
        label: "Built-in voice".to_string(),
        note: "Ships with the model".to_string(),
        builtin: true,
    }];
    voices.extend(clips.iter().map(|clip| RefVoice {
        // The label is the file's own name without the extension — the user named it, so
        // this is the only honest label — capped so it cannot break a one-line row.
        id: clip.file.clone(),
        label: label_for(&clip.file),
        note: format!("{:.1}s", clip.seconds),
        builtin: false,
    }));
    voices
}

/// `grandma_2.wav` → `grandma 2`, capped at 32 characters so the row stays one line.
fn label_for(file: &str) -> String {
    let stem = file.strip_suffix(".wav").unwrap_or(file);
    let pretty = stem.replace('_', " ");
    if pretty.chars().count() <= 32 {
        return pretty;
    }
    let mut out: String = pretty.chars().take(30).collect();
    out.push('…');
    out
}

// ─────────────────────────────── catalogue ──────────────────────────────

/// `engine_ready` is injected rather than probed here so this module stays testable
/// without a gigabyte of weights on disk.
pub fn catalog(settings: &Settings) -> Vec<EngineInfo> {
    catalog_with(settings, crate::voices::list())
}

/// Same catalogue, with the user's clips injected.
pub fn catalog_with(settings: &Settings, clips: Vec<crate::voices::VoiceClip>) -> Vec<EngineInfo> {
    let kokoro = kokoro_voices(crate::engine_paths::espeak_ng().is_some());
    let chatterbox = chatterbox_voices();
    let ref_voices = chatterbox_ref_voices(&clips);

    let kokoro_weights = crate::engine_paths::kokoro_installed();
    // Chatterbox's files are the app's own now, so this is a directory check like Kokoro's
    // rather than a look into a cache layout somebody else owns. Whether it can *speak* is a
    // separate question with a separate answer.
    let chatterbox_weights = crate::engine_paths::chatterbox_installed();

    vec![
        EngineInfo {
            id: Engine::Apple,
            label: "Apple system voices",
            summary: "Already installed on this Mac",
            can_speak: true,
            status: "Ready".to_string(),
            blocked_reason: None,
            needs_download: false,
            download_bytes: 0,
            repo: "",
            voices: Vec::new(), // the Apple list is its own field; see `UiState::voices`
            ref_voices: Vec::new(),
            selected_voice: settings.voice.clone().unwrap_or_default(),
        },
        EngineInfo {
            id: Engine::Kokoro,
            label: "Kokoro 82M",
            summary: "Local 82M model, 28 English voices",
            // The front end exists and is measured against the reference (g2p.rs), so this
            // is no longer about missing code — it is about missing files. Once the graph,
            // the tokenizer, the dictionaries and a voice table are on disk, this engine can
            // genuinely speak, and saying otherwise would make the shortcut refuse a
            // selection it is perfectly able to read.
            can_speak: kokoro_weights,
            status: if kokoro_weights {
                "Ready".to_string()
            } else {
                "Weights missing".to_string()
            },
            blocked_reason: if kokoro_weights {
                None
            } else {
                Some(
                    "Kokoro's weights are not installed yet. Use Download in its engine card, \
                     then try the shortcut again."
                        .to_string(),
                )
            },
            needs_download: !kokoro_weights,
            download_bytes: if kokoro_weights {
                0
            } else {
                kokoro_weights_bytes()
            },
            repo: KOKORO_REPO,
            voices: kokoro,
            ref_voices: Vec::new(),
            selected_voice: settings.kokoro.voice.clone(),
        },
        EngineInfo {
            id: Engine::Chatterbox,
            label: "Chatterbox Multilingual",
            summary: "Local 0.5B model, 23 languages",
            // The front end is this module's own `chatterbox.rs`: it can genuinely speak once
            // the four graphs, the tokenizer and a reference clip are on disk, so readiness is
            // a file question like Kokoro's rather than a "not written yet" constant.
            can_speak: chatterbox_weights,
            status: if chatterbox_weights {
                "Ready".to_string()
            } else {
                "Weights missing".to_string()
            },
            blocked_reason: if chatterbox_weights {
                None
            } else {
                Some(
                    "Chatterbox's weights are not installed yet. Use Download in its engine \
                     card, then try the shortcut again."
                        .to_string(),
                )
            },
            needs_download: !chatterbox_weights,
            download_bytes: if chatterbox_weights {
                0
            } else {
                chatterbox_weights_bytes()
            },
            repo: CHATTERBOX_REPO,
            voices: chatterbox,
            ref_voices,
            selected_voice: settings.chatterbox.voice.clone(),
        },
    ]
}

/// The voice list for whichever engine is active is carried inside each `EngineInfo`, so
/// the UI never has to ask for it separately.
#[cfg(test)]
mod tests {
    use super::*;

    /// Kokoro v1.0 ships 54 voices. If this changes, a real voice was added or one was
    /// dropped, and the count in the UI copy is wrong.
    #[test]
    fn kokoro_has_the_documented_54_voices() {
        assert_eq!(kokoro_voices(false).len(), 54);
    }

    /// The stray `voices/af.bin` has 512 style rows where real voices have 510, and is not
    /// in Kokoro's documented set. It must never reach the picker.
    #[test]
    fn the_512_row_stray_is_not_offered_as_a_voice() {
        assert!(!KOKORO_VOICE_IDS.contains(&"af"));
        assert!(kokoro_voices(false).iter().all(|voice| voice.id != "af"));
    }

    /// espeak-ng is GPL-3.0. Every voice whose front end is espeak must come back
    /// unavailable with a reason — never silently listed as usable.
    #[test]
    fn espeak_backed_voices_are_offered_but_marked_unusable_without_espeak() {
        let voices = kokoro_voices(false);
        for id in ["ef_dora", "ff_siwis", "hf_alpha", "if_sara", "pf_dora"] {
            let voice = voices.iter().find(|v| v.id == id).expect("voice listed");
            let why = voice.unavailable.as_deref().unwrap_or("usable");
            assert!(why.contains("espeak-ng"), "{id} said: {why}");
            assert!(why.contains("GPL"), "{id} must name the licence problem");
        }
    }

    /// The other half of the same rule: once espeak-ng *is* present, those voices have to
    /// become usable, and the ones with no front end at all must stay unusable. A catalogue
    /// that only ever greys things out would hide a working install.
    #[test]
    fn installing_espeak_ng_makes_exactly_the_espeak_voices_usable() {
        let voices = kokoro_voices(true);
        for id in ["ef_dora", "ff_siwis", "hf_alpha", "if_sara", "pf_dora"] {
            let voice = voices.iter().find(|v| v.id == id).expect("voice listed");
            assert!(
                voice.unavailable.is_none(),
                "{id} should be usable once espeak-ng is present, said: {:?}",
                voice.unavailable
            );
        }
        // Japanese and Mandarin have no front end at all, so a working espeak-ng must not
        // pretend to fix them.
        for id in ["jf_alpha", "zf_xiaobei"] {
            let voice = voices.iter().find(|v| v.id == id).expect("voice listed");
            assert!(
                voice.unavailable.is_some(),
                "{id} has no front end and must stay unavailable"
            );
        }
    }

    /// Every espeak-backed voice must name the espeak voice it needs, because that string is
    /// what the synthesis path hands to the subprocess.
    #[test]
    fn espeak_backed_voices_carry_their_espeak_voice() {
        for (id, expected) in [
            ("ef_dora", "es"),
            ("ff_siwis", "fr-fr"),
            ("hf_alpha", "hi"),
            ("if_sara", "it"),
            ("pf_dora", "pt-br"),
        ] {
            assert_eq!(espeak_language_for(id), Some(expected), "{id}");
        }
        // English uses the front end in this repo, and Japanese/Mandarin have none.
        assert_eq!(espeak_language_for("af_heart"), None);
        assert_eq!(espeak_language_for("jf_alpha"), None);
    }

    /// The English voices are the ones this project can actually make work, so they must not
    /// be accidentally excluded. 20 American + 8 British = 28; an earlier count of 29
    /// included the 512-row stray.
    #[test]
    fn the_28_english_voices_are_all_usable() {
        let voices = kokoro_voices(false);
        let english: Vec<_> = voices
            .iter()
            .filter(|v| v.language.ends_with("English"))
            .collect();
        assert_eq!(english.len(), 28);
        assert_eq!(
            english
                .iter()
                .filter(|v| v.language == "American English")
                .count(),
            20
        );
        assert_eq!(
            english
                .iter()
                .filter(|v| v.language == "British English")
                .count(),
            8
        );
        assert!(english.iter().all(|v| v.unavailable.is_none()));
    }

    #[test]
    fn every_voice_belongs_to_a_known_language_family() {
        for voice in kokoro_voices(false) {
            let family = family_for(&voice.id).expect("family");
            assert_eq!(voice.language, family.language, "for {}", voice.id);
        }
        // No voice may be silently dropped by the family lookup.
        assert_eq!(kokoro_voices(false).len(), KOKORO_VOICE_IDS.len());
    }

    #[test]
    fn kokoro_labels_strip_the_language_prefix() {
        assert_eq!(kokoro_label("af_heart"), "Heart");
        assert_eq!(kokoro_label("am_onyx"), "Onyx");
        assert_eq!(kokoro_label("zm_yunxia"), "Yunxia");
    }

    /// Apple is ready with nothing installed, and every engine that cannot speak must say why.
    /// Kokoro's readiness is not a constant any more — it depends on whether its files are on
    /// disk, which is the whole point of `can_speak`, so it is asserted in both directions.
    /// Three engines: Apple, Kokoro, Chatterbox.
    #[test]
    fn an_engine_that_cannot_speak_always_says_why() {
        let catalog = catalog(&Settings::default());
        assert_eq!(catalog.len(), 3);
        assert!(catalog[0].can_speak, "apple must be ready");
        assert!(catalog[0].blocked_reason.is_none());

        for engine in &catalog[1..] {
            assert_eq!(
                engine.can_speak,
                engine.blocked_reason.is_none(),
                "{:?} claims it can speak and also that it cannot",
                engine.id
            );
            if !engine.can_speak {
                assert!(
                    engine
                        .blocked_reason
                        .as_ref()
                        .is_some_and(|reason| reason.len() > 20),
                    "{:?} must be able to explain itself when a shortcut fails, in words",
                    engine.id
                );
            }
        }
    }

    /// Kokoro's badge flips with the files on disk. It used to be hard-coded to "cannot speak"
    /// because the front end did not exist; leaving that in place would now make the shortcut
    /// refuse a selection the engine can read perfectly well.
    #[test]
    fn kokoro_readiness_follows_its_files() {
        let kokoro = catalog(&Settings::default())
            .into_iter()
            .find(|engine| engine.id == Engine::Kokoro)
            .expect("kokoro is in the catalogue");
        assert_eq!(
            kokoro.can_speak,
            crate::engine_paths::kokoro_installed(),
            "the catalogue and the filesystem disagree about Kokoro"
        );
        assert_eq!(kokoro.needs_download, !kokoro.can_speak);
    }

    /// The pane is chrome, not documentation: every string the UI renders has to fit in a
    /// row on one line. Prose belongs in `blocked_reason`, which the UI never shows.
    /// This test exists because the first draft of this screen was a wall of explanation.
    #[test]
    fn every_ui_string_is_one_short_line() {
        // A user's own clip is the one string in this catalogue that is not written here, so
        // it is exercised with an over-long file name rather than left to the built-in clip.
        let clips = vec![crate::voices::VoiceClip {
            file: format!("{}.wav", "a-very-long-voice-name-".repeat(4)),
            seconds: 9.5,
        }];
        for engine in catalog_with(&Settings::default(), clips) {
            assert!(
                engine.summary.len() <= 42,
                "{:?} summary too long for a row: {}",
                engine.id,
                engine.summary
            );
            assert!(
                engine.status.len() <= 24,
                "{:?} status too long for a status line: {}",
                engine.id,
                engine.status
            );
            for voice in &engine.voices {
                assert!(
                    voice.label.len() <= 32 && voice.language.len() <= 32,
                    "{:?} voice text too long: {} / {}",
                    engine.id,
                    voice.label,
                    voice.language
                );
                if let Some(why) = &voice.unavailable {
                    assert!(
                        why.len() <= 40,
                        "{:?} voice reason too long for a row: {why}",
                        engine.id
                    );
                }
                if let Some(note) = &voice.note {
                    assert!(
                        note.len() <= 32,
                        "{:?} voice note too long for a row: {note}",
                        engine.id
                    );
                }
            }
            for voice in &engine.ref_voices {
                assert!(
                    voice.label.chars().count() <= 32,
                    "{:?} clip label too long for a row: {}",
                    engine.id,
                    voice.label
                );
                assert!(
                    voice.note.len() <= 24,
                    "{:?} clip note too long for a row: {}",
                    engine.id,
                    voice.note
                );
            }
        }
    }

    #[test]
    fn every_engine_carries_its_own_voices() {
        let settings = Settings::default();
        let catalog = catalog(&settings);
        let kokoro = catalog.iter().find(|e| e.id == Engine::Kokoro).unwrap();
        let chatterbox = catalog.iter().find(|e| e.id == Engine::Chatterbox).unwrap();
        let apple = catalog.iter().find(|e| e.id == Engine::Apple).unwrap();
        assert_eq!(kokoro.voices.len(), 54);
        assert_eq!(chatterbox.voices.len(), 23);
        // Apple's 184 voices travel in their own `UiState` field, not here.
        assert!(apple.voices.is_empty());
    }

    /// Chatterbox clones from a reference clip instead of carrying a speaker table, so it
    /// must be offered as one entry per *language* — 23 of them, exactly the codes and names
    /// Resemble publishes. A speaker list here would be fabricated.
    ///
    /// Two of the 23 are offered but marked unusable: the reference normalises `ja` and `he`
    /// through trained Python models this build does not have, and handing the model raw text
    /// it was never trained to read produces confident nonsense. `zh` is usable but carries a
    /// note, because it runs without the word segmenter the reference uses.
    #[test]
    fn chatterbox_offers_one_entry_per_language_not_an_invented_speaker_list() {
        let voices = chatterbox_voices();
        assert_eq!(voices.len(), 23);
        for (voice, (code, name)) in voices.iter().zip(CHATTERBOX_LANGUAGES) {
            assert_eq!(voice.id, *code, "the id is the code the runtime is handed");
            assert_eq!(voice.label, *name);
            assert_eq!(voice.language, *name);
            assert_eq!(
                voice.unavailable.is_some(),
                matches!(*code, "ja" | "he"),
                "{code} is marked unusable for the wrong reason"
            );
            assert_eq!(
                voice.note.is_some(),
                *code == "zh",
                "{code} carries a note it should not"
            );
        }
        // And the catalogue carries it through, so the pane has something to render.
        let catalog = catalog(&Settings::default());
        let entry = catalog.iter().find(|e| e.id == Engine::Chatterbox).unwrap();
        assert_eq!(entry.voices.len(), 23);
        assert_eq!(entry.selected_voice, "en");
    }

    /// The two languages with no normaliser must refuse for a reason the user can read, and
    /// every other language must be usable — a gate, not a blanket denial.
    #[test]
    fn only_the_languages_without_a_normaliser_are_marked_unusable() {
        let unusable: Vec<&str> = chatterbox_voices()
            .into_iter()
            .filter(|voice| voice.unavailable.is_some())
            .map(|voice| {
                // Leaked deliberately: this is a test, and the ids are static by construction.
                Box::leak(voice.id.into_boxed_str()) as &str
            })
            .collect();
        assert_eq!(unusable, vec!["he", "ja"]);
        for (code, _) in CHATTERBOX_LANGUAGES {
            if matches!(*code, "ja" | "he") {
                continue;
            }
            assert!(
                chatterbox_language_blocked(code).is_none(),
                "{code} must not be blocked"
            );
        }
        // And the runtime agrees with the catalogue rather than keeping its own list.
        for (code, name) in [("ja", "Japanese"), ("he", "Hebrew")] {
            let error = crate::chatterbox::prepare_text("test", code, None).expect_err("refuses");
            assert!(error.contains(name), "got: {error}");
        }
    }

    /// The reference-clip list is the shipped clip plus the user's own, and it is what the
    /// pane renders. A voice the user added must arrive with its length, and the built-in
    /// one must be marked as not deletable.
    #[test]
    fn the_clip_list_is_the_builtin_plus_what_the_user_added() {
        let clips = vec![
            crate::voices::VoiceClip {
                file: "grandma.wav".to_string(),
                seconds: 7.25,
            },
            crate::voices::VoiceClip {
                file: format!("{}.wav", "x".repeat(60)),
                seconds: 12.0,
            },
        ];
        let voices = chatterbox_ref_voices(&clips);
        assert_eq!(voices.len(), 3);
        assert!(voices[0].builtin);
        assert_eq!(voices[0].id, crate::voices::builtin_file());
        assert_eq!(voices[1].label, "grandma");
        assert_eq!(voices[1].note, "7.2s");
        assert!(!voices[1].builtin);
        // A file name longer than a row is truncated rather than allowed to wrap.
        assert!(voices[2].label.chars().count() <= 32);

        // The catalogue carries them, and only Chatterbox has any.
        let catalog = catalog_with(&Settings::default(), clips);
        let chatterbox = catalog.iter().find(|e| e.id == Engine::Chatterbox).unwrap();
        assert_eq!(chatterbox.ref_voices.len(), 3);
        assert_eq!(
            chatterbox
                .ref_voices
                .iter()
                .filter(|voice| voice.builtin)
                .count(),
            1
        );
        for engine in catalog.iter().filter(|e| e.id != Engine::Chatterbox) {
            assert!(
                engine.ref_voices.is_empty(),
                "{:?} must not offer reference clips",
                engine.id
            );
        }
    }

    /// The codes matter more than the count: a config stores one and the runtime will be
    /// handed one. Pinned against Resemble's own `SUPPORTED_LANGUAGES`, in its order.
    #[test]
    fn chatterbox_language_codes_are_the_published_set() {
        let codes: Vec<&str> = CHATTERBOX_LANGUAGES.iter().map(|(code, _)| *code).collect();
        assert_eq!(
            codes,
            vec![
                "ar", "da", "de", "el", "en", "es", "fi", "fr", "he", "hi", "it", "ja", "ko", "ms",
                "nl", "no", "pl", "pt", "ru", "sv", "sw", "tr", "zh"
            ]
        );
        assert_eq!(chatterbox_language("en"), Some("English"));
        assert_eq!(chatterbox_language("zh"), Some("Chinese"));
        // A code the checkpoint does not support must not resolve to a name: the row would
        // then look like a language the engine could be handed, and nothing would notice.
        assert_eq!(chatterbox_language("xx"), None);
    }
}
