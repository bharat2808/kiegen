//! Persistent settings. One JSON file in the app config dir; no database, no plugin.
//!
//! v0 keeps this deliberately dumb: load at startup, save on change, re-register
//! shortcuts on every save (idempotent by construction — see `shortcuts::apply`).

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tauri::{AppHandle, Manager};

/// How to obtain the selection. AX is non-destructive; ⌘C is universal but
/// briefly owns the clipboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CaptureMode {
    /// Accessibility first, synthetic ⌘C + pasteboard only if AX yields nothing.
    #[default]
    AxThenCopy,
    /// Accessibility only. Never touches the clipboard.
    AxOnly,
    /// Synthetic ⌘C only. For apps whose AX tree is useless (Chrome, Electron).
    CopyOnly,
}

/// Accelerators in `global_hotkey` syntax: `Cmd+Shift+S`, `Ctrl+Alt+1`, `F8`…
/// `CommandOrControl` / `CmdOrCtrl` are accepted too and map per platform
/// (SUPER on macOS, CONTROL elsewhere) — good for a config meant to travel.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Shortcuts {
    pub speak: String,
    pub stop: String,
}

impl Default for Shortcuts {
    fn default() -> Self {
        Self {
            speak: "Cmd+Shift+S".to_string(),
            stop: "Cmd+Shift+X".to_string(),
        }
    }
}

/// Which synthesis backend to use.
///
/// Apple is the default deliberately: it is the only engine that is already installed on
/// every Mac and speaks instantly. Kokoro needs a one-time ~325 MB model fetch and Qwen
/// needs ~2 GB plus a Python runtime, so neither can be what a first-run user hits — the
/// app has to be useful before any download happens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Engine {
    /// `/usr/bin/say` — the system voices, including Apple's neural (Premium/Enhanced)
    /// tiers. Zero download, zero dependencies, subprocess only.
    #[default]
    Apple,
    /// Local Kokoro-82M over ONNX. Opt-in: `MIT OR Apache-2.0` and no espeak, but the
    /// weights are a separate download and never ship inside the bundle.
    Kokoro,
    /// Local Qwen3-TTS 0.6B via MLX. Opt-in and heavy (~2 GB weights plus a Python
    /// sidecar); measured at 0.23 s to first audio and RTF 0.7–0.9 on an M4.
    Qwen,
    /// Local Chatterbox (Resemble AI) via MLX. MIT, and — unlike Kokoro's non-English
    /// voices — its text front end needs no espeak, so it can cover languages this build
    /// otherwise cannot. Costs ~3 GB including its speech tokenizer.
    Chatterbox,
}

/// Kokoro engine settings. Defaults follow the benchmark in docs/DESIGN.md §5: fp32 was
/// neither slower nor bigger in practice than fp16, and quantisation bought only memory
/// while costing real time.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct KokoroSettings {
    /// Kokoro voice id, e.g. `af_heart`. 54 exist; `af_*`/`am_*` are English.
    pub voice: String,
    /// `fp32` | `fp16` | `q8f16` | `quantized`.
    pub quant: String,
    /// Playback rate multiplier passed to the model.
    pub speed: f32,
    /// Keep the session resident between utterances. Unloading costs ~0.4 s to reload.
    pub keep_warm: bool,
    /// Unload the session after this many idle minutes when `keep_warm`.
    pub idle_unload_minutes: u32,
}

impl Default for KokoroSettings {
    fn default() -> Self {
        Self {
            voice: "af_heart".to_string(),
            quant: "fp32".to_string(),
            speed: 1.0,
            keep_warm: true,
            idle_unload_minutes: 30,
        }
    }
}

/// Qwen3-TTS settings. `streaming_interval` is the one number that matters: the library
/// default is 2.0 seconds of audio per yield, which puts time-to-first-audio at ~1.5 s,
/// while 0.32 reaches first audio in ~0.23 s and still sustains RTF 0.64.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct QwenSettings {
    /// Preset speaker name, e.g. `vivian`. `get_supported_speakers()` lists nine.
    pub voice: String,
    /// Seconds of audio decoded per yielded chunk. Lower = faster start, slower overall.
    pub streaming_interval: f32,
    /// Keep the sidecar process (and the 1.1 GB of weights) resident.
    pub keep_warm: bool,
    /// Interpreter for the sidecar. `None` = detect from the app's managed venv.
    pub python: Option<String>,
}

impl Default for QwenSettings {
    fn default() -> Self {
        Self {
            voice: "vivian".to_string(),
            streaming_interval: 0.32,
            keep_warm: false,
            python: None,
        }
    }
}

/// Chatterbox settings. Its signature control is `exaggeration`: emotion intensity, the
/// one knob that changes *how* a line is read rather than merely how fast. Defaults are
/// the values in the model's own `generate` signature, not guesses.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ChatterboxSettings {
    /// Chatterbox is a zero-shot voice-cloning model, so there is one built-in voice and
    /// custom ones come from `ref_audio` rather than a speaker list.
    pub voice: String,
    /// Emotion intensity, 0.0–1.0. Library default is 0.1 (flat, neutral).
    pub exaggeration: f32,
    /// Classifier-free guidance weight. Library default 0.5.
    pub cfg_weight: f32,
    /// Reference clip to clone. Not implemented yet — cloning needs an audio loader and a
    /// way for the user to pick a file, which this build does not have.
    pub ref_audio: Option<String>,
    /// Keep the sidecar process (and the weights) resident.
    pub keep_warm: bool,
    /// Interpreter for the sidecar. `None` = detect from the app's managed venv.
    pub python: Option<String>,
}

