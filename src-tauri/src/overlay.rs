//! A non-focusing status panel, following FreeFlow's top-center black overlay.
use crate::{AppState, Phase};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

pub fn setup(app: &AppHandle) -> tauri::Result<()> {
    WebviewWindowBuilder::new(
        app,
        "speech-overlay",
        WebviewUrl::App("index.html?overlay".into()),
    )
    .title("Kiegen speech")
    .inner_size(270.0, 44.0)
    .decorations(false)
    .transparent(true)
    .shadow(false)
    .resizable(false)
    .focused(false)
    .focusable(false)
    .always_on_top(true)
    .visible_on_all_workspaces(true)
    .skip_taskbar(true)
    .visible(false)
    .build()?;

    let app = app.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_millis(100));
        let handle = app.clone();
        if app.run_on_main_thread(move || tick(&handle)).is_err() {
            break;
        }
    });
    Ok(())
}

fn tick(app: &AppHandle) {
    let state = app.state::<AppState>();
    let mut job = state.job.lock().unwrap();
    let playing = state.spoken.is_speaking();
    let changed = job.observe_playback(playing);
    let expired_error =
        matches!(job.status.phase, Phase::Error) && job.changed.elapsed() > Duration::from_secs(5);
    if expired_error {
        job.set(Phase::Idle, None, None);
    }
    if changed || expired_error {
        let _ = app.emit("kiegen:status", &job.status);
    }
    let visible = !matches!(job.status.phase, Phase::Idle);
    drop(job);
    let Some(window) = app.get_webview_window("speech-overlay") else {
        return;
    };
    if visible && !window.is_visible().unwrap_or(false) {
        position(&window);
        let _ = window.show();
    } else if !visible && window.is_visible().unwrap_or(false) {
        let _ = window.hide();
    }
}

fn position(window: &tauri::WebviewWindow) {
    // AppKit coordinates are logical points with the origin at the bottom left.
    // Stay below the menu bar/notch so the label and Stop remain visible on every Mac.
    #[cfg(target_os = "macos")]
    {
        use objc2::MainThreadMarker;
        use objc2_app_kit::{NSScreen, NSWindow, NSWindowCollectionBehavior};
        use objc2_foundation::{NSPoint, NSRect, NSSize};
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let Some(screen) = NSScreen::mainScreen(mtm) else {
            return;
        };
        let frame = screen.visibleFrame();
        if let Ok(raw) = window.ns_window() {
            // Tauri owns the NSWindow; this borrow is only used on the main thread.
            let native = unsafe { &*(raw as *const NSWindow) };
            native.setLevel(1000); // NSScreenSaverWindowLevel, matching FreeFlow.
            native.setCollectionBehavior(
                NSWindowCollectionBehavior::CanJoinAllSpaces
                    | NSWindowCollectionBehavior::FullScreenAuxiliary,
            );
            native.setFrame_display(
                NSRect::new(
                    NSPoint::new(
                        frame.origin.x + (frame.size.width - 270.0) / 2.0,
                        frame.origin.y + frame.size.height - 50.0,
                    ),
                    NSSize::new(270.0, 44.0),
                ),
                true,
            );
        }
    }
}
