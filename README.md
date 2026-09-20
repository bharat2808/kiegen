# kiegen

Select text in **any** app, press a shortcut, hear it read aloud. kiegen lives in the
menu bar — no Dock icon, no window at launch. The settings panel opens only when you ask
for it.

> **Status: v0.** The menu-bar agent works end to end: tray menu, global shortcuts,
> selection capture (Accessibility API with a clipboard fallback), and speech through
> macOS `say`. The Kokoro neural voice engine lands in v0.5 — see
> [docs/DESIGN.md](docs/DESIGN.md) §5 for the measured plan behind that.

## Why it needs Accessibility permission

macOS has no general API for reading another app's selection. Only the Accessibility
API can do it, and it requires an explicit grant:

**System Settings → Privacy & Security → Accessibility → enable kiegen**

Without it, kiegen does nothing — it will not guess, and it will not touch your
clipboard. Everything happens locally: selections are never logged, cached, or sent
anywhere, and there is no network code in this repository.

If you would rather never risk your clipboard, set **Method → “Accessibility only”**.
If you use Chrome, Electron apps, or a terminal, set **“Copying only”** — those apps
often expose no usable Accessibility tree.

## Using it

| Action | Default |
|---|---|
| Speak the selection | `Cmd+Shift+S` |
| Stop | `Cmd+Shift+X` |
| Open settings | tray icon → Settings… |

Both shortcuts are rebindable. Click **Record…**, press the combo; a modifier is required
so a bare key cannot be swallowed system-wide.

## Picking a voice

**Voice** opens on your system language and lists only that language's voices. Each row has
a play button to audition it before you commit, and the chosen one is marked *Default*.

Two things macOS makes awkward, handled up front:

- **There may be no voice for your exact locale.** Canadian English has none, so kiegen
  falls back to the closest language family, says which one it used, and puts the other
  regions (UK, Australia, Ireland, …) one click away.
- **Novelty voices are hidden.** `say` lists Bells, Zarvox, Boing and friends alongside real
  voices. They are sound effects, not speech, so they sit behind a "show 15 novelty voices"
  button rather than in your face.

Speed is a slider (80–500 wpm, 200 default) and previews immediately.

## Design

The settings panel mirrors the visual language of the local **freeflow-notes** app —
AppKit's own metrics rather than web conventions:
a 180 pt sidebar, cards of `controlBackgroundColor` at 50 % with a 6 % hairline and radius
10, selection rows tinted with the accent colour, and AppKit point sizes for type. The
mapping table at the top of `src/App.css` records each token against the SwiftUI construct
it came from, so the two stay in step.

## Build from source

Requires macOS, Node 22+, Rust stable.

```bash
npm install
npm run tauri dev      # dev build, hot reload on the frontend
npm run tauri build    # produces src-tauri/target/release/bundle/macos/kiegen.app
```

Then open the built app and grant Accessibility as above.

**Expect to re-grant after every rebuild.** macOS ties the permission to the app's code
signature, so an unsigned dev build looks like a new app each time. `tccutil reset
Accessibility com.kiegen.app` clears the stale entry so it reappears in the list — see
[docs/DEV.md](docs/DEV.md) for the full dev loop.

## Tests

```bash
cargo test --manifest-path src-tauri/Cargo.toml   # capture guard, shortcut parsing, voices
./scripts/check-licenses.sh                       # no copyleft in the dependency graph
./scripts/preview.sh                              # settings UI in a browser (stubbed IPC)
```

## Licence

`MIT OR Apache-2.0`, at your option. kiegen deliberately depends on nothing copyleft:
`./scripts/check-licenses.sh` scans the resolved graph, and [`deny.toml`](deny.toml)
enforces the same rule in CI, including an explicit ban on the `kokoro-tts` crate, whose
build compiles GPL-3.0-derived C.

**Not affiliated with the Kokoro authors.** Kokoro-82M weights are Apache-2.0 and will be
downloaded (not vendored) when the v0.5 engine lands; the model card documents two CC BY
training corpora which will be credited in an in-app licences screen.