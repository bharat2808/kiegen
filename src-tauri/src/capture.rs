//! Selection capture — the risky part of the app.
//!
//! Two mechanisms, one entry point:
//!   1. the Accessibility API (non-destructive; unreliable in Electron/Chrome/terminals)
//!   2. synthetic ⌘C into the pasteboard (universal; briefly owns the clipboard)
//!
//! Everything here requires the Accessibility TCC grant. A denied grant shows up
//! as `NoPermission`, never as a silent empty string.

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureError {
    /// Accessibility not granted (or revoked since launch).
    NoPermission,
    /// Permission is fine; there was nothing selected / the app exposes no selection.
    NoSelection,
    /// Posted ⌘C but the pasteboard never changed.
    Timeout,
    /// A copy happened but the pasteboard held no usable text.
    Empty,
    /// The user is typing into a secure field (password); we refuse before posting anything.
    SecureInput,
}

impl std::fmt::Display for CaptureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let msg = match self {
            Self::NoPermission => {
                "TextHalo needs Accessibility permission (System Settings → Privacy & Security → Accessibility)"
            }
            Self::NoSelection => "no selected text found",
            Self::Timeout => "nothing was copied — is text selected?",
            Self::Empty => "the selection contained no text",
            Self::SecureInput => "not available in password fields",
        };
        f.write_str(msg)
    }
}

/// Capture the current selection from whatever app is frontmost.
///
/// `max_chars` truncation is applied by the caller so the error semantics stay clean.
pub fn capture(
    mode: crate::config::CaptureMode,
    timeout_ms: u64,
    restore: bool,
) -> Result<String, CaptureError> {
    platform::capture(mode, timeout_ms, restore)
}

/// Is the Accessibility grant in place right now? Re-checked per invocation, never cached:
/// users revoke it, and every rebuild of an unsigned dev build invalidates it.
pub fn is_trusted() -> bool {
    platform::is_trusted()
}

/// Show macOS's first-run Accessibility permission prompt when access is missing.
pub fn request_accessibility() -> bool {
    platform::request_accessibility()
}

/// A copy-mode capture while secure input is on would leak nothing (the OS blocks it)
/// but would also silently fail; detecting it lets us say something useful instead.
pub fn secure_input_active() -> bool {
    platform::secure_input_active()
}

// ─────────────────────────────── macOS ───────────────────────────────
#[cfg(target_os = "macos")]
mod platform {
    use super::CaptureError;
    use accessibility_sys::{
        kAXErrorSuccess, kAXFocusedUIElementAttribute, kAXSelectedTextAttribute,
        kAXTrustedCheckOptionPrompt, AXIsProcessTrusted, AXIsProcessTrustedWithOptions,
        AXUIElementCopyAttributeValue, AXUIElementCreateSystemWide, AXUIElementRef,
    };
    use core_foundation::base::{CFGetTypeID, CFRelease, CFTypeRef, TCFType};
    use core_foundation::boolean::CFBoolean;
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::string::{CFString, CFStringRef};
    use core_graphics::event::{CGEvent, CGEventFlags, CGEventTapLocation, KeyCode};
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
    use objc2_app_kit::{NSPasteboard, NSPasteboardType, NSPasteboardTypeString};
    use objc2_foundation::NSString;
    use std::time::{Duration, Instant};

    pub fn is_trusted() -> bool {
        unsafe { AXIsProcessTrusted() }
    }

    /// Ask macOS to show its Accessibility permission prompt.
    pub fn request_accessibility() -> bool {
        let prompt_key = unsafe { CFString::wrap_under_get_rule(kAXTrustedCheckOptionPrompt) };
        let options: CFDictionary<CFString, CFBoolean> =
            CFDictionary::from_CFType_pairs(&[(prompt_key, CFBoolean::true_value())]);
        unsafe { AXIsProcessTrustedWithOptions(options.as_concrete_TypeRef()) }
    }

    /// `IsSecureEventInputEnabled()` lives in HIToolbox. Resolve it at runtime rather than
    /// linking the framework: we only need a yes/no, and a missing symbol must not be fatal.
    ///
    /// `b"…\0"` rather than a C-string literal because this crate is edition 2021
    /// (clippy::manual_c_str_literals only knows about `c"…"`).
    pub fn secure_input_active() -> bool {
        #[allow(clippy::manual_c_str_literals)] // c"..." needs edition 2024
        const SYMBOL: &[u8] = b"IsSecureEventInputEnabled\0";
        unsafe {
            let sym = libc::dlsym(libc::RTLD_DEFAULT, SYMBOL.as_ptr() as *const libc::c_char);
            if sym.is_null() {
                return false;
            }
            let f: extern "C" fn() -> bool = std::mem::transmute(sym);
            f()
        }
    }

    /// Read one attribute off an AX element, taking ownership of the returned CFTypeRef.
    unsafe fn copy_attribute(
        element: AXUIElementRef,
        attribute: &'static str,
    ) -> Option<CFTypeRef> {
        let name = CFString::from_static_string(attribute);
        let mut value: CFTypeRef = std::ptr::null();
        let err = AXUIElementCopyAttributeValue(element, name.as_concrete_TypeRef(), &mut value);
        if err == kAXErrorSuccess && !value.is_null() {
            Some(value)
        } else {
            None
        }
    }

