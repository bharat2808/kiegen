//! kiegen — a menu-bar agent that speaks any selected text.
//!
//! Shape (see docs/DESIGN.md §1): no Dock icon, no window at launch, the tray is the
//! entire persistent UI, and the settings window is created on demand. The real app is
//! the Rust service below; the webview is a config editor.

mod capture;
pub mod chatterbox;
pub mod config;
pub mod download;
pub mod engine_paths;
pub mod engines;
pub mod espeak;
pub mod g2p;
pub mod kokoro;
pub mod lexicon;
pub mod numbers;
mod overlay;
mod pcm;
mod shortcuts;
mod speech;
mod speech_job;
pub mod spoken;
mod streaming;
pub mod voices;

use std::sync::Mutex;

use serde::Serialize;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_global_shortcut::ShortcutState;

use config::Settings;
use shortcuts::Action;
use speech::Voice;

/// Copy-mode capture is bounded: an app that never touches the pasteboard should
/// cost a blink, not a hang.
const COPY_TIMEOUT_MS: u64 = 150;

/// Deep-link to the Accessibility pane in System Settings. Used instead of the AX
/// "prompt" API because it lands the user exactly where the toggle lives.
const ACCESSIBILITY_PANE: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility";

pub struct AppState {
    pub settings: Mutex<Settings>,
    job: Mutex<speech_job::SpeechJob>,
    synthesis: Mutex<()>,
    /// Every engine the app can speak with, routed by the settings. The Apple path lives
    /// inside it rather than beside it so there is one place that decides what speaks.
    pub spoken: spoken::Spoken,
    pub voices: Vec<Voice>,
    pub bindings: Mutex<Vec<shortcuts::Binding>>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Idle,
    Capturing,
    Preparing,
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
        speaking: state.spoken.is_speaking(),
        refused_shortcuts,
        system_language: speech::system_language(),
        config_path: config::settings_path(app).display().to_string(),
    }
}

#[tauri::command]
fn get_speech_status(app: AppHandle) -> StatusEvent {
    app.state::<AppState>().job.lock().unwrap().status.clone()
}

fn begin_speech(app: &AppHandle, phase: Phase) -> u64 {
    let state = app.state::<AppState>();
    let mut job = state.job.lock().unwrap();
    state.spoken.stop();
    let id = job.begin(phase);
    let _ = app.emit("kiegen:status", &job.status);
    id
}

fn job_status(
    app: &AppHandle,
    id: u64,
    phase: Phase,
    message: Option<String>,
    chars: Option<usize>,
) {
    let state = app.state::<AppState>();
    let mut job = state.job.lock().unwrap();
    if job.is_current(id) {
        job.set(phase, message, chars);
        let _ = app.emit("kiegen:status", &job.status);
    }
}

/// Serialize synthesis, but keep Stop independent of the model lock. A canceled
/// render may finish computing; only the current job is allowed to start playback.
fn run_speech(app: &AppHandle, id: u64, settings: Settings, text: String, truncated: bool) {
    let state = app.state::<AppState>();
    let _synthesis = state.synthesis.lock().unwrap();
    if !state.job.lock().unwrap().is_current(id) {
        return;
    }
    let chars = text.chars().count();
    job_status(app, id, Phase::Preparing, None, Some(chars));
    if let Some(reason) = selected_engine_refusal(&settings) {
        job_status(app, id, Phase::Error, Some(reason), Some(chars));
        return;
    }
    if settings.engine == config::Engine::Apple {
        let result = {
            let job = state.job.lock().unwrap();
            if !job.is_current(id) {
                return;
            }
            state.spoken.speak(&settings, &text)
        };
        match result {
            Ok(report) => job_status(
                app,
                id,
                Phase::Speaking,
                Some(report.summary()),
                Some(chars),
            ),
            Err(error) => job_status(app, id, Phase::Error, Some(error), Some(chars)),
        }
        return;
    }
    {
        let mut job = state.job.lock().unwrap();
        if !job.is_current(id) {
            return;
        }
        job.streaming = true;
    }
    let result = state
        .spoken
        .stream_pcm(
            &settings,
            &text,
            || !state.job.lock().unwrap().is_current(id),
            |player| {
                let mut job = state.job.lock().unwrap();
                if !job.is_current(id) {
                    return Ok(());
                }
                player.start()?;
                job.set(Phase::Speaking, None, Some(chars));
                let _ = app.emit("kiegen:status", &job.status);
                Ok(())
            },
        )
        .map(|(report, _, _)| report);
    let mut job = state.job.lock().unwrap();
    if !job.is_current(id) {
        return;
    }
    job.streaming = false;
    match result {
        Ok(report) => job.set(
            Phase::Idle,
            Some(if truncated {
                format!("truncated to {} characters", settings.max_chars)
            } else {
                report.summary()
            }),
            Some(chars),
        ),
        Err(error) => {
            state.spoken.stop();
            job.set(Phase::Error, Some(error), Some(chars));
        }
    }
    let _ = app.emit("kiegen:status", &job.status);
}

