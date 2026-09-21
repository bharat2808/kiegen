//! The engine catalogue: every synthesis backend, its voices, and — stated plainly —
//! whether it can actually speak yet.
//!
//! This is the only place that knows what a "Kokoro voice" or a "Qwen speaker" is, so the
//! settings UI can render all three engines from data instead of hard-coding lists in
//! TypeScript. The `can_speak` flag is deliberately honest rather than aspirational: an
//! engine whose front end does not exist is selectable and its choice persists, but the UI
//! is told it cannot speak, so it can say why instead of failing at the shortcut.

use serde::Serialize;

use crate::config::{Engine, Settings};

/// Weights are fetched from HuggingFace, never bundled — see docs/DESIGN.md §4.
pub const KOKORO_REPO: &str = "onnx-community/Kokoro-82M-v1.0-ONNX";
pub const QWEN_REPO: &str = "mlx-community/Qwen3-TTS-12Hz-0.6B-CustomVoice-8bit";
pub const QWEN_WEIGHTS_BYTES: u64 = 2_070_000_000; // measured: 1974 MB, 8-bit

/// Chatterbox needs its speech tokenizer as a separate repo, so the install is two repos.
/// Upstream (`ResembleAI/chatterbox`) is MIT and — unlike Kokoro — carries no phonemiser at
/// all: its text path is a Llama BPE tokenizer, so there is no espeak in it to avoid.
pub const CHATTERBOX_REPO: &str = "mlx-community/chatterbox-fp16";
pub const CHATTERBOX_TOKENIZER_REPO: &str = "mlx-community/S3TokenizerV2";
pub const CHATTERBOX_WEIGHTS_BYTES: u64 = 2_577_000_000 + 495_000_000;

/// Bytes Kokoro needs on disk, derived from the download plan so the figure the UI shows is
/// the figure actually fetched (the 325.5 MB graph, the tokenizer, and 28 voice tables).
fn kokoro_weights_bytes() -> u64 {
    crate::download::kokoro_bytes()
}

/// One selectable voice, whatever the engine calls it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct EngineVoice {
    /// What gets written to the config: a `say` name, a Kokoro id, a Qwen speaker.
    pub id: String,
    /// Display name with the engine's own prefixes stripped.
    pub label: String,
    pub language: String,
    /// `Some(why)` = shown, but not selectable, and the UI says why.
    pub unavailable: Option<String>,
    /// Voice metadata the engine happens to know (Kokoro encodes gender in the id).
    pub note: Option<String>,
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
/// front end. The synthesis path branches on this, and it is the single place that decision
/// is made — a voice is espeak-backed here or it is not, everywhere.
pub fn espeak_language_for(voice: &str) -> Option<&'static str> {
    family_for(voice)?.espeak
}

// ───────────────────────────────── Qwen ─────────────────────────────────

/// Qwen3-TTS CustomVoice preset speakers, read from the model's own `talker_config.spk_id`
/// in the cached 8-bit checkpoint — not from documentation.
const QWEN_SPEAKERS: &[(&str, &str)] = &[
    ("serena", "Female"),
    ("vivian", "Female"),
    ("sohee", "Female"),
    ("ono_anna", "Female"),
    ("aiden", "Male"),
    ("dylan", "Male"),
    ("eric", "Male"),
    ("ryan", "Male"),
    ("uncle_fu", "Male"),
];

