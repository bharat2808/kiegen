import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import "./SpeechOverlay.css";

type Status = { phase: "idle" | "capturing" | "preparing" | "speaking" | "error"; message?: string | null };

export default function SpeechOverlay() {
  const [status, setStatus] = useState<Status>({ phase: "idle" });
  useEffect(() => {
    let mounted = true;
    const refresh = () => invoke<Status>("get_speech_status").then((next) => {
      if (mounted) setStatus(next);
    }).catch(() => {});
    const subscription = listen<Status>("kiegen:status", ({ payload }) => setStatus(payload));
    void refresh();
    // The panel is created hidden: a snapshot also covers events before it mounted.
    const timer = window.setInterval(() => void refresh(), 250);
    return () => { mounted = false; window.clearInterval(timer); void subscription.then((off) => off()); };
  }, []);
  if (status.phase === "idle") return <div className="overlay-root" />;
  const label = status.phase === "capturing" ? "Reading selection…"
    : status.phase === "preparing" ? "Preparing speech…"
    : status.phase === "error" ? "Unable to speak" : "Speaking";
  return (
    <div className={`overlay-root speech-overlay ${status.phase}`}>
      <div className="activity" aria-hidden="true">
        {status.phase === "error" ? "!" : [0, 1, 2, 3, 4].map((i) => <i key={i} style={{ animationDelay: `${i * -0.13}s` }} />)}
      </div>
      <span className="speech-label" role="status" title={status.message ?? label}>{label}</span>
      <button aria-label={status.phase === "error" ? "Dismiss speech error" : "Stop speech"}
        title={status.phase === "error" ? "Dismiss" : "Stop speech (⌘⇧X)"}
        onClick={() => void invoke("stop_speaking")}>
        <span />
      </button>
    </div>
  );
}
