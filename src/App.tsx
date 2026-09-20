import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import "./App.css";

/* ── types mirroring the Rust side ─────────────────────────────────── */

type CaptureMode = "ax_then_copy" | "ax_only" | "copy_only";

type Voice = { name: string; locale: string; novelty: boolean };

type Settings = {
  shortcuts: { speak: string; stop: string };
  voice: string | null;
  rate: number;
  capture_mode: CaptureMode;
  max_chars: number;
  restore_clipboard: boolean;
};

type UiState = {
  settings: Settings;
  voices: Voice[];
  trusted: boolean;
  secure_input: boolean;
  speaking: boolean;
  refused_shortcuts: string[];
  system_language: string;
  config_path: string;
};

type Phase = "idle" | "capturing" | "speaking" | "error";
type Status = { phase: Phase; message?: string | null; chars?: number | null };

type Tab = "general" | "voice" | "shortcuts" | "capture";

/* ── icons (16×16, currentColor) ───────────────────────────────────── */

/* Icons: Bootstrap Icons (MIT), 16×16 paths inlined to avoid a runtime dependency. */
function ico(paths: string[], size = 16) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="currentColor" aria-hidden="true">
      {paths.map((d, index) => (
        <path key={index} d={d} />
      ))}
    </svg>
  );
}