    /// Accessibility path: system-wide element → focused element → AXSelectedText.
    pub fn ax_selected_text() -> Option<String> {
        unsafe {
            let system_wide = AXUIElementCreateSystemWide();
            if system_wide.is_null() {
                return None;
            }
            let focused = copy_attribute(system_wide, kAXFocusedUIElementAttribute);
            CFRelease(system_wide as CFTypeRef);
            let focused = focused?;

            let text = copy_attribute(focused as AXUIElementRef, kAXSelectedTextAttribute);
            CFRelease(focused);
            let text = text?;

            // Defensive: an app may return a non-string for this attribute.
            if CFGetTypeID(text) != CFString::type_id() {
                CFRelease(text);
                return None;
            }
            let cf = CFString::wrap_under_create_rule(text as CFStringRef);
            let s = cf.to_string();
            if s.trim().is_empty() {
                None
            } else {
                Some(s)
            }
        }
    }

    /// Post a synthetic ⌘C to the focused app.
    fn post_copy_keystroke() {
        let Ok(source) = CGEventSource::new(CGEventSourceStateID::CombinedSessionState) else {
            return;
        };
        for keydown in [true, false] {
            let Ok(event) = CGEvent::new_keyboard_event(source.clone(), KeyCode::ANSI_C, keydown)
            else {
                continue;
            };
            event.set_flags(CGEventFlags::CGEventFlagCommand);
            event.post(CGEventTapLocation::HID);
        }
    }

    /// Copy path: snapshot the pasteboard text → ⌘C → wait for changeCount → read → restore.
    ///
    /// v0 snapshots and restores *text only*. Full-fidelity restore needs
    /// `NSPasteboard.pasteboardItems()` + `writeObjects:` with `NSPasteboardWriting`
    /// objects (see docs/DESIGN.md §2) — deferred to v1, and the reason
    /// `restore_clipboard` is a setting rather than an assumption.
    pub fn copy_selected_text(timeout_ms: u64, restore: bool) -> Result<String, CaptureError> {
        objc2::rc::autoreleasepool(|_| {
            let pasteboard = NSPasteboard::generalPasteboard();
            let before = pasteboard.changeCount();

            let previous: Option<String> = unsafe {
                pasteboard
                    .stringForType(NSPasteboardTypeString)
                    .map(|s| s.to_string())
            };

            post_copy_keystroke();

            let deadline = Instant::now() + Duration::from_millis(timeout_ms);
            let mut captured: Option<String> = None;
            while Instant::now() < deadline {
                if pasteboard.changeCount() != before {
                    captured = unsafe {
                        pasteboard
                            .stringForType(NSPasteboardTypeString)
                            .map(|s| s.to_string())
                    };
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }

            if restore {
                if let Some(previous) = previous {
                    let ty: &NSPasteboardType = unsafe { NSPasteboardTypeString };
                    pasteboard.clearContents();
                    pasteboard.setString_forType(&NSString::from_str(&previous), ty);
                }
            }

            match captured {
                Some(text) if !text.trim().is_empty() => Ok(text),
                Some(_) => Err(CaptureError::Empty),
                None => Err(CaptureError::Timeout),
            }
        })
    }

    pub fn capture(
        mode: crate::config::CaptureMode,
        timeout_ms: u64,
        restore: bool,
    ) -> Result<String, CaptureError> {
        use crate::config::CaptureMode;
        if !is_trusted() {
            return Err(CaptureError::NoPermission);
        }
        match mode {
            CaptureMode::AxOnly => ax_selected_text().ok_or(CaptureError::NoSelection),
            CaptureMode::CopyOnly => {
                if secure_input_active() {
                    return Err(CaptureError::SecureInput);
                }
                copy_selected_text(timeout_ms, restore)
            }
            CaptureMode::AxThenCopy => {
                if let Some(text) = ax_selected_text() {
                    return Ok(text);
                }
                if secure_input_active() {
                    return Err(CaptureError::SecureInput);
                }
                copy_selected_text(timeout_ms, restore)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CaptureMode;

    /// Without the Accessibility grant, capture must fail *loudly and cheaply*:
    /// a typed error, no synthetic keystroke, no clipboard mutation.
    #[test]
    fn without_permission_capture_refuses_instead_of_guessing() {
        if is_trusted() {
            // Developer machine with the grant in place — nothing to assert here.
            return;
        }
        for mode in [
            CaptureMode::AxThenCopy,
            CaptureMode::AxOnly,
            CaptureMode::CopyOnly,
        ] {
            let result = capture(mode, 150, true);
            assert!(
                matches!(result, Err(CaptureError::NoPermission)),
                "expected NoPermission for {mode:?}, got {result:?}"
            );
        }
    }
}

// ─────────────────────────── other platforms ───────────────────────────
#[cfg(not(target_os = "macos"))]
mod platform {
    use super::CaptureError;

    pub fn is_trusted() -> bool {
        false
    }

    pub fn request_accessibility() -> bool {
        false
    }

    pub fn secure_input_active() -> bool {
        false
    }

    pub fn capture(
        _mode: crate::config::CaptureMode,
        _timeout_ms: u64,
        _restore: bool,
    ) -> Result<String, CaptureError> {
        Err(CaptureError::NoPermission)
    }
}
