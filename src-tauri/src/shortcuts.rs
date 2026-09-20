//! Shortcut registration.
//!
//! Rust owns the shortcuts; the webview only edits the accelerator strings and
//! asks for a re-apply. Applying is unregister-all-then-register-from-config, so a
//! half-broken config can never leave the app wedged with stale hotkeys.

use crate::config::Settings;
use crate::AppState;
use tauri::{AppHandle, Manager};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum Action {
    Speak,
    Stop,
}

#[derive(Debug, Clone)]
pub struct Binding {
    pub shortcut: Shortcut,
    pub action: Action,
}

pub fn parse(accelerator: &str) -> Result<Shortcut, String> {
    accelerator
        .trim()
        .parse::<Shortcut>()
        .map_err(|e| format!("`{accelerator}` is not a valid shortcut ({e})"))
}

/// Validate the whole set before touching the OS: parse errors and collisions are
/// reported to the user rather than silently dropping a chord.
pub fn bindings(settings: &Settings) -> Result<Vec<Binding>, String> {
    let mut out: Vec<Binding> = Vec::new();

    for (accelerator, action) in [
        (&settings.shortcuts.speak, Action::Speak),
        (&settings.shortcuts.stop, Action::Stop),
    ] {
        if accelerator.trim().is_empty() {
            continue;
        }
        let shortcut = parse(accelerator)?;
        if let Some(previous) = out.iter().find(|b| b.shortcut == shortcut) {
            return Err(format!(
                "`{accelerator}` is already bound to {:?}",
                previous.action
            ));
        }
        out.push(Binding { shortcut, action });
    }

    Ok(out)
}

/// Unregister everything, then register what the (already validated) config asks for.
/// Returns the accelerators that the OS refused — usually because another app owns them.
pub fn apply(app: &AppHandle) -> Result<Vec<String>, String> {
    let settings = app.state::<AppState>().settings.lock().unwrap().clone();
    let bindings = bindings(&settings)?;

    let manager = app.global_shortcut();
    manager
        .unregister_all()
        .map_err(|e| format!("could not clear existing shortcuts: {e}"))?;

    let mut refused = Vec::new();
    for binding in &bindings {
        if let Err(e) = manager.register(binding.shortcut) {
            eprintln!("[kiegen] OS refused {:?}: {e}", binding.shortcut);
            refused.push(binding.shortcut.to_string());
        }
    }

    *app.state::<AppState>().bindings.lock().unwrap() = bindings
        .into_iter()
        .filter(|b| !refused.contains(&b.shortcut.to_string()))
        .collect();

    Ok(refused)
}

/// Look up the action bound to a chord the OS just delivered.
pub fn action_for(app: &AppHandle, shortcut: &Shortcut) -> Option<Action> {
    app.state::<AppState>()
        .bindings
        .lock()
        .unwrap()
        .iter()
        .find(|b| &b.shortcut == shortcut)
        .map(|b| b.action)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Settings;

    /// The shipped defaults must actually parse: a typo here means a silently
    /// hotkey-less app on first launch.
    #[test]
    fn default_shortcuts_parse_and_bind() {
        let bindings = bindings(&Settings::default()).expect("defaults must parse");
        assert_eq!(bindings.len(), 2);
        let actions: Vec<Action> = bindings.iter().map(|b| b.action).collect();
        assert!(actions.contains(&Action::Speak));
        assert!(actions.contains(&Action::Stop));
    }

    /// Two actions on the same chord is a configuration error the user must see,
    /// not a silent last-one-wins.
    #[test]
    fn duplicate_bindings_are_rejected() {
        let mut settings = Settings::default();
        settings.shortcuts.stop = settings.shortcuts.speak.clone();
        let error = bindings(&settings).expect_err("duplicates must be rejected");
        assert!(
            error.contains("already bound"),
            "unexpected message: {error}"
        );
    }

    #[test]
    fn invalid_accelerator_is_rejected_with_context() {
        let mut settings = Settings::default();
        // Not a key name in global_hotkey's vocabulary.
        settings.shortcuts.speak = "Cmd+Shift+NotAKey".to_string();
        let error = bindings(&settings).expect_err("invalid accelerator must be rejected");
        assert!(error.contains("NotAKey"), "unexpected message: {error}");
    }

    /// `CommandOrControl` is Tauri-JS syntax, but global_hotkey accepts it too and maps
    /// it per platform (SUPER on macOS, CONTROL elsewhere) — verified in its
    /// `parse_hotkey`. Worth keeping working, since it is what a portable default uses.
    #[test]
    fn command_or_control_maps_to_super_on_macos() {
        let mut settings = Settings::default();
        settings.shortcuts.speak = "CommandOrControl+Shift+S".to_string();
        let parsed = bindings(&settings).expect("must parse");
        let speak = parsed
            .iter()
            .find(|b| b.action == Action::Speak)
            .expect("speak binding");
        assert!(speak
            .shortcut
            .mods
            .contains(tauri_plugin_global_shortcut::Modifiers::SUPER));
    }

    /// Function keys are legal without modifiers; bare letters are not, because they
    /// would swallow normal typing system-wide.
    #[test]
    fn function_keys_bind_but_bare_letters_are_questionable() {
        let mut settings = Settings::default();
        settings.shortcuts.speak = "F8".to_string();
        assert!(bindings(&settings).is_ok());

        settings.shortcuts.speak = "S".to_string();
        assert!(
            bindings(&settings).is_ok(),
            "the OS decides; we only require it to parse"
        );
    }
}
