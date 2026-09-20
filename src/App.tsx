import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

// Shapes mirror the serde structs in src-tauri/src/{config,lib}.rs
type CaptureMode = "ax_then_copy" | "ax_only" | "copy_only";

interface Shortcuts {
  speak: string;
  stop: string;
}

interface Settings {
  shortcuts: Shortcuts;
  voice: string | null;
  rate: number;
  capture_mode: CaptureMode;
  max_chars: number;
  restore_clipboard: boolean;
}

interface Voice {
  name: string;
  locale: string;
}

interface UiState {
  settings: Settings;
  voices: Voice[];
  trusted: boolean;
  secure_input: boolean;
  speaking: boolean;
  refused_shortcuts: string[];
}

interface StatusEvent {
  phase: "idle" | "capturing" | "speaking" | "error";
  message: string | null;
  chars: number | null;
}

const color = {
  text: "#e8e8ea",
  dim: "#9a9aa2",
  warn: "#ffb454",
  error: "#ff6b6b",
  ok: "#5dd39e",
  border: "#33333a",
  panel: "#1c1c20",
  accent: "#7aa2f7",
};

const box: React.CSSProperties = {
  border: `1px solid ${color.border}`,
  background: color.panel,
  borderRadius: 8,
  padding: 12,
  marginBottom: 12,
};

const rowStyle: React.CSSProperties = {
  display: "flex",
  alignItems: "center",
  gap: 10,
  marginBottom: 8,
};

const labelStyle: React.CSSProperties = {
  width: 130,
  color: color.dim,
  fontSize: 13,
  flexShrink: 0,
};

const inputStyle: React.CSSProperties = {
  background: "#121216",
  color: color.text,
  border: `1px solid ${color.border}`,
  borderRadius: 6,
  padding: "6px 8px",
  font: "inherit",
  flex: 1,
};

// ── accelerator recording ─────────────────────────────────────────────
// The recorder is ours because Tauri has no shortcut widget: capture keydown,
// normalise to global_hotkey syntax ("Cmd+Shift+S"), and refuse anything without a
// modifier (except function keys) — a bare letter would swallow normal typing
// system-wide.
const SIMPLE_KEYS: Record<string, string> = {
  Minus: "-",
  Equal: "=",
  BracketLeft: "[",
  BracketRight: "]",
  Backslash: "\\",
  Semicolon: ";",
  Quote: "'",
  Comma: ",",
  Period: ".",
  Slash: "/",
  Backquote: "`",
};

function codeToKey(code: string): string | null {
  if (/^Key[A-Z]$/.test(code)) return code.slice(3);
  if (/^Digit[0-9]$/.test(code)) return code.slice(5);
  if (/^F([1-9]|1[0-9]|2[0-4])$/.test(code)) return code;
  return SIMPLE_KEYS[code] ?? null;
}

function acceleratorFromEvent(event: React.KeyboardEvent): string | null {
  const key = codeToKey(event.code);
  if (!key) return null;
  const mods: string[] = [];
  if (event.metaKey) mods.push("Cmd");
  if (event.ctrlKey) mods.push("Ctrl");
  if (event.altKey) mods.push("Alt");
  if (event.shiftKey) mods.push("Shift");
  const isFunctionKey = /^F([1-9]|1[0-9]|2[0-4])$/.test(key);
  if (mods.length === 0 && !isFunctionKey) return null;
  return [...mods, key].join("+");
}

function ShortcutField({
  value,
  onChange,
}: {
  value: string;
  onChange: (next: string) => void;
}) {
  const [recording, setRecording] = useState(false);
  const [invalid, setInvalid] = useState(false);

  return (
    <button
      type="button"
      style={{
        ...inputStyle,
        textAlign: "left",
        cursor: "pointer",
        color: recording ? color.accent : color.text,
        borderColor: recording ? color.accent : color.border,
        flex: 1,
      }}
      onFocus={() => {
        setRecording(true);
        setInvalid(false);
      }}
      onBlur={() => setRecording(false)}
      onKeyDown={(event) => {
        event.preventDefault();
        if (event.key === "Escape") {
          setRecording(false);
          (event.target as HTMLElement).blur();
          return;
        }
        const accelerator = acceleratorFromEvent(event);
        if (!accelerator) {
          setInvalid(true);
          return;
        }
        onChange(accelerator);
        setRecording(false);
        (event.target as HTMLElement).blur();
      }}
    >
      {recording ? (invalid ? "need a modifier (e.g. Cmd+Shift+S)" : "press keys…") : value || "unset"}
    </button>
  );
}