const Icon = {
  gear: () =>
    ico([
      "M8 4.754a3.246 3.246 0 1 0 0 6.492 3.246 3.246 0 0 0 0-6.492M5.754 8a2.246 2.246 0 1 1 4.492 0 2.246 2.246 0 0 1-4.492 0",
      "M9.796 1.343c-.527-1.79-3.065-1.79-3.592 0l-.094.319a.873.873 0 0 1-1.255.52l-.292-.16c-1.64-.892-3.433.902-2.54 2.541l.159.292a.873.873 0 0 1-.52 1.255l-.319.094c-1.79.527-1.79 3.065 0 3.592l.319.094a.873.873 0 0 1 .52 1.255l-.16.292c-.892 1.64.901 3.434 2.541 2.54l.292-.159a.873.873 0 0 1 1.255.52l.094.319c.527 1.79 3.065 1.79 3.592 0l.094-.319a.873.873 0 0 1 1.255-.52l.292.16c1.64.893 3.434-.902 2.54-2.541l-.159-.292a.873.873 0 0 1 .52-1.255l.319-.094c1.79-.527 1.79-3.065 0-3.592l-.319-.094a.873.873 0 0 1-.52-1.255l.16-.292c.893-1.64-.902-3.433-2.541-2.54l-.292.159a.873.873 0 0 1-1.255-.52zm-2.633.283c.246-.835 1.428-.835 1.674 0l.094.319a1.873 1.873 0 0 0 2.693 1.115l.291-.16c.764-.415 1.6.42 1.184 1.185l-.159.292a1.873 1.873 0 0 0 1.116 2.692l.318.094c.835.246.835 1.428 0 1.674l-.319.094a1.873 1.873 0 0 0-1.115 2.693l.16.291c.415.764-.42 1.6-1.185 1.184l-.291-.159a1.873 1.873 0 0 0-2.693 1.116l-.094.318c-.246.835-1.428.835-1.674 0l-.094-.319a1.873 1.873 0 0 0-2.692-1.115l-.292.16c-.764.415-1.6-.42-1.184-1.185l.159-.291A1.873 1.873 0 0 0 1.945 8.93l-.319-.094c-.835-.246-.835-1.428 0-1.674l.319-.094A1.873 1.873 0 0 0 3.06 4.377l-.16-.292c-.415-.764.42-1.6 1.185-1.184l.292.159a1.873 1.873 0 0 0 2.692-1.115z",
    ]),
  speaker: () =>
    ico([
      "M11.536 14.01A8.47 8.47 0 0 0 14.026 8a8.47 8.47 0 0 0-2.49-6.01l-.708.707A7.48 7.48 0 0 1 13.025 8c0 2.071-.84 3.946-2.197 5.303z",
      "M10.121 12.596A6.48 6.48 0 0 0 12.025 8a6.48 6.48 0 0 0-1.904-4.596l-.707.707A5.48 5.48 0 0 1 11.025 8a5.48 5.48 0 0 1-1.61 3.89z",
      "M10.025 8a4.5 4.5 0 0 1-1.318 3.182L8 10.475A3.5 3.5 0 0 0 9.025 8c0-.966-.392-1.841-1.025-2.475l.707-.707A4.5 4.5 0 0 1 10.025 8M7 4a.5.5 0 0 0-.812-.39L3.825 5.5H1.5A.5.5 0 0 0 1 6v4a.5.5 0 0 0 .5.5h2.325l2.363 1.89A.5.5 0 0 0 7 12zM4.312 6.39 6 5.04v5.92L4.312 9.61A.5.5 0 0 0 4 9.5H2v-3h2a.5.5 0 0 0 .312-.11",
    ]),
  command: () =>
    ico([
      "M3.5 2A1.5 1.5 0 0 1 5 3.5V5H3.5a1.5 1.5 0 1 1 0-3M6 5V3.5A2.5 2.5 0 1 0 3.5 6H5v4H3.5A2.5 2.5 0 1 0 6 12.5V11h4v1.5a2.5 2.5 0 1 0 2.5-2.5H11V6h1.5A2.5 2.5 0 1 0 10 3.5V5zm4 1v4H6V6zm1-1V3.5A1.5 1.5 0 1 1 12.5 5zm0 6h1.5a1.5 1.5 0 1 1-1.5 1.5zm-6 0v1.5A1.5 1.5 0 1 1 3.5 11z",
    ]),
  textCursor: () =>
    ico([
      "M5 2a.5.5 0 0 1 .5-.5c.862 0 1.573.287 2.06.566.174.099.321.198.44.286.119-.088.266-.187.44-.286A4.17 4.17 0 0 1 10.5 1.5a.5.5 0 0 1 0 1c-.638 0-1.177.213-1.564.434a3.5 3.5 0 0 0-.436.294V7.5H9a.5.5 0 0 1 0 1h-.5v4.272c.1.08.248.187.436.294.387.221.926.434 1.564.434a.5.5 0 0 1 0 1 4.17 4.17 0 0 1-2.06-.566A5 5 0 0 1 8 13.65a5 5 0 0 1-.44.285 4.17 4.17 0 0 1-2.06.566.5.5 0 0 1 0-1c.638 0 1.177-.213 1.564-.434.188-.107.335-.214.436-.294V8.5H7a.5.5 0 0 1 0-1h.5V3.228a3.5 3.5 0 0 0-.436-.294A3.17 3.17 0 0 0 5.5 2.5.5.5 0 0 1 5 2m2.648 10.645",
    ]),
  checkCircle: () =>
    ico([
      "M8 15A7 7 0 1 1 8 1a7 7 0 0 1 0 14m0 1A8 8 0 1 0 8 0a8 8 0 0 0 0 16",
      "m10.97 4.97-.02.022-3.473 4.425-2.093-2.094a.75.75 0 0 0-1.06 1.06L6.97 11.03a.75.75 0 0 0 1.079-.02l3.992-4.99a.75.75 0 0 0-1.071-1.05",
    ]),
  circle: () => ico(["M8 15A7 7 0 1 1 8 1a7 7 0 0 1 0 14m0 1A8 8 0 1 0 8 0a8 8 0 0 0 0 16"]),
  warn: () =>
    ico([
      "M7.938 2.016A.13.13 0 0 1 8.002 2a.13.13 0 0 1 .063.016.15.15 0 0 1 .054.057l6.857 11.667c.036.06.035.124.002.183a.2.2 0 0 1-.054.06.1.1 0 0 1-.066.017H1.146a.1.1 0 0 1-.066-.017.2.2 0 0 1-.054-.06.18.18 0 0 1 .002-.183L7.884 2.073a.15.15 0 0 1 .054-.057m1.044-.45a1.13 1.13 0 0 0-1.96 0L.165 13.233c-.457.778.091 1.767.98 1.767h13.713c.889 0 1.438-.99.98-1.767z",
      "M7.002 12a1 1 0 1 1 2 0 1 1 0 0 1-2 0M7.1 5.995a.905.905 0 1 1 1.8 0l-.35 3.507a.552.552 0 0 1-1.1 0z",
    ]),
  xCircle: () =>
    ico([
      "M8 15A7 7 0 1 1 8 1a7 7 0 0 1 0 14m0 1A8 8 0 1 0 8 0a8 8 0 0 0 0 16",
      "M4.646 4.646a.5.5 0 0 1 .708 0L8 7.293l2.646-2.647a.5.5 0 0 1 .708.708L8.707 8l2.647 2.646a.5.5 0 0 1-.708.708L8 8.707l-2.646 2.647a.5.5 0 0 1-.708-.708L7.293 8 4.646 5.354a.5.5 0 0 1 0-.708",
    ]),
  info: () =>
    ico([
      "M8 15A7 7 0 1 1 8 1a7 7 0 0 1 0 14m0 1A8 8 0 1 0 8 0a8 8 0 0 0 0 16",
      "m8.93 6.588-2.29.287-.082.38.45.083c.294.07.352.176.288.469l-.738 3.468c-.194.897.105 1.319.808 1.319.545 0 1.178-.252 1.465-.598l.088-.416c-.2.176-.492.246-.686.246-.275 0-.375-.193-.304-.533zM9 4.5a1 1 0 1 1-2 0 1 1 0 0 1 2 0",
    ]),
  keyboard: () =>
    ico([
      "M14 5a1 1 0 0 1 1 1v5a1 1 0 0 1-1 1H2a1 1 0 0 1-1-1V6a1 1 0 0 1 1-1zM2 4a2 2 0 0 0-2 2v5a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V6a2 2 0 0 0-2-2z",
      "M13 10.25a.25.25 0 0 1 .25-.25h.5a.25.25 0 0 1 .25.25v.5a.25.25 0 0 1-.25.25h-.5a.25.25 0 0 1-.25-.25zm0-2a.25.25 0 0 1 .25-.25h.5a.25.25 0 0 1 .25.25v.5a.25.25 0 0 1-.25.25h-.5a.25.25 0 0 1-.25-.25zm-5 0A.25.25 0 0 1 8.25 8h.5a.25.25 0 0 1 .25.25v.5a.25.25 0 0 1-.25.25h-.5A.25.25 0 0 1 8 8.75zm2 0a.25.25 0 0 1 .25-.25h1.5a.25.25 0 0 1 .25.25v.5a.25.25 0 0 1-.25.25h-1.5a.25.25 0 0 1-.25-.25zm1 2a.25.25 0 0 1 .25-.25h.5a.25.25 0 0 1 .25.25v.5a.25.25 0 0 1-.25.25h-.5a.25.25 0 0 1-.25-.25zm-5-2A.25.25 0 0 1 6.25 8h.5a.25.25 0 0 1 .25.25v.5a.25.25 0 0 1-.25.25h-.5A.25.25 0 0 1 6 8.75zm-2 0A.25.25 0 0 1 4.25 8h.5a.25.25 0 0 1 .25.25v.5a.25.25 0 0 1-.25.25h-.5A.25.25 0 0 1 4 8.75zm-2 0A.25.25 0 0 1 2.25 8h.5a.25.25 0 0 1 .25.25v.5a.25.25 0 0 1-.25.25h-.5A.25.25 0 0 1 2 8.75zm11-2a.25.25 0 0 1 .25-.25h.5a.25.25 0 0 1 .25.25v.5a.25.25 0 0 1-.25.25h-.5a.25.25 0 0 1-.25-.25zm-2 0a.25.25 0 0 1 .25-.25h.5a.25.25 0 0 1 .25.25v.5a.25.25 0 0 1-.25.25h-.5a.25.25 0 0 1-.25-.25zm-2 0A.25.25 0 0 1 9.25 6h.5a.25.25 0 0 1 .25.25v.5a.25.25 0 0 1-.25.25h-.5A.25.25 0 0 1 9 6.75zm-2 0A.25.25 0 0 1 7.25 6h.5a.25.25 0 0 1 .25.25v.5a.25.25 0 0 1-.25.25h-.5A.25.25 0 0 1 7 6.75zm-2 0A.25.25 0 0 1 5.25 6h.5a.25.25 0 0 1 .25.25v.5a.25.25 0 0 1-.25.25h-.5A.25.25 0 0 1 5 6.75zm-3 0A.25.25 0 0 1 2.25 6h1.5a.25.25 0 0 1 .25.25v.5a.25.25 0 0 1-.25.25h-1.5A.25.25 0 0 1 2 6.75zm0 4a.25.25 0 0 1 .25-.25h.5a.25.25 0 0 1 .25.25v.5a.25.25 0 0 1-.25.25h-.5a.25.25 0 0 1-.25-.25zm2 0a.25.25 0 0 1 .25-.25h5.5a.25.25 0 0 1 .25.25v.5a.25.25 0 0 1-.25.25h-5.5a.25.25 0 0 1-.25-.25z",
    ]),
  play: () =>
    ico([
      "m11.596 8.697-6.363 3.692c-.54.313-1.233-.066-1.233-.697V4.308c0-.63.692-1.01 1.233-.696l6.363 3.692a.802.802 0 0 1 0 1.393",
    ]),
  gauge: () =>
    ico([
      "M8 4a.5.5 0 0 1 .5.5V6a.5.5 0 0 1-1 0V4.5A.5.5 0 0 1 8 4M3.732 5.732a.5.5 0 0 1 .707 0l.915.914a.5.5 0 1 1-.708.708l-.914-.915a.5.5 0 0 1 0-.707M2 10a.5.5 0 0 1 .5-.5h1.586a.5.5 0 0 1 0 1H2.5A.5.5 0 0 1 2 10m9.5 0a.5.5 0 0 1 .5-.5h1.5a.5.5 0 0 1 0 1H12a.5.5 0 0 1-.5-.5m.754-4.246a.39.39 0 0 0-.527-.02L7.547 9.31a.91.91 0 1 0 1.302 1.258l3.434-4.297a.39.39 0 0 0-.029-.518z",
      "M0 10a8 8 0 1 1 15.547 2.661c-.442 1.253-1.845 1.602-2.932 1.25C11.309 13.488 9.475 13 8 13c-1.474 0-3.31.488-4.615.911-1.087.352-2.49.003-2.932-1.25A8 8 0 0 1 0 10m8-7a7 7 0 0 0-6.603 9.329c.203.575.923.876 1.68.63C4.397 12.533 6.358 12 8 12s3.604.532 4.923.96c.757.245 1.477-.056 1.68-.631A7 7 0 0 0 8 3",
    ]),
  box: () =>
    ico([
      "M8.186 1.113a.5.5 0 0 0-.372 0L1.846 3.5l2.404.961L10.404 2zm3.564 1.426L5.596 5 8 5.961 14.154 3.5zm3.25 1.7-6.5 2.6v7.922l6.5-2.6V4.24zM7.5 14.762V6.838L1 4.239v7.923zM7.443.184a1.5 1.5 0 0 1 1.114 0l7.129 2.852A.5.5 0 0 1 16 3.5v8.662a1 1 0 0 1-.629.928l-7.185 2.874a.5.5 0 0 1-.372 0L.63 13.09a1 1 0 0 1-.63-.928V3.5a.5.5 0 0 1 .314-.464z",
    ]),
};