impl Default for ChatterboxSettings {
    fn default() -> Self {
        Self {
            voice: "default".to_string(),
            exaggeration: 0.1,
            cfg_weight: 0.5,
            ref_audio: None,
            keep_warm: false,
            python: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub shortcuts: Shortcuts,
    /// Which engine speaks. Apple unless the user opts into a local model.
    pub engine: Engine,
    pub kokoro: KokoroSettings,
    pub qwen: QwenSettings,
    pub chatterbox: ChatterboxSettings,
    /// macOS voice name (`say -v ?`). `None` = system default voice. Apple engine only.
    pub voice: Option<String>,
    /// Words per minute, passed to `say -r`. Apple engine only.
    pub rate: u32,
    pub capture_mode: CaptureMode,
    /// Refuse selections longer than this (protects against selecting a whole document).
    pub max_chars: usize,
    /// Restore the previous clipboard contents after a copy-mode capture.
    pub restore_clipboard: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            shortcuts: Shortcuts::default(),
            engine: Engine::default(),
            kokoro: KokoroSettings::default(),
            qwen: QwenSettings::default(),
            chatterbox: ChatterboxSettings::default(),
            voice: None,
            rate: 200,
            capture_mode: CaptureMode::default(),
            max_chars: 5000,
            restore_clipboard: true,
        }
    }
}

pub fn settings_path(app: &AppHandle) -> PathBuf {
    app.path()
        .app_config_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("settings.json")
}

/// Missing or corrupt file ⇒ defaults. A broken config must never stop the app
/// from launching, because the tray is the only way back to it.
pub fn load(app: &AppHandle) -> Settings {
    let path = settings_path(app);
    match std::fs::read_to_string(&path) {
        Ok(raw) => serde_json::from_str(&raw).unwrap_or_else(|e| {
            eprintln!("[kiegen] settings at {path:?} are invalid ({e}); using defaults");
            Settings::default()
        }),
        Err(_) => Settings::default(),
    }
}

pub fn save(app: &AppHandle, settings: &Settings) -> Result<(), String> {
    let path = settings_path(app);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("mkdir {dir:?}: {e}"))?;
    }
    let json = serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| format!("write {path:?}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The default must stay Apple. If this ever flips, a fresh install starts by asking
    /// the user for a 325 MB (or 2 GB) download before it can say a word.
    #[test]
    fn a_fresh_config_speaks_with_apple() {
        let settings = Settings::default();
        assert_eq!(settings.engine, Engine::Apple);
        assert_eq!(serde_json::to_value(&settings).unwrap()["engine"], "apple");
    }

    /// Config files written before the engine choice existed must keep working: they have
    /// no `engine` key, and the tray app has no way to recover from a config that fails
    /// to load.
    #[test]
    fn a_config_from_before_engines_still_loads() {
        let legacy = r#"{
            "shortcuts": { "speak": "Cmd+Shift+S", "stop": "Cmd+Shift+X" },
            "voice": "Samantha",
            "rate": 190,
            "capture_mode": "ax_then_copy",
            "max_chars": 4000,
            "restore_clipboard": true
        }"#;
        let settings: Settings = serde_json::from_str(legacy).expect("legacy config must parse");
        assert_eq!(settings.engine, Engine::Apple);
        assert_eq!(settings.voice.as_deref(), Some("Samantha"));
        assert_eq!(settings.rate, 190);
        assert_eq!(settings.max_chars, 4000);
        // Engine sections the file never mentioned arrive at their defaults.
        assert_eq!(settings.kokoro.voice, "af_heart");
        assert_eq!(settings.qwen.streaming_interval, 0.32);
        assert_eq!(settings.chatterbox.exaggeration, 0.1);
        assert!(settings.chatterbox.ref_audio.is_none());
    }

    /// The same rule for an engine added last: naming one section must not disturb another.
    #[test]
    fn a_chatterbox_section_does_not_disturb_qwen() {
        let json = r#"{ "engine": "chatterbox", "chatterbox": { "exaggeration": 0.7 } }"#;
        let settings: Settings = serde_json::from_str(json).unwrap();
        assert_eq!(settings.engine, Engine::Chatterbox);
        assert_eq!(settings.chatterbox.exaggeration, 0.7);
        // Untouched neighbours keep their measured-good values.
        assert_eq!(settings.chatterbox.cfg_weight, 0.5);
        assert_eq!(settings.qwen.streaming_interval, 0.32);
        assert_eq!(settings.kokoro.voice, "af_heart");
    }

    #[test]
    fn engine_choice_round_trips_through_json() {
        for (engine, wire) in [
            (Engine::Apple, "\"apple\""),
            (Engine::Kokoro, "\"kokoro\""),
            (Engine::Qwen, "\"qwen\""),
            (Engine::Chatterbox, "\"chatterbox\""),
        ] {
            let settings = Settings {
                engine,
                ..Settings::default()
            };
            let json = serde_json::to_string(&settings).unwrap();
            assert!(json.contains(wire), "expected {wire} in {json}");
            let back: Settings = serde_json::from_str(&json).unwrap();
            assert_eq!(back.engine, engine);
        }
    }

    /// A partially-written engine section must not wipe the fields it omits.
    #[test]
    fn a_partial_engine_section_keeps_its_other_defaults() {
        let json = r#"{ "engine": "qwen", "qwen": { "voice": "ryan" } }"#;
        let settings: Settings = serde_json::from_str(json).unwrap();
        assert_eq!(settings.engine, Engine::Qwen);
        assert_eq!(settings.qwen.voice, "ryan");
        // The measured-good latency setting survives even though the file omitted it.
        assert_eq!(settings.qwen.streaming_interval, 0.32);
    }
}
