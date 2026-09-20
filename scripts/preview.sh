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

stub = """
const VOICES = %s;
const SYSTEM_LANGUAGE = %s;
const state = {
  settings: { shortcuts: { speak: "Cmd+Shift+S", stop: "Cmd+Shift+X" }, voice: null,
              rate: 200, capture_mode: "ax_then_copy", max_chars: 5000, restore_clipboard: true },
  voices: VOICES, trusted: false, secure_input: false, speaking: false,
  refused_shortcuts: [], system_language: SYSTEM_LANGUAGE,
  config_path: "/Users/home/Library/Application Support/com.kiegen.app/settings.json"
};
window.__TAURI_INTERNALS__ = {
  invoke: async (cmd, args) => {
    if (cmd === "get_state") return state;
    if (cmd === "save_settings") { Object.assign(state.settings, (args || {}).settings || {}); return state; }
    if (cmd === "plugin:event|listen" || cmd === "plugin:event|unlisten") return 1;
    return null;
  },
  transformCallback: (cb) => { const id = Math.floor(Math.random() * 1e9); window["_" + id] = cb; return id; },
  unregisterCallback: () => {},
  convertFileSrc: (p) => p
};
window.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} };
""" % (json.dumps(voices, ensure_ascii=False), json.dumps(system_language))

(preview / "index.html").write_text(
    '<!doctype html>\n<html lang="en"><head><meta charset="UTF-8">'
    '<title>kiegen preview</title>\n'
    f'<link rel="stylesheet" href="{css}"><script>{stub}</script></head>\n'
    f'<body><div id="root"></div><script type="module" src="{js}"></script></body></html>'
)
print(f"harness ready: {len(voices)} voices ({css}, {js})")
PY

echo "serving $PREVIEW on http://127.0.0.1:$PORT"
cd "$PREVIEW" && exec python3 -m http.server "$PORT"