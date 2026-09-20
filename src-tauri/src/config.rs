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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub shortcuts: Shortcuts,
    /// macOS voice name (`say -v ?`). `None` = system default voice.
    pub voice: Option<String>,
    /// Words per minute, passed to `say -r`.
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