/* ── language naming ───────────────────────────────────────────────── */

const LANGUAGE_NAMES: Record<string, string> = {
  en_US: "English (United States)",
  en_GB: "English (United Kingdom)",
  en_CA: "English (Canada)",
  en_AU: "English (Australia)",
  en_IE: "English (Ireland)",
  en_IN: "English (India)",
  en_ZA: "English (South Africa)",
  fr_FR: "French (France)",
  fr_CA: "French (Canada)",
  es_ES: "Spanish (Spain)",
  es_MX: "Spanish (Mexico)",
  pt_BR: "Portuguese (Brazil)",
  pt_PT: "Portuguese (Portugal)",
  de_DE: "German",
  it_IT: "Italian",
  nl_NL: "Dutch",
  nl_BE: "Dutch (Belgium)",
  sv_SE: "Swedish",
  nb_NO: "Norwegian",
  da_DK: "Danish",
  fi_FI: "Finnish",
  pl_PL: "Polish",
  cs_CZ: "Czech",
  sk_SK: "Slovak",
  sl_SI: "Slovenian",
  hr_HR: "Croatian",
  hu_HU: "Hungarian",
  ro_RO: "Romanian",
  bg_BG: "Bulgarian",
  el_GR: "Greek",
  tr_TR: "Turkish",
  uk_UA: "Ukrainian",
  ru_RU: "Russian",
  lt_LT: "Lithuanian",
  ca_ES: "Catalan",
  he_IL: "Hebrew",
  hi_IN: "Hindi",
  bn_IN: "Bengali",
  ta_IN: "Tamil",
  te_IN: "Telugu",
  kn_IN: "Kannada",
  th_TH: "Thai",
  vi_VN: "Vietnamese",
  id_ID: "Indonesian",
  ms_MY: "Malay",
  kk_KZ: "Kazakh",
  ar_001: "Arabic",
  ar_SA: "Arabic (Saudi Arabia)",
  zh_CN: "Chinese (China)",
  zh_TW: "Chinese (Taiwan)",
  zh_HK: "Chinese (Hong Kong)",
  ja_JP: "Japanese",
  ko_KR: "Korean",
};