/// Why the selected engine cannot speak, or `None` if it can.
///
/// A user who picks Kokoro must never hear Samantha and conclude that is what Kokoro
/// sounds like, so there is no fallback here — the shortcut reports the reason instead.
fn selected_engine_refusal(settings: &Settings) -> Option<String> {
    if settings.engine == config::Engine::Apple {
        return None;
    }
    engine_refusal(settings.engine, engines::catalog(settings))
}

fn engine_refusal(engine: config::Engine, catalog: Vec<engines::EngineInfo>) -> Option<String> {
    catalog
        .into_iter()
        .find(|info| info.id == engine)
        .filter(|info| !info.can_speak)
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
fn speak_selection(app: &AppHandle, id: u64) {
    let settings = app.state::<AppState>().settings.lock().unwrap().clone();
    if let Some(reason) = selected_engine_refusal(&settings) {
        job_status(app, id, Phase::Error, Some(reason), None);
        return;
    }
    let text = match capture::capture(
        settings.capture_mode,
        COPY_TIMEOUT_MS,
        settings.restore_clipboard,
    ) {
        Ok(text) => text,
        Err(error) => {
            job_status(app, id, Phase::Error, Some(error.to_string()), None);
            return;
        }
    };
    let truncated = text.chars().count() > settings.max_chars;
    let text = text.chars().take(settings.max_chars).collect();
    run_speech(app, id, settings, text, truncated);
}

fn speak_given(app: &AppHandle, id: u64, text: String) {
    let settings = app.state::<AppState>().settings.lock().unwrap().clone();
    let truncated = text.chars().count() > settings.max_chars;
    let text = text.chars().take(settings.max_chars).collect();
    run_speech(app, id, settings, text, truncated);
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
    let id = begin_speech(&app, Phase::Capturing);
    std::thread::spawn(move || speak_selection(&app, id));
}

#[tauri::command]
fn speak_text(app: AppHandle, text: String) {
    let id = begin_speech(&app, Phase::Preparing);
    std::thread::spawn(move || speak_given(&app, id, text));
}

/// Audition a voice without committing to it. The voice browser previews rows this
/// way, so clicking through the list never silently rewrites the saved setting.
#[tauri::command]
fn preview_voice(app: AppHandle, voice: Option<String>, rate: u32, text: Option<String>) {
    let id = begin_speech(&app, Phase::Preparing);
    let mut settings = app.state::<AppState>().settings.lock().unwrap().clone();
    settings.rate = rate;
    match settings.engine {
        config::Engine::Apple => settings.voice = voice,
        config::Engine::Kokoro => {
            if let Some(voice) = voice {
                settings.kokoro.voice = voice;
            }
        }
        config::Engine::Chatterbox => {
            if let Some(voice) = voice {
                settings.chatterbox.voice = voice;
            }
        }
    }
    let sample =
        text.unwrap_or_else(|| "This is how I sound when reading your selection.".to_string());
    std::thread::spawn(move || run_speech(&app, id, settings, sample, false));
}

#[tauri::command]
fn stop_speaking(app: AppHandle) {
    let state = app.state::<AppState>();
    let mut job = state.job.lock().unwrap();
    job.cancel();
    state.spoken.stop();
    let _ = app.emit("kiegen:status", &job.status);
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

/// Copy a user-chosen WAV into the app's own voice store, then report the new catalogue.
///
/// `path` is resolved by the window's file dialog, so this never prompts: a Rust command that
/// blocks on a modal dialog is a command the UI cannot show progress for.
#[tauri::command]
fn add_chatterbox_voice(app: AppHandle, path: String) -> Result<UiState, String> {
    voices::add(std::path::Path::new(&path))?;
    Ok(ui_state(&app, Vec::new()))
}

/// Delete a stored reference clip, clearing the selection if it was the chosen one — a config
/// naming a file that is gone would otherwise read as "the user picked something invalid".
#[tauri::command]
fn delete_chatterbox_voice(app: AppHandle, file: String) -> Result<UiState, String> {
    voices::remove(&file)?;
    {
        let state = app.state::<AppState>();
        let mut settings = state.settings.lock().unwrap();
        if settings.chatterbox.ref_audio.as_deref() == Some(file.as_str()) {
            settings.chatterbox.ref_audio = None;
            let snapshot = settings.clone();
            drop(settings);
            config::save(&app, &snapshot)?;
        }
    }
    Ok(ui_state(&app, Vec::new()))
}

/// Kokoro's and Chatterbox's weights are plain files the app fetches and verifies itself.
/// Nothing here is installed by another runtime: the app owns every file it needs, which is
/// what makes the local engines work with no Python on the machine at all.
fn install_local_engine(app: &AppHandle, engine: config::Engine) -> Result<(), String> {
    let dir = match engine {
        config::Engine::Kokoro => engine_paths::kokoro_dir(),
        config::Engine::Chatterbox => engine_paths::chatterbox_dir(),
        config::Engine::Apple => return Ok(()),
    }
    .ok_or("could not locate the app support directory")?;

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
            emit_install(app, engine, "downloading", path, done, total, None);
        }
    };

    match engine {
        config::Engine::Kokoro => download::install_kokoro_into(&dir, &mut on_progress)?,
        _ => download::install_chatterbox_into(&dir, &mut on_progress)?,
    }
    emit_install(app, engine, "done", "", actual, last_total, None);
    Ok(())
}

