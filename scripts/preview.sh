#!/usr/bin/env bash
# Render the settings window in a normal browser, with the Tauri IPC stubbed.
#
# Why: the real window is a WKWebView inside a menu-bar app, which is awkward to
# inspect while iterating on the design. This builds the frontend, writes a harness
# page that fakes `invoke()` (get_state / save_settings / preview_voice) with data
# taken from the real `say -v ?` list, and serves it.
#
#   ./scripts/preview.sh            # then open http://127.0.0.1:8777
#   ./scripts/preview.sh 9000
set -euo pipefail

PORT="${1:-8777}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PREVIEW="$ROOT/.preview"

cd "$ROOT"
npm run build

python3 - "$ROOT" "$PREVIEW" <<'PY'
import json, pathlib, re, shutil, subprocess, sys

root, preview = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
preview.mkdir(exist_ok=True)
shutil.rmtree(preview / "assets", ignore_errors=True)
shutil.copytree(root / "dist" / "assets", preview / "assets")

html = (root / "dist" / "index.html").read_text()
css = re.search(r'href="([^"]+\.css)"', html).group(1)
js = re.search(r'src="([^"]+\.js)"', html).group(1)

# Mirror speech::list_voices so the preview shows the machine's real voice list.
NOVELTY = {"Albert", "Bad News", "Bahh", "Bells", "Boing", "Bubbles", "Cellos", "Deranged",
           "Good News", "Hysterical", "Jester", "Organ", "Superstar", "Trinoids", "Whisper",
           "Wobble", "Zarvox"}
out = subprocess.run(["/usr/bin/say", "-v", "?"], capture_output=True, text=True).stdout
voices = []
for line in out.splitlines():
    tokens = line.split("#")[0].split()
    idx = next((i for i, t in enumerate(tokens)
                if re.fullmatch(r"[a-z]{2}[_-]([A-Z]{2}|[0-9]{3})", t)), None)
    if idx is None:
        continue
    name = " ".join(tokens[:idx])
    if name:
        voices.append({"name": name,
                       "locale": tokens[idx].replace("-", "_"),
                       "novelty": name.split(" (")[0].strip() in NOVELTY})

# Mirror speech::system_language so the preview opens on the machine's real language.
try:
    raw = subprocess.run(["/usr/bin/defaults", "read", "-g", "AppleLocale"],
                         capture_output=True, text=True).stdout.strip()
    system_language = raw.replace("-", "_") if re.fullmatch(r"[a-z]{2}[-_][A-Z]{2}", raw) else "en_US"
except Exception:
    system_language = "en_US"

# The engine catalogue is produced by the real Rust code, so the preview cannot drift
# from the app the moment a voice or an engine changes.
catalog = subprocess.run(["cargo", "run", "--quiet", "--example", "engine_catalog"],
                         cwd=root / "src-tauri", capture_output=True, text=True)
if catalog.returncode != 0:
    raise SystemExit("engine_catalog example failed:\n" + catalog.stderr[-800:])
engines = json.loads(catalog.stdout)