const LANGUAGE_FAMILIES: Record<string, string> = {
  en: "English",
  fr: "French",
  es: "Spanish",
  pt: "Portuguese",
  de: "German",
  it: "Italian",
  nl: "Dutch",
  zh: "Chinese",
  ja: "Japanese",
  ko: "Korean",
  ru: "Russian",
  hi: "Hindi",
};

const languageName = (locale: string) => LANGUAGE_NAMES[locale] ?? locale.replace("_", " ");

/** `Eddy (English (US))` → `Eddy`: the locale column already says the language. */
const voiceLabel = (name: string) => name.split(" (")[0].trim();

/* ── shortcut capture ──────────────────────────────────────────────── */

const NAMED_KEYS: Record<string, string> = {
  Backquote: "Backquote",
  Backslash: "Backslash",
  BracketLeft: "BracketLeft",
  BracketRight: "BracketRight",
  Comma: "Comma",
  Equal: "Equal",
  Minus: "Minus",
  Period: "Period",
  Quote: "Quote",
  Semicolon: "Semicolon",
  Slash: "Slash",
  Space: "Space",
  Tab: "Tab",
  Enter: "Enter",
  Backspace: "Backspace",
  Delete: "Delete",
  Home: "Home",
  End: "End",
  PageUp: "PageUp",
  PageDown: "PageDown",
  Insert: "Insert",
  ArrowUp: "Up",
  ArrowDown: "Down",
  ArrowLeft: "Left",
  ArrowRight: "Right",
};

/**
 * Build an accelerator in `global_hotkey` syntax from a DOM event. The vocabulary is
 * that crate's `parse_key`/`parse_hotkey` — bare `S`/`1`/`F5`, arrows as `Up`/`Down` —
 * not the browser's `event.key`. Combinations need a modifier, or the chord would
 * swallow ordinary typing system-wide; function keys are allowed bare.
 */
function acceleratorFromEvent(event: KeyboardEvent): string | null {
  const key = (() => {
    if (NAMED_KEYS[event.code]) return NAMED_KEYS[event.code];
    const letter = /^Key([A-Z])$/.exec(event.code);
    if (letter) return letter[1];
    const digit = /^Digit([0-9])$/.exec(event.code);
    if (digit) return digit[1];
    if (/^F([1-9]|1[0-9]|2[0-4])$/.test(event.code)) return event.code;
    return null;
  })();
  if (!key) return null;

  const parts: string[] = [];
  if (event.metaKey) parts.push("Cmd");
  if (event.ctrlKey) parts.push("Ctrl");
  if (event.altKey) parts.push("Alt");
  if (event.shiftKey) parts.push("Shift");

  const isFunctionKey = /^F([1-9]|1[0-9]|2[0-4])$/.test(key);
  if (parts.length === 0 && !isFunctionKey) return null;

  parts.push(key);
  return parts.join("+");
}

/* ── shared components ─────────────────────────────────────────────── */

