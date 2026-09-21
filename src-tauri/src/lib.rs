//! kiegen — a menu-bar agent that speaks any selected text.
//!
//! Shape (see docs/DESIGN.md §1): no Dock icon, no window at launch, the tray is the
//! entire persistent UI, and the settings window is created on demand. The real app is
//! the Rust service below; the webview is a config editor.

mod capture;
pub mod config;
pub mod download;
pub mod engine_paths;
pub mod engines;
pub mod g2p;
pub mod kokoro;
pub mod lexicon;
pub mod numbers;
mod shortcuts;
mod speech;

use std::sync::Mutex;

use serde::Serialize;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_global_shortcut::ShortcutState;

use config::Settings;
use shortcuts::Action;
use speech::{Speaker, Voice};

/// Copy-mode capture is bounded: an app that never touches the pasteboard should
/// cost a blink, not a hang.
const COPY_TIMEOUT_MS: u64 = 150;

/// Deep-link to the Accessibility pane in System Settings. Used instead of the AX
/// "prompt" API because it lands the user exactly where the toggle lives.
const ACCESSIBILITY_PANE: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility";

pub struct AppState {
    pub settings: Mutex<Settings>,
    pub speaker: Speaker,
    pub voices: Vec<Voice>,
    pub bindings: Mutex<Vec<shortcuts::Binding>>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Idle,
    Capturing,
    Speaking,
    Error,
}

#[derive(Serialize, Clone)]
struct StatusEvent {
    phase: Phase,
    message: Option<String>,
    chars: Option<usize>,
}

#[derive(Serialize)]
struct UiState {
    settings: Settings,
    voices: Vec<Voice>,
    /// Every engine with its voices and its readiness. The picker renders from this, so
    /// adding an engine never means editing TypeScript.
    engines: Vec<engines::EngineInfo>,
    trusted: bool,
    /// True while a password field has secure input on — a copy-mode capture will
    /// refuse rather than fail mysteriously, and the UI says so up front.
    secure_input: bool,
    speaking: bool,
    refused_shortcuts: Vec<String>,
    /// `xx_YY` from the OS, so the voice browser can open on the user's own language.
    system_language: String,
    /// Shown in Settings so the file is findable without digging through Library.
    config_path: String,
}

fn ui_state(app: &AppHandle, refused_shortcuts: Vec<String>) -> UiState {
    let state = app.state::<AppState>();
    let settings = state.settings.lock().unwrap().clone();
    // Built before the struct literal: the literal moves `settings` into its first field,
    // so borrowing it later in the same expression is a borrow of a moved value.
    let engines = engines::catalog(&settings);
    UiState {
        settings,
        voices: state.voices.clone(),
        engines,
        trusted: capture::is_trusted(),
        secure_input: capture::secure_input_active(),
        speaking: state.speaker.is_speaking(),
        refused_shortcuts,
        system_language: speech::system_language(),
        config_path: config::settings_path(app).display().to_string(),
    }
}

fn emit_status(app: &AppHandle, phase: Phase, message: Option<String>, chars: Option<usize>) {
    let _ = app.emit(
        "kiegen:status",
        StatusEvent {
            phase,
            message,
            chars,
        },
    );
}

/// Why the selected engine cannot speak, or `None` if it can.
///
/// A user who picks Kokoro must never hear Samantha and conclude that is what Kokoro
/// sounds like, so there is no fallback here — the shortcut reports the reason instead.
fn selected_engine_refusal(settings: &Settings) -> Option<String> {
    if settings.engine == config::Engine::Apple {
        return None;
    }
    engines::catalog(settings)
        .into_iter()
        .find(|info| info.id == settings.engine)
        .map(|info| match info.blocked_reason {
            // The user asked for speech and got silence: give them the reason and the way
            // out, rather than the terse status line the settings pane shows.
            Some(reason) => format!(
                "{}: {reason}. Switch to the Apple system voices to read the selection now.",
                info.label
            ),
            None => format!("{} cannot speak right now.", info.label),
        })
}

/// The whole point of the app: capture → speak. Runs off the main thread because the
/// AX read plus a possible ⌘C round-trip blocks for up to `COPY_TIMEOUT_MS`.
fn speak_selection(app: &AppHandle) {
    let (mode, restore, max_chars, voice, rate, refusal) = {
        let state = app.state::<AppState>();
        let settings = state.settings.lock().unwrap();
        (
            settings.capture_mode,
            settings.restore_clipboard,
            settings.max_chars,
            settings.voice.clone(),
            settings.rate,
            selected_engine_refusal(&settings),
        )
    };

    // Check before capturing: there is no point running a ⌘C round-trip for text the
    // engine cannot speak.
    if let Some(reason) = refusal {
        emit_status(app, Phase::Error, Some(reason), None);
        return;
    }

    emit_status(app, Phase::Capturing, None, None);

    let text = match capture::capture(mode, COPY_TIMEOUT_MS, restore) {
        Ok(text) => text,
        Err(error) => {
            eprintln!("[kiegen] capture failed: {error}");
            emit_status(app, Phase::Error, Some(error.to_string()), None);
            return;
        }
    };

    let mut truncated = false;
    let text: String = if text.chars().count() > max_chars {
        truncated = true;
        text.chars().take(max_chars).collect()
    } else {
        text
    };
    let chars = text.chars().count();

    let speaker = &app.state::<AppState>().speaker;
    match speaker.speak(&text, voice.as_deref(), rate) {
        Ok(()) => {
            let message = truncated.then(|| format!("truncated to {max_chars} characters"));
            emit_status(app, Phase::Speaking, message, Some(chars));
        }
        Err(error) => emit_status(app, Phase::Error, Some(error), Some(chars)),
    }
}