stub = """
const VOICES = %s;
const ENGINES = %s;
const SYSTEM_LANGUAGE = %s;
const state = {
  settings: { shortcuts: { speak: "Cmd+Shift+S", stop: "Cmd+Shift+X" },
              engine: "apple",
              kokoro: { voice: "af_heart", quant: "fp32", speed: 1.0,
                        keep_warm: true, idle_unload_minutes: 30 },
              chatterbox: { voice: "en", exaggeration: 0.1, cfg_weight: 0.5,
                            ref_audio: null, keep_warm: false },
              voice: null,
              rate: 200, capture_mode: "ax_then_copy", max_chars: 5000, restore_clipboard: true },
  voices: VOICES, engines: ENGINES, trusted: false, secure_input: false, speaking: false,
  refused_shortcuts: [], system_language: SYSTEM_LANGUAGE,
  config_path: "/Users/home/Library/Application Support/com.kiegen.app/settings.json"
};
const handlers = {};
window.__TAURI_INTERNALS__ = {
  invoke: async (cmd, args) => {
    if (cmd === "get_state") return state;
    // The native picker, stubbed: a fixed path is enough to preview the "voice added" row.
    if (cmd === "plugin:dialog|open") return "/Users/home/Documents/my_voice.wav";
    if (cmd === "save_settings") {
      const patch = (args || {}).settings || {};
      Object.assign(state.settings, patch);
      // The real backend rebuilds the whole catalogue on every get_state, so mirror that
      // here. Without it the "Default" badge in the preview would never move and the
      // preview would disagree with the app.
      state.engines = state.engines.map(e =>
        e.id === "apple" ? Object.assign({}, e, { selected_voice: patch.voice || "" })
        : e.id === "kokoro" ? Object.assign({}, e, { selected_voice: (patch.kokoro || {}).voice || e.selected_voice })
        : e.id === "chatterbox" ? Object.assign({}, e, { selected_voice: (patch.chatterbox || {}).voice || e.selected_voice })
        : e);
      return state;
    }
    // Remember the callback Tauri would call, so a fake download can post progress at it.
    if (cmd === "plugin:event|listen") {
      handlers[(args || {}).event] = (args || {}).handler;
      return 1;
    }
    if (cmd === "plugin:event|unlisten") return 1;
    // Add/delete a reference voice. The copy is Rust's job in the app; here it only has to
    // move the catalogue so the list and the Delete buttons can be looked at.
    if (cmd === "add_chatterbox_voice" || cmd === "delete_chatterbox_voice") {
      const engine = state.engines.find(e => e.id === "chatterbox");
      const existing = (engine && engine.ref_voices) || [];
      const added = (args || {}).path ? String((args || {}).path).split("/").pop() : "clip.wav";
      const voices = cmd === "add_chatterbox_voice"
        ? existing.concat([{ id: added, label: added, note: "6.1 s of speech", builtin: false }])
        : existing.filter(v => v.id !== (args || {}).file);
      state.engines = state.engines.map(e => e.id === "chatterbox"
        ? Object.assign({}, e, { ref_voices: voices }) : e);
      return state;
    }
    // Drives the real progress rendering with a scripted download, then flips the
    // catalogue the same way the backend does once the weights are on disk.
    if (cmd === "install_engine") {
      const engine = (args || {}).engine;
      const info = state.engines.find(e => e.id === engine) || {};
      const handler = handlers["kiegen:install"];
      // Mirror the backend: Kokoro and Chatterbox are both plain file sets the app fetches
      // itself now, so either one can be scripted here.
      if (engine !== "kokoro" && engine !== "chatterbox") {
        if (handler !== undefined) {
          const message = (info.label || engine) + " has no weights the app installs";
          window["_" + handler]({ event: "kiegen:install", id: 0, payload: {
            engine, phase: "error", file: "", done: 0, total: 0, message } });
        }
        return null;
      }
      const total = info.download_bytes || 1;
      const file = "onnx/model.onnx";
      let done = 0;
      const tick = () => {
        done = Math.min(total, done + total / 10);
        const finished = done >= total;
        if (handler !== undefined) {
          window["_" + handler]({ event: "kiegen:install", id: 0, payload: {
            engine, phase: finished ? "done" : "downloading", file,
            done: Math.round(done), total, message: null } });
        }
        if (!finished) { setTimeout(tick, 140); return; }
        state.engines = state.engines.map(e => e.id === engine
          ? Object.assign({}, e, { needs_download: false, download_bytes: 0 }) : e);
      };
      tick();
      return null;
    }
    return null;
  },
  transformCallback: (cb) => { const id = Math.floor(Math.random() * 1e9); window["_" + id] = cb; return id; },
  unregisterCallback: () => {},
  convertFileSrc: (p) => p
};
window.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} };
""" % (json.dumps(voices, ensure_ascii=False), json.dumps(engines),
       json.dumps(system_language))

(preview / "index.html").write_text(
    '<!doctype html>\n<html lang="en"><head><meta charset="UTF-8">'
    '<title>TextHalo preview</title>\n'
    f'<link rel="stylesheet" href="{css}"><script>{stub}</script></head>\n'
    f'<body><div id="root"></div><script type="module" src="{js}"></script></body></html>'
)
print(f"harness ready: {len(voices)} Apple voices, {len(engines)} engines "
      f"({', '.join(e['id'] + ':' + str(len(e['voices'])) for e in engines)}) ({css}, {js})")
PY

echo "serving $PREVIEW on http://127.0.0.1:$PORT"
cd "$PREVIEW" && exec python3 -m http.server "$PORT"