fn install_engine_blocking(app: &AppHandle, engine: config::Engine) {
    match engine {
        // Already on the machine; nothing to fetch.
        config::Engine::Apple => {}
        config::Engine::Kokoro | config::Engine::Chatterbox => {
            if let Err(error) = install_local_engine(app, engine) {
                emit_install(app, engine, "error", "", 0, 0, Some(error));
            }
        }
    }
}

#[tauri::command]
fn install_engine(app: AppHandle, engine: config::Engine) {
    std::thread::spawn(move || install_engine_blocking(&app, engine));
}

/// Install espeak-ng — the extra back end for Kokoro's five non-English languages.
///
/// Threaded and event-reported rather than returning a value, because a package-manager run
/// takes long enough that blocking the command would freeze the pane, and because the
/// catalogue has to be re-read afterwards either way.
#[tauri::command]
fn install_espeak_ng(app: AppHandle) {
    std::thread::spawn(move || {
        let message = match espeak::install() {
            Ok(message) => message,
            Err(error) => error,
        };
        let _ = app.emit("kiegen:espeak", message);
    });
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
                let id = begin_speech(&app, Phase::Capturing);
                std::thread::spawn(move || speak_selection(&app, id));
            }
            "stop" => {
                stop_speaking(app.clone());
            }
            "settings" => show_settings(app),
            "quit" => {
                app.state::<AppState>().spoken.stop();
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
        .on_window_event(|window, event| {
            // A menu-bar app keeps its settings window alive between visits. Destroying
            // it makes get_webview_window("main") return None when Settings is clicked.
            if window.label() == "main" {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
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
                            let id = begin_speech(&app, Phase::Capturing);
                            std::thread::spawn(move || speak_selection(&app, id));
                        }
                        Some(Action::Stop) => {
                            stop_speaking(app.clone());
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
                job: Mutex::new(speech_job::SpeechJob::default()),
                synthesis: Mutex::new(()),
                spoken: spoken::Spoken::new(),
                voices,
                bindings: Mutex::new(Vec::new()),
            });

            if let Err(error) = shortcuts::apply(&handle) {
                eprintln!("[kiegen] shortcut setup failed: {error}");
            }
            install_tray(&handle)?;
            overlay::setup(&handle)?;

            // First run: nothing is bound, nothing is granted — put the window in front
            // of the user once. Afterwards the tray is the only way in.
            if !capture::is_trusted() {
                show_settings(&handle);
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_state,
            get_speech_status,
            save_settings,
            speak_selection_now,
            speak_text,
            preview_voice,
            stop_speaking,
            install_engine,
            install_espeak_ng,
            add_chatterbox_voice,
            delete_chatterbox_voice,
            open_settings_window,
            open_accessibility_settings,
            permission_status,
        ])
        .run(tauri::generate_context!())
        .expect("error while running kiegen");
}

#[cfg(test)]
mod readiness_tests {
    use super::*;

    #[test]
    fn ready_local_engines_are_allowed_to_speak() {
        for engine in [config::Engine::Kokoro, config::Engine::Chatterbox] {
            let mut catalog = engines::catalog_with(&config::Settings::default(), Vec::new());
            let info = catalog.iter_mut().find(|info| info.id == engine).unwrap();
            info.can_speak = true;
            info.blocked_reason = None;
            assert_eq!(engine_refusal(engine, catalog), None);
        }
    }

    #[test]
    fn unavailable_engine_explains_what_is_missing() {
        let mut catalog = engines::catalog_with(&config::Settings::default(), Vec::new());
        let info = catalog
            .iter_mut()
            .find(|info| info.id == config::Engine::Kokoro)
            .unwrap();
        info.can_speak = false;
        info.blocked_reason = Some("Weights missing".to_string());
        let reason = engine_refusal(config::Engine::Kokoro, catalog).unwrap();
        assert!(reason.contains("Weights missing"));
    }

    #[test]
    fn apple_speech_remains_available() {
        assert_eq!(selected_engine_refusal(&config::Settings::default()), None);
    }
}