/// Speak a caller-supplied string (voice previews), bypassing capture entirely.
fn speak_given(app: &AppHandle, text: String) {
    let (voice, rate, max_chars, refusal) = {
        let state = app.state::<AppState>();
        let settings = state.settings.lock().unwrap();
        (
            settings.voice.clone(),
            settings.rate,
            settings.max_chars,
            selected_engine_refusal(&settings),
        )
    };
    if let Some(reason) = refusal {
        emit_status(app, Phase::Error, Some(reason), None);
        return;
    }
    let text: String = text.chars().take(max_chars).collect();
    let chars = text.chars().count();
    match app
        .state::<AppState>()
        .speaker
        .speak(&text, voice.as_deref(), rate)
    {
        Ok(()) => emit_status(app, Phase::Speaking, None, Some(chars)),
        Err(error) => emit_status(app, Phase::Error, Some(error), Some(chars)),
    }
}

fn show_settings(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

// ─────────────────────────────── commands ───────────────────────────────

#[tauri::command]
fn get_state(app: AppHandle) -> UiState {
    ui_state(&app, Vec::new())
}

/// Persist, then re-apply shortcuts. Registration failures come back to the UI so it
/// can say "macOS refused Cmd+Shift+S" instead of pretending the chord works.
#[tauri::command]
fn save_settings(app: AppHandle, settings: Settings) -> Result<UiState, String> {
    shortcuts::bindings(&settings)?; // validate before writing anything
    config::save(&app, &settings)?;
    *app.state::<AppState>().settings.lock().unwrap() = settings.clone();
    let refused = shortcuts::apply(&app)?;
    Ok(ui_state(&app, refused))
}

/// Speak the current selection right now (the settings window's "try it" button).
#[tauri::command]
fn speak_selection_now(app: AppHandle) {
    std::thread::spawn(move || speak_selection(&app));
}

#[tauri::command]
fn speak_text(app: AppHandle, text: String) {
    std::thread::spawn(move || speak_given(&app, text));
}

/// Audition a voice without committing to it. The voice browser previews rows this
/// way, so clicking through the list never silently rewrites the saved setting.
#[tauri::command]
fn preview_voice(app: AppHandle, voice: Option<String>, rate: u32, text: Option<String>) {
    let sample =
        text.unwrap_or_else(|| "This is how I sound when reading your selection.".to_string());
    std::thread::spawn(move || {
        // Auditioning a voice belongs to the engine that owns it. Clicking play on a
        // Kokoro row while Apple is active must say so, not play an Apple voice and
        // imply the two are the same.
        let refusal = {
            let state = app.state::<AppState>();
            let settings = state.settings.lock().unwrap();
            selected_engine_refusal(&settings)
        };
        if let Some(reason) = refusal {
            emit_status(&app, Phase::Error, Some(reason), None);
            return;
        }
        let speaker = &app.state::<AppState>().speaker;
        match speaker.speak(&sample, voice.as_deref(), rate) {
            Ok(()) => emit_status(&app, Phase::Speaking, Some("preview".to_string()), None),
            Err(error) => emit_status(&app, Phase::Error, Some(error), None),
        }
    });
}

#[tauri::command]
fn stop_speaking(app: AppHandle) {
    app.state::<AppState>().speaker.stop();
    emit_status(&app, Phase::Idle, None, None);
}

// ─────────────────────────── weight downloads ───────────────────────────

/// Progress of a weight download, so the window can show it rather than a frozen button.
/// `phase` is `downloading`, `done` or `error`.
#[derive(Serialize, Clone)]
struct InstallEvent {
    engine: config::Engine,
    phase: &'static str,
    file: String,
    done: u64,
    total: u64,
    message: Option<String>,
}

fn emit_install(
    app: &AppHandle,
    engine: config::Engine,
    phase: &'static str,
    file: &str,
    done: u64,
    total: u64,
    message: Option<String>,
) {
    let _ = app.emit(
        "kiegen:install",
        InstallEvent {
            engine,
            phase,
            file: file.to_string(),
            done,
            total,
            message,
        },
    );
}

fn engine_label(engine: config::Engine) -> String {
    engines::catalog(&Settings::default())
        .into_iter()
        .find(|info| info.id == engine)
        .map(|info| info.label.to_string())
        .unwrap_or_else(|| "this engine".to_string())
}

/// Kokoro's weights are plain files the app fetches and verifies itself. The MLX engines
/// (Qwen, Chatterbox) keep their weights in the HuggingFace cache that their own Python
/// runtime owns, so they have to be installed *by* that runtime — the app must not
/// hand-place files into a cache layout it does not control.
fn install_kokoro(app: &AppHandle) -> Result<(), String> {
    let dir = engine_paths::kokoro_dir().ok_or("could not locate the app support directory")?;

    // Emitting on every read would flood the IPC channel. A megabyte, or a new file, is
    // plenty to keep a progress bar honest.
    let mut emitted: u64 = 0;
    let mut emitted_path = String::new();
    let mut actual: u64 = 0;
    let mut last_total: u64 = 0;

    let mut on_progress = |path: &str, done: u64, total: u64| {
        actual = done;
        last_total = total;
        if done.saturating_sub(emitted) >= 1 << 20 || path != emitted_path {
            emitted = done;
            emitted_path = path.to_string();
            emit_install(
                app,
                config::Engine::Kokoro,
                "downloading",
                path,
                done,
                total,
                None,
            );
        }
    };

    download::install_kokoro_into(&dir, &mut on_progress)?;
    emit_install(
        app,
        config::Engine::Kokoro,
        "done",
        "",
        actual,
        last_total,
        None,
    );
    Ok(())
}

fn install_engine_blocking(app: &AppHandle, engine: config::Engine) {
    match engine {
        // Already on the machine; nothing to fetch.
        config::Engine::Apple => {}
        config::Engine::Kokoro => {
            if let Err(error) = install_kokoro(app) {
                emit_install(app, engine, "error", "", 0, 0, Some(error));
            }
        }
        other => emit_install(
            app,
            other,
            "error",
            "",
            0,
            0,
            Some(format!(
                "{} is installed by its own Python runtime, and that sidecar is not implemented yet",
                engine_label(other)
            )),
        ),
    }
}

#[tauri::command]
fn install_engine(app: AppHandle, engine: config::Engine) {
    std::thread::spawn(move || install_engine_blocking(&app, engine));
}

#[tauri::command]
fn open_settings_window(app: AppHandle) {
    show_settings(&app);
}

#[tauri::command]
fn open_accessibility_settings() -> Result<(), String> {
    std::process::Command::new("/usr/bin/open")
        .arg(ACCESSIBILITY_PANE)
        .status()
        .map(|_| ())
        .map_err(|e| format!("could not open System Settings: {e}"))
}

#[tauri::command]
fn permission_status() -> bool {
    capture::is_trusted()
}

// ──────────────────────────────── setup ────────────────────────────────

fn install_tray(app: &AppHandle) -> tauri::Result<()> {
    let speak = MenuItem::with_id(app, "speak", "Speak selection", true, None::<&str>)?;
    let stop = MenuItem::with_id(app, "stop", "Stop", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", "Settings…", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit kiegen", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;

    let menu = Menu::with_items(app, &[&speak, &stop, &separator, &settings, &quit])?;

    let mut builder = TrayIconBuilder::with_id("kiegen")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .tooltip("kiegen — speak the selection")
        .on_menu_event(|app, event| match event.id().as_ref() {
            "speak" => {
                let app = app.clone();
                std::thread::spawn(move || speak_selection(&app));
            }
            "stop" => {
                app.state::<AppState>().speaker.stop();
                emit_status(app, Phase::Idle, None, None);
            }
            "settings" => show_settings(app),
            "quit" => {
                app.state::<AppState>().speaker.stop();
                app.exit(0);
            }
            _ => {}
        });

    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone()).icon_as_template(true);
    }

    builder.build(app)?;
    Ok(())
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, shortcut, event| {
                    // Press only: `Released` would double-trigger every chord.
                    if event.state != ShortcutState::Pressed {
                        return;
                    }
                    match shortcuts::action_for(app, shortcut) {
                        Some(Action::Speak) => {
                            let app = app.clone();
                            std::thread::spawn(move || speak_selection(&app));
                        }
                        Some(Action::Stop) => {
                            app.state::<AppState>().speaker.stop();
                            emit_status(app, Phase::Idle, None, None);
                        }
                        None => {}
                    }
                })
                .build(),
        )
        .setup(|app| {
            // Menu-bar agent, not a windowed app: no Dock icon, no app switcher entry.
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let handle = app.handle().clone();
            let settings = config::load(&handle);
            let voices = speech::list_voices();

            app.manage(AppState {
                settings: Mutex::new(settings),
                speaker: Speaker::new(),
                voices,
                bindings: Mutex::new(Vec::new()),
            });

            if let Err(error) = shortcuts::apply(&handle) {
                eprintln!("[kiegen] shortcut setup failed: {error}");
            }
            install_tray(&handle)?;

            // First run: nothing is bound, nothing is granted — put the window in front
            // of the user once. Afterwards the tray is the only way in.
            if !capture::is_trusted() {
                show_settings(&handle);
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_state,
            save_settings,
            speak_selection_now,
            speak_text,
            preview_voice,
            stop_speaking,
            install_engine,
            open_settings_window,
            open_accessibility_settings,
            permission_status,
        ])
        .run(tauri::generate_context!())
        .expect("error while running kiegen");
}