function Card({
  title,
  icon,
  children,
}: {
  title: string;
  icon: React.ReactNode;
  children: React.ReactNode;
}) {
  return (
    <div className="card">
      <div className="card-title">
        {icon}
        <span>{title}</span>
      </div>
      {children}
    </div>
  );
}

function Row({
  selected,
  glyph,
  title,
  subtitle,
  mono,
  badge,
  disabled,
  onSelect,
  trailing,
}: {
  selected: boolean;
  glyph: React.ReactNode;
  title: string;
  subtitle?: string;
  mono?: boolean;
  badge?: string;
  disabled?: boolean;
  onSelect: () => void;
  trailing?: React.ReactNode;
}) {
  return (
    <div
      className={selected ? "sel-row selected" : "sel-row"}
      role="button"
      tabIndex={disabled ? -1 : 0}
      aria-pressed={selected}
      onClick={disabled ? undefined : onSelect}
      onKeyDown={(event) => {
        if (event.key === "Enter" || event.key === " ") {
          event.preventDefault();
          onSelect();
        }
      }}
    >
      <span className="glyph">{glyph}</span>
      <span className="sel-text">
        <span className={mono ? "sel-title mono" : "sel-title"}>{title}</span>
        {subtitle ? <span className="sel-subtitle">{subtitle}</span> : null}
      </span>
      {badge ? <span className="sel-badge">{badge}</span> : null}
      {trailing}
    </div>
  );
}

function Note({
  kind,
  icon,
  children,
}: {
  kind: "error" | "warning" | "info" | "secondary";
  icon: React.ReactNode;
  children: React.ReactNode;
}) {
  return (
    <div className={`note-line ${kind}`}>
      {icon}
      <span>{children}</span>
    </div>
  );
}

/* ── app ───────────────────────────────────────────────────────────── */