// ── app ───────────────────────────────────────────────────────────────
function App() {
  const [state, setState] = useState<UiState | null>(null);
  const [status, setStatus] = useState<StatusEvent | null>(null);
  const [draft, setDraft] = useState<Settings | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [showAllVoices, setShowAllVoices] = useState(false);
  const pollRef = useRef<number | null>(null);

  async function refresh() {
    const next = await invoke<UiState>("get_state");
    setState(next);
    setDraft((current) => current ?? next.settings);
  }

  useEffect(() => {
    refresh();
    const unlisten = listen<StatusEvent>("kiegen:status", (event) => setStatus(event.payload));
    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  // Permission is granted outside the app (System Settings), so poll while the
  // window is open and stop once it lands.
  useEffect(() => {
    if (!state || state.trusted) {
      if (pollRef.current) window.clearInterval(pollRef.current);
      return;
    }
    pollRef.current = window.setInterval(async () => {
      const trusted = await invoke<boolean>("permission_status");
      if (trusted) setState((current) => (current ? { ...current, trusted } : current));
    }, 1500);
    return () => {
      if (pollRef.current) window.clearInterval(pollRef.current);
    };
  }, [state?.trusted]);

  if (!draft) return <main style={{ padding: 20, color: color.dim }}>loading…</main>;

  const patch = (values: Partial<Settings>) => setDraft({ ...draft, ...values });

  async function save() {
    if (!draft) return;
    setSaving(true);
    setNotice(null);
    try {
      const next = await invoke<UiState>("save_settings", { settings: draft });
      setState(next);
      setDraft(next.settings);
      setNotice(
        next.refused_shortcuts.length > 0
          ? `macOS refused ${next.refused_shortcuts.join(", ")} — another app probably owns it.`
          : "saved",
      );
    } catch (error) {
      setNotice(String(error));
    } finally {
      setSaving(false);
    }
  }

  const voices = (state?.voices ?? []).filter(
    (voice) => showAllVoices || voice.locale.toLowerCase().startsWith("en"),
  );

  return (
    <main
      style={{
        padding: 20,
        color: color.text,
        font: "13px/1.5 -apple-system, BlinkMacSystemFont, 'SF Pro Text', sans-serif",
      }}
    >
      <h1 style={{ fontSize: 17, margin: "0 0 4px" }}>kiegen</h1>
      <p style={{ margin: "0 0 16px", color: color.dim }}>
        Select text anywhere, press your shortcut, and it is read aloud. No window has to
        be open for that to work — this is only the settings panel.
      </p>

      {/* permission */}
      <section style={box}>
        <div style={{ ...rowStyle, marginBottom: 6 }}>
          <strong>Accessibility permission</strong>
          <span style={{ color: state?.trusted ? color.ok : color.warn }}>
            {state?.trusted ? "granted" : "required"}
          </span>
        </div>
        {state?.trusted ? (
          <p style={{ margin: 0, color: color.dim }}>
            kiegen can read the selection in the frontmost app. Nothing is sent anywhere.
          </p>
        ) : (
          <>
            <p style={{ margin: "0 0 8px", color: color.dim }}>
              macOS requires an explicit grant before any app may read another app's
              selection. Open the pane, switch kiegen on, then come back — this window
              detects it automatically. If a rebuild changed the app's signature you may
              have to remove and re-add it.
            </p>
            <button
              style={{ ...inputStyle, flex: "none", cursor: "pointer", padding: "6px 12px" }}
              onClick={() => invoke("open_accessibility_settings")}
            >
              Open System Settings → Accessibility
            </button>
          </>
        )}
        {state?.secure_input && (
          <p style={{ margin: "8px 0 0", color: color.warn }}>
            A secure input field is focused right now — capture will refuse until you leave
            it (this is the password-field safeguard).
          </p>
        )}
      </section>

      {/* shortcuts */}
      <section style={box}>
        <strong>Shortcuts</strong>
        <div style={{ ...rowStyle, marginTop: 10 }}>
          <span style={labelStyle}>Speak selection</span>
          <ShortcutField
            value={draft.shortcuts.speak}
            onChange={(speak) => patch({ shortcuts: { ...draft.shortcuts, speak } })}
          />
        </div>
        <div style={rowStyle}>
          <span style={labelStyle}>Stop</span>
          <ShortcutField
            value={draft.shortcuts.stop}
            onChange={(stop) => patch({ shortcuts: { ...draft.shortcuts, stop } })}
          />
        </div>
        <p style={{ margin: "4px 0 0", color: color.dim }}>
          Click a field and press the combo. At least one modifier is required, or the key
          would be swallowed system-wide.
        </p>
      </section>

      {/* voice */}
      <section style={box}>
        <strong>Voice</strong>
        <div style={{ ...rowStyle, marginTop: 10 }}>
          <span style={labelStyle}>Voice</span>
          <select
            style={inputStyle}
            value={draft.voice ?? ""}
            onChange={(event) => patch({ voice: event.target.value || null })}
          >
            <option value="">System default</option>
            {voices.map((voice) => (
              <option key={`${voice.name}-${voice.locale}`} value={voice.name}>
                {voice.name} — {voice.locale}
              </option>
            ))}
          </select>
        </div>
        <div style={rowStyle}>
          <span style={labelStyle} />
          <label style={{ color: color.dim }}>
            <input
              type="checkbox"
              checked={showAllVoices}
              onChange={(event) => setShowAllVoices(event.target.checked)}
            />{" "}
            show all {state?.voices.length ?? 0} languages
          </label>
        </div>
        <div style={rowStyle}>
          <span style={labelStyle}>Rate</span>
          <input
            type="range"
            min={80}
            max={400}
            value={draft.rate}
            onChange={(event) => patch({ rate: Number(event.target.value) })}
            style={{ flex: 1 }}
          />
          <span style={{ color: color.dim, width: 80, textAlign: "right" }}>
            {draft.rate} wpm
          </span>
        </div>
        <div style={rowStyle}>
          <span style={labelStyle} />
          <button
            style={{ ...inputStyle, flex: "none", cursor: "pointer", padding: "6px 12px" }}
            onClick={() =>
              invoke("speak_text", {
                text: "This is how kiegen will read your selection aloud.",
              })
            }
          >
            Preview voice
          </button>
          <button
            style={{ ...inputStyle, flex: "none", cursor: "pointer", padding: "6px 12px" }}
            onClick={() => invoke("stop_speaking")}
          >
            Stop
          </button>
        </div>
      </section>

      {/* capture */}
      <section style={box}>
        <strong>Reading the selection</strong>
        <div style={{ ...rowStyle, marginTop: 10 }}>
          <span style={labelStyle}>Method</span>
          <select
            style={inputStyle}
            value={draft.capture_mode}
            onChange={(event) => patch({ capture_mode: event.target.value as CaptureMode })}
          >
            <option value="ax_then_copy">
              Accessibility, then fall back to copying (recommended)
            </option>
            <option value="ax_only">Accessibility only — never touch my clipboard</option>
            <option value="copy_only">
              Copying only — for Chrome, Electron, terminals
            </option>
          </select>
        </div>
        <div style={rowStyle}>
          <span style={labelStyle}>Restore clipboard</span>
          <label style={{ color: color.dim }}>
            <input
              type="checkbox"
              checked={draft.restore_clipboard}
              onChange={(event) => patch({ restore_clipboard: event.target.checked })}
            />{" "}
            put back what was on the clipboard after a copy-mode capture
          </label>
        </div>
        <div style={rowStyle}>
          <span style={labelStyle}>Longest selection</span>
          <input
            type="number"
            min={100}
            max={100000}
            step={100}
            style={{ ...inputStyle, maxWidth: 120 }}
            value={draft.max_chars}
            onChange={(event) => patch({ max_chars: Number(event.target.value) })}
          />
          <span style={{ color: color.dim }}>characters, then truncated</span>
        </div>
      </section>

      <div style={{ ...rowStyle, gap: 10 }}>
        <button
          style={{
            ...inputStyle,
            flex: "none",
            cursor: "pointer",
            padding: "8px 16px",
            borderColor: color.accent,
            color: color.accent,
          }}
          onClick={save}
          disabled={saving}
        >
          {saving ? "saving…" : "Save"}
        </button>
        <button
          style={{ ...inputStyle, flex: "none", cursor: "pointer", padding: "8px 16px" }}
          onClick={() => invoke("speak_selection_now")}
        >
          Try it on my current selection
        </button>
      </div>

      {notice && <p style={{ color: color.ok }}>{notice}</p>}
      {status && (
        <p style={{ color: status.phase === "error" ? color.error : color.dim }}>
          {status.phase === "capturing" && "reading the selection…"}
          {status.phase === "speaking" &&
            `speaking ${status.chars ?? 0} characters${
              status.message ? ` (${status.message})` : ""
            }`}
          {status.phase === "idle" && "stopped"}
          {status.phase === "error" && `could not speak: ${status.message}`}
        </p>
      )}
      <p style={{ color: color.dim, fontSize: 12 }}>
        Settings are stored in the app config directory. Selections are never logged or
        cached.
      </p>
    </main>
  );
}

export default App;