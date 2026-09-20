# Developing kiegen

Everything here is about the one thing that makes this app awkward to develop: **macOS
permission**. The Rust is ordinary; the TCC grant is not.

## The rebuild problem

TCC keys the Accessibility grant to the app's **bundle identifier + code signature**. A
clean `cargo build` produces a differently-signed binary, so macOS treats it as a new app:

- the toggle in System Settings goes stale (it is still checked, but does nothing);
- the app reports `required` in the settings panel even though it "looks granted".

Two habits make this bearable:

```bash
# 1. Clear the stale grant so the app reappears cleanly in the list
tccutil reset Accessibility com.kiegen.app

# 2. Re-open the pane, toggle kiegen on, done
open "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility"
```

The settings window polls `AXIsProcessTrusted` every 1.5 s while it is open, so the
"granted / required" line flips by itself once you toggle it — no relaunch needed.

If you are going to iterate on capture a lot, sign with a **stable self-signed identity**
in `tauri.conf.json` → `bundle.macOS.signingIdentity`. The grant then survives rebuilds,
which is worth the ten minutes of certificate wrangling.

## Watching what macOS does

```bash
# TCC + Accessibility decisions for this app
log stream --predicate 'process == "kiegen"' --level info

# our own diagnostics (capture failures print here)
./src-tauri/target/release/kiegen
```

`[kiegen] capture failed: …` is the line to watch. It is emitted on every failed capture
so the hotkey path is debuggable without a GUI.

## What the tests cover, and what they cannot

`cargo test` covers the parts that do not need a grant:

- `capture::tests::without_permission_capture_refuses_instead_of_guessing` — in all three
  capture modes, an ungranted process returns `NoPermission` and posts **no** synthetic
  keystroke. This one is inverted on a machine that already has the grant (it returns
  early), so CI on a clean runner is the meaningful run.
- `shortcuts::tests::*` — the shipped defaults parse, duplicates are rejected with a
  message naming the offending chord, invalid accelerators are rejected *before* anything
  is written to disk, and `CommandOrControl` maps to SUPER on macOS.
- `speech::tests::*` — `say -v ?` parsing, including names with spaces ("Bad News").

What no test can cover: the actual end-to-end capture, because it needs a real grant and
a real selection in a real app. To check it by hand:

1. Open TextEdit, type a sentence, select it.
2. Press the speak shortcut (default `Cmd+Shift+S`).
3. Expect speech within ~100 ms for AX, up to ~150 ms more if it fell back to ⌘C.

If nothing happens, check the four failure modes in order: permission (settings panel),
secure input (password field focused — deliberately refused), no selection, and an app
that exposes neither AX text nor copy.

## Iterating on the UI without the app

The settings panel is a WKWebView inside a menu-bar app, which is a slow loop to inspect.
`scripts/preview.sh` builds the frontend, writes a harness page that stubs the Tauri IPC
(`get_state`, `save_settings`, `preview_voice`) with **the real voice list from `say -v ?`
and the real system language from `AppleLocale`**, and serves it:

```bash
./scripts/preview.sh          # → http://127.0.0.1:8777
```

Everything except the Rust round-trip behaves exactly as it does in the app, so it is the
place to check layout, the voice browser and the shortcut recorder. Screenshots in
`.preview/shots/` come from this harness.

The design language is not invented here: `src/App.css` opens with a table mapping each
colour and metric to the AppKit/SwiftUI construct it mirrors (card = `controlBackgroundColor`
at 50 % with a 6 % hairline and radius 10; selection row = accent fill at 10 % with a 1.5 px
accent stroke and radius 8; AppKit point sizes for the type scale). Change the token, not
the call site.

## Layout

```
src-tauri/src/
  lib.rs         tray, global shortcuts, commands, event emission
  capture.rs     Accessibility API + ⌘C/pasteboard fallback  ← the risky part
  speech.rs      the `say` bootstrap engine (Kokoro replaces it in v0.5)
  shortcuts.rs   accelerator parsing, validation, registration
  config.rs      one JSON file in the app config dir
src/App.tsx      settings panel: General / Voice / Shortcuts / Capture panes
src/App.css      design tokens + components (see the mapping table at the top)
scripts/
  check-licenses.sh   fails the build if GPL-family code enters the tree
  preview.sh          browser preview of the settings UI
```

## Voice browser

`speech::system_language()` reads `AppleLocale` (falling back to `AppleLanguages[0]`, then
`en_US`) so the Voice pane opens on the user's own language. There is frequently **no voice
for the exact locale** — `en_CA` has none on macOS — so the pane falls back to the language
family, largest first, says so, and offers the other regions as chips.

`say -v ?` lists novelty voices (Bells, Zarvox, …) mixed in with speech. They are flagged in
`speech.rs`, hidden by default, and revealed by one button: offering "Bells" as a reading
voice is a trap. The `every_installed_voice_is_parsed` test guards the parser against
silently dropping voices — it caught `ar_001`, the one voice whose region is numeric.

## Where the risk actually is

Capture runs on a worker thread — never the main thread — because the AX read plus a
possible ⌘C round-trip blocks for up to `COPY_TIMEOUT_MS` (150 ms), and a frozen main
thread would freeze the tray. If you add work to that path, keep it off the main thread.

The clipboard fallback in v0 restores **text only**. Full-fidelity restore (images, rich
text, multiple flavours) needs `pasteboardItems()` + `writeObjects:` over
`NSPasteboardWriting` objects and is scheduled for v1 — the setting
`restore_clipboard` exists so users can opt out in the meantime.