export default function App() {
  const [state, setState] = useState<UiState | null>(null);
  const [tab, setTab] = useState<Tab>("general");
  const [status, setStatus] = useState<Status>({ phase: "idle" });
  const [error, setError] = useState<string | null>(null);
  const [language, setLanguage] = useState<string | null>(null);
  const [showNovelty, setShowNovelty] = useState(false);
  const [recording, setRecording] = useState<"speak" | "stop" | null>(null);
  const [rateDraft, setRateDraft] = useState<number | null>(null);
  const rateTimer = useRef<number | null>(null);

  const refresh = useCallback(async () => {
    try {
      const next = await invoke<UiState>("get_state");
      setState(next);
    } catch (e) {
      setError(String(e));
    }
  }, []);

  useEffect(() => {
    void refresh();
    const unlisten = listen<Status>("kiegen:status", (event) => setStatus(event.payload));
    // Permission is granted outside the app, and speech ends on its own: poll rather
    // than pretend we can observe either.
    const poll = window.setInterval(() => void refresh(), 2000);
    return () => {
      void unlisten.then((off) => off());
      window.clearInterval(poll);
    };
  }, [refresh]);

  const save = useCallback(
    async (patch: Partial<Settings>) => {
      const current = state;
      if (!current) return;
      const next: Settings = { ...current.settings, ...patch };
      setState({ ...current, settings: next }); // optimistic: a click must not lag
      try {
        const updated = await invoke<UiState>("save_settings", { settings: next });
        setState(updated);
        setError(null);
      } catch (e) {
        setError(String(e));
        void refresh();
      }
    },
    [state, refresh],
  );

  /* Voice + language derivation. */
  const voices = state?.voices ?? [];
  const byLanguage = useMemo(() => {
    const map = new Map<string, Voice[]>();
    for (const voice of voices) {
      const list = map.get(voice.locale) ?? [];
      list.push(voice);
      map.set(voice.locale, list);
    }
    for (const list of map.values()) {
      list.sort(
        (a, b) => Number(a.novelty) - Number(b.novelty) || a.name.localeCompare(b.name),
      );
    }
    return map;
  }, [voices]);

  const systemLanguage = state?.system_language ?? "en_US";
  const family = systemLanguage.split("_")[0];

  /**
   * Open on the user's own language. There is frequently no voice for the exact locale
   * — this machine reports `en_CA` and macOS ships no Canadian English voice — so fall
   * back to the same language family, largest first, and say so in the UI.
   */
  const defaultLanguage = useMemo(() => {
    if (voices.length === 0) return null;
    if (byLanguage.has(systemLanguage)) return systemLanguage;
    const siblings = [...byLanguage.keys()].filter((locale) =>
      locale.startsWith(`${family}_`),
    );
    if (siblings.length === 0) return null;
    return siblings.sort(
      (a, b) => (byLanguage.get(b)?.length ?? 0) - (byLanguage.get(a)?.length ?? 0),
    )[0];
  }, [voices.length, byLanguage, systemLanguage, family]);

  useEffect(() => {
    if (language === null && defaultLanguage) setLanguage(defaultLanguage);
  }, [defaultLanguage, language]);

  const activeLanguage = language ?? defaultLanguage;
  const activeVoices = activeLanguage ? (byLanguage.get(activeLanguage) ?? []) : voices;
  const speechVoices = activeVoices.filter((voice) => !voice.novelty);
  const noveltyVoices = activeVoices.filter((voice) => voice.novelty);
  const siblingLocales = [...byLanguage.keys()]
    .filter((locale) => locale.startsWith(`${family}_`) && locale !== activeLanguage)
    .sort((a, b) => (byLanguage.get(b)?.length ?? 0) - (byLanguage.get(a)?.length ?? 0));
  const exactLocaleMissing = voices.length > 0 && !byLanguage.has(systemLanguage);

  /* Keyboard recording for the shortcut rows. */
  useEffect(() => {
    if (!recording || !state) return;
    const shortcuts = state.settings.shortcuts;
    const onKey = (event: KeyboardEvent) => {
      event.preventDefault();
      if (event.key === "Escape") {
        setRecording(null);
        return;
      }
      const accelerator = acceleratorFromEvent(event);
      if (!accelerator) return;
      setRecording(null);
      void save({
        shortcuts:
          recording === "speak"
            ? { ...shortcuts, speak: accelerator }
            : { ...shortcuts, stop: accelerator },
      });
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [recording, save, state]);

  const preview = useCallback(
    (voice: Voice | null) => {
      if (!state) return;
      void invoke("preview_voice", {
        voice: voice ? voice.name : null,
        rate: state.settings.rate,
        text: null,
      });
    },
    [state],
  );

  if (!state) {
    return (
      <div className="settings">
        <div className="sidebar" />
        <div className="vertical-rule" />
        <div className="pane">
          <div className="pane-inner">
            <span className="card-note">Loading…</span>
          </div>
        </div>
      </div>
    );
  }

  const { settings } = state;
  const selectedVoice = voices.find((voice) => voice.name === settings.voice) ?? null;

  const statusLine =
    status.phase === "error"
      ? { kind: "error" as const, text: status.message ?? "failed" }
      : status.phase === "capturing"
        ? { kind: "info" as const, text: "Reading selection…" }
        : status.phase === "speaking"
          ? {
              kind: "info" as const,
              text: status.chars ? `Speaking ${status.chars} characters` : "Speaking…",
            }
          : { kind: "secondary" as const, text: "Idle" };

  const tabs: { id: Tab; title: string; icon: React.ReactNode }[] = [
    { id: "general", title: "General", icon: Icon.gear() },
    { id: "voice", title: "Voice", icon: Icon.speaker() },
    { id: "shortcuts", title: "Shortcuts", icon: Icon.command() },
    { id: "capture", title: "Capture", icon: Icon.textCursor() },
  ];

  const voiceRow = (voice: Voice) => (
    <Row
      key={voice.name}
      selected={settings.voice === voice.name}
      glyph={settings.voice === voice.name ? Icon.checkCircle() : Icon.circle()}
      title={voiceLabel(voice.name)}
      subtitle={voice.novelty ? "Not speech — a sound effect" : undefined}
      badge={settings.voice === voice.name ? "Default" : undefined}
      onSelect={() => void save({ voice: voice.name })}
      trailing={
        <button
          className="icon"
          title={`Preview ${voiceLabel(voice.name)}`}
          onClick={(event) => {
            event.stopPropagation();
            preview(voice);
          }}
        >
          {Icon.play()}
        </button>
      }
    />
  );

  return (
    <div className="settings">
      <div className="sidebar">
        {tabs.map((entry) => (
          <button
            key={entry.id}
            className={tab === entry.id ? "sidebar-row active" : "sidebar-row"}
            onClick={() => setTab(entry.id)}
          >
            {entry.icon}
            <span>{entry.title}</span>
          </button>
        ))}
        <div className="sidebar-spacer" />
        {error ? (
          <Note kind="error" icon={Icon.xCircle()}>
            <span className="truncate" title={error}>
              {error}
            </span>
          </Note>
        ) : null}
        <Note kind={statusLine.kind} icon={Icon.info()}>
          <span className="truncate">{statusLine.text}</span>
        </Note>
      </div>
      <div className="vertical-rule" />

      <div className="pane">
        {tab === "general" ? (
          <div className="pane-inner">
            <h1 className="pane-title">General</h1>
            <p className="pane-subtitle">
              Select text anywhere, press <span className="mono">{settings.shortcuts.speak}</span>{" "}
              and it is read aloud. Nothing is sent anywhere.
            </p>

            <Card title="Permission" icon={Icon.textCursor()}>
              {state.trusted ? (
                <Note kind="secondary" icon={Icon.checkCircle()}>
                  Accessibility access granted — kiegen can read the selection.
                </Note>
              ) : (
                <>
                  <Note kind="warning" icon={Icon.warn()}>
                    kiegen needs Accessibility access before it can read anything.
                  </Note>
                  <div className="inline">
                    <button
                      className="plain"
                      onClick={() => void invoke("open_accessibility_settings")}
                    >
                      Open System Settings…
                    </button>
                  </div>
                  <div className="card-note">
                    Switch <strong>kiegen</strong> on under Privacy &amp; Security →
                    Accessibility. This panel notices by itself once you do.
                  </div>
                  <div className="card-note">
                    Rebuilding from source invalidates the grant, and the stale entry keeps
                    failing. Clear it with:
                  </div>
                  <div className="mono-block">tccutil reset Accessibility com.kiegen.app</div>
                </>
              )}
              {state.secure_input ? (
                <Note kind="warning" icon={Icon.warn()}>
                  Secure input is on — a password field has focus. Capture refuses until you
                  click away.
                </Note>
              ) : null}
            </Card>

            <Card title="Try it" icon={Icon.play()}>
              <div className="inline">
                <button className="plain" onClick={() => void invoke("speak_selection_now")}>
                  Speak the current selection
                </button>
                <button className="plain" onClick={() => void invoke("stop_speaking")}>
                  Stop
                </button>
              </div>
              <div className="card-note">
                Select text in another app first, then press the button — same path the
                global shortcut takes.
              </div>
            </Card>

            <Card title="About" icon={Icon.info2()}>
              <div className="card-note">Version 0.1.0 · MIT OR Apache-2.0</div>
              <div className="field">
                <span className="field-label">Settings file</span>
                <div className="mono-block">{state.config_path}</div>
              </div>
            </Card>
          </div>
        ) : null}

        {tab === "voice" ? (
          <div className="pane-inner">
            <h1 className="pane-title">Voice</h1>
            <p className="pane-subtitle">
              The voice kiegen reads with. {voices.length} installed on this Mac.
            </p>

            <Card title="Spoken voice" icon={Icon.speaker()}>
              <div className="field">
                <span className="field-label">Language</span>
                <select
                  value={activeLanguage ?? "all"}
                  onChange={(event) =>
                    setLanguage(event.target.value === "all" ? null : event.target.value)
                  }
                >
                  <option value="all">All languages ({voices.length})</option>
                  {[...byLanguage.entries()]
                    .sort((a, b) => languageName(a[0]).localeCompare(languageName(b[0])))
                    .map(([locale, list]) => (
                      <option key={locale} value={locale}>
                        {languageName(locale)} — {list.length}
                        {locale === systemLanguage ? " · your language" : ""}
                      </option>
                    ))}
                </select>
                <span className="field-hint">
                  This Mac is set to {languageName(systemLanguage)}.
                </span>
              </div>

              {exactLocaleMissing && activeLanguage ? (
                <>
                  <Note kind="warning" icon={Icon.warn()}>
                    No {languageName(systemLanguage)} voice is installed, so this shows{" "}
                    {languageName(activeLanguage)} instead.
                  </Note>
                  {siblingLocales.length > 0 ? (
                    <div className="field">
                      <span className="field-label">
                        Other {LANGUAGE_FAMILIES[family] ?? family} regions
                      </span>
                      <div className="chip-row">
                        {siblingLocales.map((locale) => (
                          <button
                            key={locale}
                            className="chip"
                            title={`${byLanguage.get(locale)?.length ?? 0} voices`}
                            onClick={() => setLanguage(locale)}
                          >
                            {languageName(locale).replace(/^[^(]*\(|\)$/g, "")}
                          </button>
                        ))}
                      </div>
                    </div>
                  ) : null}
                </>
              ) : null}

              <div className="voice-list">
                {activeLanguage === null ? (
                  // "All languages" is the one view where a flat list is wrong.
                  [...byLanguage.entries()]
                    .sort((a, b) => languageName(a[0]).localeCompare(languageName(b[0])))
                    .map(([locale, list]) => {
                      const shown = list.filter((voice) => showNovelty || !voice.novelty);
                      if (shown.length === 0) return null;
                      return (
                        <div key={locale}>
                          <div className="group-heading">{languageName(locale)}</div>
                          <div className="row-stack" style={{ marginTop: 6 }}>
                            {shown.map(voiceRow)}
                          </div>
                        </div>
                      );
                    })
                ) : (
                  <>
                    <Row
                      selected={settings.voice === null}
                      glyph={settings.voice === null ? Icon.checkCircle() : Icon.circle()}
                      title="System default"
                      subtitle="Whatever macOS is set to"
                      badge={settings.voice === null ? "Default" : undefined}
                      onSelect={() => void save({ voice: null })}
                    />
                    <div className="row-stack" style={{ marginTop: 6 }}>
                      {speechVoices.map(voiceRow)}
                      {showNovelty && noveltyVoices.length > 0 ? (
                        <>
                          <div className="group-heading">Novelty / sound effects</div>
                          {noveltyVoices.map(voiceRow)}
                        </>
                      ) : null}
                      {speechVoices.length === 0 && !showNovelty ? (
                        <span className="card-note">
                          No speaking voices in this language — only sound effects.
                        </span>
                      ) : null}
                    </div>
                  </>
                )}
              </div>

              <div className="inline">
                <button className="plain" onClick={() => preview(selectedVoice)}>
                  {Icon.play()} Preview default
                </button>
                {noveltyVoices.length > 0 ? (
                  <button className="text" onClick={() => setShowNovelty(!showNovelty)}>
                    {showNovelty
                      ? "Hide novelty voices"
                      : `Show ${noveltyVoices.length} novelty / sound-effect voices`}
                  </button>
                ) : null}
              </div>
              <div className="card-note">
                {selectedVoice
                  ? `Default: ${voiceLabel(selectedVoice.name)} (${selectedVoice.locale})`
                  : "Default: system voice"}
              </div>
            </Card>

            <Card title="Speed" icon={Icon.command()}>
              <div className="inline">
                <input
                  type="range"
                  min={80}
                  max={500}
                  step={5}
                  value={rateDraft ?? settings.rate}
                  onChange={(event) => {
                    const value = Number(event.target.value);
                    setRateDraft(value);
                    if (rateTimer.current) window.clearTimeout(rateTimer.current);
                    // Dragging a slider must not write the config eighty times.
                    rateTimer.current = window.setTimeout(() => {
                      void save({ rate: value });
                      setRateDraft(null);
                    }, 350);
                  }}
                />
                <span className="mono">{rateDraft ?? settings.rate} wpm</span>
              </div>
              <div className="card-note">
                200 wpm is the default. Preview a voice to hear the difference.
              </div>
            </Card>
          </div>
        ) : null}

        {tab === "shortcuts" ? (
          <div className="pane-inner">
            <h1 className="pane-title">Shortcuts</h1>
            <p className="pane-subtitle">
              These work in every app. Speak plays the selection; Stop silences it.
            </p>

            <Card title="Global shortcuts" icon={Icon.command()}>
              {(["speak", "stop"] as const).map((role) => {
                const isRecording = recording === role;
                return (
                  <div className="field" key={role}>
                    <span className="field-label">
                      {role === "speak" ? "Speak the selection" : "Stop speaking"}
                    </span>
                    <Row
                      selected={!isRecording}
                      glyph={isRecording ? Icon.keyboard() : Icon.checkCircle()}
                      title={isRecording ? "Press a key combo…" : settings.shortcuts[role]}
                      subtitle={
                        isRecording
                          ? "Press Esc to cancel"
                          : role === "speak"
                            ? "Reads the selected text aloud"
                            : "Stops playback immediately"
                      }
                      mono={!isRecording}
                      onSelect={() => setRecording(role)}
                      trailing={
                        <button
                          className="plain"
                          onClick={(event) => {
                            event.stopPropagation();
                            setRecording(isRecording ? null : role);
                          }}
                        >
                          {isRecording ? "Cancel" : "Record…"}
                        </button>
                      }
                    />
                  </div>
                );
              })}

              {state.refused_shortcuts.length > 0 ? (
                <Note kind="error" icon={Icon.xCircle()}>
                  macOS refused {state.refused_shortcuts.join(", ")} — another app already
                  owns it. Pick a different chord.
                </Note>
              ) : null}
              <div className="card-note">
                A combination needs at least one modifier (⌘ ⌃ ⌥ ⇧); function keys may be
                used on their own.
              </div>
            </Card>
          </div>
        ) : null}

        {tab === "capture" ? (
          <div className="pane-inner">
            <h1 className="pane-title">Capture</h1>
            <p className="pane-subtitle">How kiegen gets hold of the text you selected.</p>

            <Card title="Capture method" icon={Icon.textCursor()}>
              <div className="row-stack">
                {(
                  [
                    {
                      mode: "ax_then_copy" as CaptureMode,
                      title: "Accessibility, then copy",
                      subtitle:
                        "Reads the selection directly, falling back to a simulated ⌘C when an app won't answer",
                    },
                    {
                      mode: "ax_only" as CaptureMode,
                      title: "Accessibility only",
                      subtitle: "Never touches the clipboard",
                    },
                    {
                      mode: "copy_only" as CaptureMode,
                      title: "Simulated ⌘C only",
                      subtitle:
                        "For apps with no usable accessibility tree — Chrome, Electron, some terminals",
                    },
                  ] as const
                ).map((option) => (
                  <Row
                    key={option.mode}
                    selected={settings.capture_mode === option.mode}
                    glyph={
                      settings.capture_mode === option.mode
                        ? Icon.checkCircle()
                        : Icon.circle()
                    }
                    title={option.title}
                    subtitle={option.subtitle}
                    onSelect={() => void save({ capture_mode: option.mode })}
                  />
                ))}
              </div>
            </Card>

            <Card title="Limits" icon={Icon.warn()}>
              <label className="toggle-row">
                <input
                  type="checkbox"
                  checked={settings.restore_clipboard}
                  onChange={(event) => void save({ restore_clipboard: event.target.checked })}
                />
                <span>
                  <span>Put the clipboard back after a copy-mode capture</span>
                  <span className="field-hint" style={{ display: "block" }}>
                    Without this, capturing overwrites whatever you had copied.
                  </span>
                </span>
              </label>
              <div className="field">
                <span className="field-label">Speak at most</span>
                <div className="inline">
                  <input
                    type="number"
                    min={100}
                    max={100000}
                    step={100}
                    value={settings.max_chars}
                    onChange={(event) => {
                      const value = Number(event.target.value);
                      if (Number.isFinite(value) && value >= 100) {
                        void save({ max_chars: Math.min(value, 100000) });
                      }
                    }}
                  />
                  <span className="card-note">characters per selection</span>
                </div>
                <span className="field-hint">
                  Guards against capturing a whole document by accident.
                </span>
              </div>
            </Card>
          </div>
        ) : null}
      </div>
    </div>
  );
}