/// `ono_anna` → `Ono Anna`, `uncle_fu` → `Uncle Fu`. The id stays visible in the row's
/// subtitle, so the label is free to be a name.
fn qwen_label(id: &str) -> String {
    id.split('_')
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn qwen_voices() -> Vec<EngineVoice> {
    QWEN_SPEAKERS
        .iter()
        .map(|(id, gender)| EngineVoice {
            id: (*id).to_string(),
            label: qwen_label(id),
            language: "Multilingual".to_string(),
            unavailable: None,
            note: Some((*gender).to_string()),
        })
        .collect()
}

// ─────────────────────────────── Chatterbox ──────────────────────────────

/// Chatterbox is a **zero-shot voice-cloning** model: it has one built-in voice and custom
/// voices come from a reference clip, not from a speaker table. Offering a nine-item list
/// here would be inventing voices the checkpoint does not have.
pub fn chatterbox_voices() -> Vec<EngineVoice> {
    vec![EngineVoice {
        id: "default".to_string(),
        label: "Built-in voice".to_string(),
        language: "English".to_string(),
        unavailable: None,
        note: Some("Cloning not implemented yet".to_string()),
    }]
}

// ─────────────────────────────── catalogue ──────────────────────────────

/// `engine_ready` is injected rather than probed here so this module stays testable
/// without a 2 GB model on disk.
pub fn catalog(settings: &Settings) -> Vec<EngineInfo> {
    let kokoro = kokoro_voices(crate::engine_paths::espeak_ng().is_some());
    let qwen = qwen_voices();
    let chatterbox = chatterbox_voices();

    let kokoro_weights = crate::engine_paths::kokoro_installed();
    let sidecar = crate::engine_paths::sidecar_python();
    // A runtime without weights still needs the download, so both are required.
    let qwen_set_up = sidecar.is_some() && crate::engine_paths::mlx_model_installed(QWEN_REPO);
    // Chatterbox needs two repos: the model itself and its speech tokenizer.
    let chatterbox_set_up = sidecar.is_some()
        && crate::engine_paths::mlx_model_installed(CHATTERBOX_REPO)
        && crate::engine_paths::mlx_model_installed(CHATTERBOX_TOKENIZER_REPO);

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
            selected_voice: settings.kokoro.voice.clone(),
        },
        EngineInfo {
            id: Engine::Qwen,
            label: "Qwen3-TTS 0.6B",
            summary: "Local 0.6B model, 9 speakers",
            can_speak: false,
            status: "Sidecar missing".to_string(),
            blocked_reason: Some(
                "the Python sidecar is not implemented yet, so it cannot speak".to_string(),
            ),
            needs_download: !qwen_set_up,
            download_bytes: if qwen_set_up { 0 } else { QWEN_WEIGHTS_BYTES },
            repo: QWEN_REPO,
            voices: qwen,
            selected_voice: settings.qwen.voice.clone(),
        },
        EngineInfo {
            id: Engine::Chatterbox,
            label: "Chatterbox",
            summary: "Local 0.5B model, voice cloning",
            can_speak: false,
            status: "Sidecar missing".to_string(),
            blocked_reason: Some(
                "the Python sidecar is not implemented yet, so it cannot speak".to_string(),
            ),
            needs_download: !chatterbox_set_up,
            download_bytes: if chatterbox_set_up {
                0
            } else {
                CHATTERBOX_WEIGHTS_BYTES
            },
            repo: CHATTERBOX_REPO,
            voices: chatterbox,
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

    #[test]
    fn qwen_labels_read_as_names() {
        assert_eq!(qwen_label("ono_anna"), "Ono Anna");
        assert_eq!(qwen_label("uncle_fu"), "Uncle Fu");
        assert_eq!(qwen_label("vivian"), "Vivian");
    }

    /// Qwen's nine speakers come from the checkpoint's own `talker_config`, so the list
    /// must match it exactly.
    #[test]
    fn qwen_offers_the_nine_checkpoint_speakers() {
        let voices = qwen_voices();
        assert_eq!(voices.len(), 9);
        for id in [
            "serena", "vivian", "uncle_fu", "ryan", "aiden", "ono_anna", "sohee", "eric", "dylan",
        ] {
            assert!(voices.iter().any(|v| v.id == id), "{id} missing");
        }
        assert!(voices.iter().all(|v| v.unavailable.is_none()));
    }

    /// Apple is ready with nothing installed, and every engine that cannot speak must say why.
    /// Kokoro's readiness is not a constant any more — it depends on whether its files are on
    /// disk, which is the whole point of `can_speak`, so it is asserted in both directions.
    /// Four engines: Apple, Kokoro, Qwen, Chatterbox.
    #[test]
    fn an_engine_that_cannot_speak_always_says_why() {
        let catalog = catalog(&Settings::default());
        assert_eq!(catalog.len(), 4);
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
        for engine in catalog(&Settings::default()) {
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
            }
        }
    }

    #[test]
    fn every_engine_carries_its_own_voices() {
        let settings = Settings::default();
        let catalog = catalog(&settings);
        let kokoro = catalog.iter().find(|e| e.id == Engine::Kokoro).unwrap();
        let qwen = catalog.iter().find(|e| e.id == Engine::Qwen).unwrap();
        let apple = catalog.iter().find(|e| e.id == Engine::Apple).unwrap();
        assert_eq!(kokoro.voices.len(), 54);
        assert_eq!(qwen.voices.len(), 9);
        // Apple's 184 voices travel in their own `UiState` field, not here.
        assert!(apple.voices.is_empty());
    }

    /// Chatterbox clones from a reference clip instead of carrying a speaker table, so it
    /// must be offered as exactly one voice. A longer list here would be fabricated.
    #[test]
    fn chatterbox_offers_one_voice_not_an_invented_speaker_list() {
        let voices = chatterbox_voices();
        assert_eq!(voices.len(), 1);
        assert_eq!(voices[0].id, "default");
        assert!(voices[0].unavailable.is_none());
        // And the catalogue carries it through, so the pane has something to render.
        let catalog = catalog(&Settings::default());
        let entry = catalog.iter().find(|e| e.id == Engine::Chatterbox).unwrap();
        assert_eq!(entry.voices.len(), 1);
        assert_eq!(entry.selected_voice, "default");
    }
}
