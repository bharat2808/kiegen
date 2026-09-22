# kiegen — design notes

**One line:** a menu-bar agent that turns *any* text you have selected in *any* app into audio, with the shortcuts you choose; no window exists except the settings panel you open on purpose.

---

## 1. Does the idea make sense?

Yes, and it names an established category — this is a **macOS menu-bar agent / background utility** (PopClip, Raycast, Maccy, Shottr all live here). The shape is right:

- no Dock icon, no window at launch → `ActivationPolicy::Accessory` + `LSUIElement`
- a tray item is the app's entire persistent UI
- a settings window is created on demand and destroyed on close
- the "product" is a *service*: select text → chord → audio

What distinguishes it from the category: the input is the OS-wide selection rather than text you pasted into the app, and the output is locally generated speech (Kokoro, English-only unless you accept GPL espeak-ng — plus Chatterbox Multilingual's 23 languages — §5).

---

## 2. The crux: getting "the selected text" from another application

macOS gives no general-purpose API for this. There are three real mechanisms, and you should ship a primary with one fallback.

### A. Accessibility API (AXUIElement) — read, don't steal

```
AXUIElementCreateSystemWide()
  → copy kAXFocusedUIElementAttribute
  → copy kAXSelectedTextAttribute   (and kAXSelectedTextRangeAttribute)
```

- **Pros:** non-destructive — the user's selection and clipboard are untouched; instant; gives you the *focused app* and *window title* for free.
- **Cons:** requires the **Accessibility** TCC grant (user-facing, scary-looking). Not all apps implement `kAXSelectedText`: Cocoa/AppKit apps mostly do; Safari usually does; **Chrome/Electron, terminals, Java, and canvas-based web editors frequently return nothing**.

### B. Synthetic ⌘C + pasteboard read — the universal fallback

```
save pasteboard.items + changeCount
post CGEvent Cmd+C to the focused app (down + up)
wait for changeCount to change (poll ≤150 ms, else abort)
read pasteboard string → restore original items
```

- **Pros:** works nearly everywhere a human can copy from, terminals and Electron included.
- **Cons:** mutates the clipboard for ~100 ms (observable by clipboard managers — flag it in settings and offer "use copy mode only when needed"); still needs Accessibility (posting events to other apps is privileged); apps with no selection contribute nothing to the pasteboard, so you must detect "no copy happened" via `changeCount` and bail rather than reading stale content.
- **Never overwrite user clipboard content with multiple flavors** — snapshot `NSPasteboardItem[]` and restore all of them, or you break rich-text/image clipboards.

### C. macOS Services / Quick Actions (`NSServices` in Info.plist)

- **Pros:** *zero* permissions, the OS routes selected text to you; appears in right-click → Services and in your app's own menu.
- **Cons:** no programmatic shortcut binding inside your app — the user assigns the key in System Settings → Keyboard → Shortcuts → Services. Two-step to invoke. Fine as a **bonus entry point**, not the primary.

### Recommendation

**Hotkey-triggered, AX-first with ⌘C fallback**, plus a Services entry. Do *not* attempt PopClip-style "pop up automatically on mouse-up": that needs a global `NSEvent`/`CGEventTap` monitor, which adds the **Input Monitoring** permission on top of Accessibility and is the single biggest source of permission friction and OS-version breakage. Make auto-popup a later opt-in.

Capture must be **on-demand** (triggered by the chord), never polled — polling the focused app's AX tree burns CPU and is visible to other apps.

---

## 3. Permissions reality check (the main product risk)

| Capability | Permission | Notes |
|---|---|---|
| Read selection via AX | Accessibility (`AXIsProcessTrustedWithOptions`) | Deep-link to `x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility`; re-check trust at every invocation, not just launch |
| Post ⌘C | Accessibility | same grant |
| Global hotkeys | none (register via the OS) | OS-reserved combos fail — detect and report |
| Auto-popup on mouse-up | Input Monitoring | defer to v2 |
| Kokoro synthesis | none | fully local, no network at inference time |

Two hard consequences:

1. **Unsigned/adhoc dev builds lose TCC grants on every rebuild** (TCC keys on bundle ID + signature). Expect to re-add the app in System Settings repeatedly while developing — this is normal, not a bug. Use a stable signing identity early, or you will waste days.
2. **Mac App Store distribution is effectively off the table.** Sandboxed apps cannot drive other apps' UI, and there's no sandbox entitlement that makes AX-to-other-apps acceptable. Plan on **Developer ID + notarization + direct download**, and state the Accessibility requirement prominently on the download page so users don't think it's malware.

---

## 4. Architecture

```
┌───────────── kiegen.app (LSUIElement, no dock icon) ─────────────┐
│                                                                  │
│  macOS integration layer (Rust)                                  │
│    tray (TrayIconBuilder)  ·  global-shortcut  ·  autostart      │
│    selection capture: AX → pasteboard fallback                   │
│    secure-input guard  ·  frontmost-app lookup                    │
│                    │                                             │
│  core service (Rust, headless, the real app)                     │
│    command router → job queue → chunker → Kokoro → player        │
│                    │                                             │
│  settings UI (webview) — created on demand, hidden otherwise     │
└──────────────────────────────────────────────────────────────────┘
```

**Pipeline:** `Selection → normalize → profile resolution → cache lookup → chunk → synth → play (cancellable)`

**Job model** (single source of truth in Rust):

```
Job { id, text, source_app, profile_id, status: Queued|Synth|Playing|Done|Failed|Cancelled, error? }
```

Rules: at most **one playing job** — a new request cancels the current one (with a setting for queue-vs-interrupt); every job is cancellable including mid-synthesis; the tray icon animates while `Synth|Playing`.

**Config** lives in one JSON via `tauri-plugin-store`; the tray menu is regenerated from config on every change:

```jsonc
{
  "version": 1,
  "capture": { "mode": "ax_then_copy", "restoreClipboard": true, "maxChars": 5000 },
  "shortcuts": [
    { "accelerator": "CommandOrControl+Shift+S", "action": "speak",        "profile": "default" },
    { "accelerator": "CommandOrControl+Shift+D", "action": "speak_to_file","profile": "narration" },
    { "accelerator": "CommandOrControl+Shift+X", "action": "stop" }
  ],
  "profiles": [
    { "id": "default",   "engine": "kokoro", "voice": "af_heart", "speed": 1.0 },
    { "id": "narration", "engine": "kokoro", "voice": ["am_michael", "bm_george"], "mix": [0.7, 0.3], "speed": 0.92 }
  ],
  "engine": { "model": "kokoro-v1.0", "quant": "fp32", "keepWarm": true, "idleUnloadMinutes": 30 },
  "overrides": [ { "bundleId": "com.tinyspeck.slackmacgap", "profile": "default", "speed": 1.1 } ],
  "history": { "enabled": false }   // off by default: selections are sensitive
}
```

**Engine seam** — keep a trait so a cloud provider can be added later without touching the pipeline:

```rust
trait Synth {
    fn synth_stream(&self, chunks: Receiver<Chunk>, p: &Profile, out: AudioSink) -> Result<()>;
    fn cancel(&self, job: JobId);
}
```

- `KokoroSynth` → local ONNX engine (§5). Default: offline, free, private, no data leaves the machine.
- `SaySynth` → `/usr/bin/say`. Not the product, but a **degraded fallback** for the window between install and "model downloaded", and for anything Kokoro can't serve. It makes the app useful in the first five minutes after install.
- `CloudSynth` → HTTP → mp3 bytes (OpenAI / ElevenLabs / MiniMax) if ever wanted; emit progress to the tray HUD, back off on 429, fall back to Kokoro rather than failing silently.

**Content-addressed cache:** `sha256(text + voice/mix + speed + model_hash + quant)` → PCM under `~/Library/Caches/kiegen/`. Replaying the same paragraph is instant and free. LRU-cap it (e.g. 500 MB).

---

## 5. TTS engine: Kokoro-82M

Facts verified against HuggingFace, crates.io and npm:

| | |
|---|---|
| Model | `hexgrad/Kokoro-82M` v1.0, **Apache-2.0**, 82M params, 24 kHz mono, `kokoro-v1_0.pth` = 327 MB fp32, 11.6M downloads, #1 on TTS Arena in early 2026 |
| Voices | **54 voices across 9 language codes** (`a`/`b` American+British English, `e` Spanish, `f` French, `h` Hindi, `i` Italian, `j` Japanese, `p` Brazilian Portuguese, `z` Mandarin). ID = `<lang><f\|m>_<name>`: `af_heart`, `am_michael`, `bf_emma`, `bm_george`… |
| ONNX builds | `onnx-community/Kokoro-82M-v1.0-ONNX` (Apache-2.0): `model.onnx` 325 MB fp32 · `model_fp16` 163 MB · `model_q8f16` 86 MB · `model_quantized` 92 MB. **Each voice is only 0.5 MB** (~29 MB for all 54) |
| Hard limit | **~510 phoneme tokens per generation call** → sentence chunking is mandatory, not an optimization |
| Rust ONNX runtime | `ort` 2.0.0-rc.13 (verified on crates.io) |
| Rust end-to-end binding | `kokoro-tts` 0.3.3, Apache-2.0 (`mzdk100/kokoro`) — wraps `ort`, brings `cmudict-fast` for English G2P plus `jieba-rs`/`pinyin` for Chinese |
| Playback | `rodio` 0.22 — f32 PCM straight into a sink, no temp files |

### The real fork in the road: phonemization

Kokoro does not consume text — it consumes **IPA phonemes**. Something must do grapheme-to-phoneme first, and that choice drags in the entire dependency surface:

| Path | G2P | Runtime deps | Verdict |
|---|---|---|---|
| **Rust in-process, own pipeline**: `ort` + our own tokenizer/chunker/G2P | cmudict → IPA (en), pinyin/jieba (zh) | none | **the plan** — the crate is licence-blocked, so kiegen owns ~6 small modules instead |
| espeak-ng (subprocess) | espeak-ng | GPL-3.0 | **arm's-length only** under kiegen's permissive licence: user-installed, invoked as a subprocess, never linked, bundled, or vendored |
| misaki (Python) | trained G2P + espeak for out-of-dictionary words | Python 3.12, spacy, phonemizer-fork | best English quality, worst packaging: ships a Python runtime |
| `kokoro-js` sidecar | `phonemizer.js` (espeak-ng compiled to WASM) | Node + 100 MB+ | fastest to prototype, but two runtimes, and the espeak-ng licence question is unchanged |

**espeak-ng is GPL-3.0** (verified 2026-09). It is the licensing landmine in this design, and it is avoidable for English and Chinese — but **not** with the Rust crate as published, and **not** for five of the eight languages. Both caveats are documented in the two subsections below.

### Licence audit of the whole stack (verified)

| Component | Licence | Ships in the .app? |
|---|---|---|
| `hexgrad/Kokoro-82M` weights (HF) | **Apache-2.0** | downloaded, not bundled |
| `onnx-community/Kokoro-82M-v1.0-ONNX` | **Apache-2.0** | downloaded, not bundled |
| `hexgrad/kokoro` (reference impl) | Apache-2.0 | no |
| `hexgrad/misaki` (G2P) | Apache-2.0 | no (Python path) |
| `thewh1teagle/kokoro-onnx` | MIT | no |
| `mzdk100/kokoro` → `kokoro-tts` crate | Apache-2.0 crate — but its `build.rs` compiles **espeak-derived C** | yes, until forked |
| `cmudict-fast` (English G2P data) | MIT/Apache-2.0 | yes |
| `ort` (ONNX Runtime binding) | MIT OR Apache-2.0 | yes |
| `rodio` (playback) | MIT OR Apache-2.0 | yes |
| **`espeak-ng`** | **GPL-3.0** | **only if you take the espeak path** |

Read the `espeak` row carefully: it is the *only* copyleft item in the stack, it is **optional**, and `phonemizer.js` being Apache-2.0 does **not** launder the GPL wasm it wraps. Two caveats worth stating plainly: the `kokoro-tts` crate is young (35 stars, 0.3.x) *and* licence-blocked for kiegen, so it is out entirely — kiegen owns its pipeline — and none of this is legal advice; if kiegen is ever commercialised, have counsel read the shipping bill of materials.

**What Apache-2.0 asks of any app, closed or open:** ship the licence text and any `NOTICE` file, keep the copyright notices, state significant modifications if you fork, and don't use the licensor's trademarks as your own branding. No source disclosure, no royalty. The model author states it outright on the card: *"Kokoro has been deployed in numerous projects and commercial APIs. We welcome the deployment of the model in real use cases."*

**One obligation that is not Apache:** the Kokoro v1.0 training set included two **CC BY** corpora (Koniwa `tnc`, CC BY 3.0; SIWIS, CC BY 4.0), which the card documents under "Creative Commons Attribution". Attribute Kokoro and those datasets in an "Open-source licences" screen and you have covered it. Also note the card's own claim that training used "permissive/non-copyrighted audio" including synthetic audio from closed providers — that is the author's assertion, documented on the card, not something you can independently verify.

### Dropping espeak-ng: what it actually costs

Verified in `hexgrad/kokoro/kokoro/pipeline.py` — language routing is not uniform:

| lang_code | languages | G2P | espeak needed? |
|---|---|---|---|
| `a` / `b` | American / British English | `misaki.en.G2P(trf, unk='')` | **only as an OOV fallback**, and it degrades gracefully (`EspeakFallback not Enabled: OOD words will be skipped`) |
| `j` | Japanese | `misaki.ja.JAG2P()` (cutlet) | no |
| `z` | Mandarin | `misaki.zh.ZHG2P()` (pypinyin/jieba) | no |
| `e` `f` `h` `i` `p` | Spanish, French, Hindi, Italian, Portuguese | `espeak.EspeakG2P(language=…)` | **yes — nothing else exists** |

So the model supports 8 languages and 54 voices, but **5 of the 8 have no non-espeak text front-end** in the reference implementation. The "50+ languages" figure that circulates comes from espeak-ng's phoneme inventory — it is the same dependency, renamed. Choosing "no espeak-ng" means English + Mandarin (+ Japanese if you build or port a Rust G2P), full stop.

#### The espeak path, as built and as measured

espeak-ng is reached the only way it can be. `src/espeak.rs` finds the user's own install (`KIEGEN_ESPEAK_NG`, then an app-managed directory, then the Homebrew prefixes, then `PATH`) and runs **one subprocess per text chunk**, reading IPA from stdout. Nothing links `libespeak-ng`, and nothing is downloaded into the app: the pane's *Add N voices* button asks the user's own package manager (`brew install espeak-ng`) to install it, and with no package manager it opens upstream's install page rather than fetching a copy — because fetching one would make kiegen a distributor of GPL code.

A relocated copy is not a workable fallback, and this was measured rather than assumed: a Homebrew **bottle will not run outside its prefix**. The binary is linked against placeholders Homebrew rewrites only at install time:

```
@@HOMEBREW_CELLAR@@/espeak-ng/1.52.0/lib/libespeak-ng.1.dylib
@@HOMEBREW_PREFIX@@/opt/pcaudiolib/lib/libpcaudio.0.dylib
```

so "the app keeps its own copy" would mean reproducing Homebrew's placeholder rewriting for the binary *and* its dylib *and* fetching `pcaudiolib` as a second bottle. Using the install in place sidesteps all of it — and it is the licence-cleaner answer anyway.

Three upstream behaviours must be reproduced or the audio is quietly wrong. Each was read out of upstream's source and then confirmed against a real run, not inferred:

| Behaviour | Why the obvious subprocess gets it wrong |
|---|---|
| `--tie=^` | espeak writes a tie between the halves of one phoneme. phonemizer asks for `͡` (U+0361) and *rewrites* it to the caller's tie; misaki passes `^` and writes its table against `t^ʃ`→`ʧ`. A plain `--ipa` yields `tʃ` — matching nothing, so every affricate and diphthong reaches the model as characters it was never trained on. Confirmed by running both spellings. |
| punctuation chunking | `preserve_punctuation=True` is **not** an espeak setting. phonemizer splits the line at the marks, phonemizes the bare chunks, and re-inserts the marks by position — in Python. No CLI flag produces this, so `Punctuation.preserve`/`restore` are ported from **phonemizer-fork 3.3.2 as installed**, not from master: master added a decimal-separator exception this version does not have, and splitting `19,99` is a difference you can hear. |
| bracket shuffle | misaki swaps `«»`→curly quotes and `()`→`«»` before phonemizing, and back afterwards, so parentheses travel through the punctuation machinery above instead of being read as espeak clause markers. It is **not** symmetric: an `«` in the source leaves as `“`, because the reverse mapping only covers the brackets the shuffle itself introduced. |

**Measured parity against upstream's own espeak path** — `misaki.espeak.EspeakG2P` driven through phonemizer, pointed at the same Homebrew 1.52.0 library so both sides run the same build, over a 92-line corpus in all five languages:

> **91/92 lines byte-for-byte identical (98.9%)**

The corpus covers accented text, `¿¡`/`«»`/`;`, numbers as digits, and a decimal comma. The harness lives in `~/.hermes/cache/scratch/espeak_parity/` (`corpus.tsv`, `oracle.py`, `compare.py`, `probe.py`); it compares codepoint-by-codepoint and reports the first divergence, because a systematic off-by-one-character reads very differently from scattered noise.

The one line that differs is Portuguese `está` in final position: upstream's library path (`espeak_TextToPhonemes`, no synthesis) gives it secondary stress — `estˌa` — where the CLI's synthesis path gives `estˈa`. It is **not** state carry-over across calls, not call ordering, not the voice, and not a missing flag: every CLI spelling tried returns `estˈa`, including `--punct`, `-x`, `--stdout`, `--stdin` and several utterances in one process. It is also not general — `café`, `sofá`, `você`, `avô`, `Pará`, `Aracaju`, and pt-BR `está` in *medial* position all match exactly. So the price of using the binary instead of the library is one stress mark on one word, and it is inherent to the arm's-length boundary rather than a bug with a flag-shaped fix. If it ever needs fixing, the mechanism is a pronunciation override (`~/.config/kiegen/pronounce.json`), not another flag.

**What the catalogue does with it.** An espeak-backed voice is listed but not selectable until espeak-ng is found, and the reason names both the dependency and the licence (`Needs espeak-ng (GPL-3.0)`), so the state is never a mystery or a silent failure. Installing it flips all 13 voices usable without a restart, and adds their 13 style tables — ~6.8 MB — to the next download, because `kokoro_plan` uses the same availability rule the catalogue shows. Japanese and Mandarin stay unavailable in both states: no install fixes them, and the copy says so.

### `kokoro-tts` is not the clean build it looks like

The crate is Apache-2.0, but `build.rs` is:

```rust
const SRC: &str = "src/transcription/en_ipa.c";
cc::Build::new().file(SRC).compile("es");   // unconditional
```

and `src/transcription/en_ipa.c` (223 KB, ~3.6k lines) contains espeak-ng's internals: `LETTERGP_VOWEL2`, `N_HASH_DICT`, `INSTN_CONTINUE`, `phFLAGBIT_NONSYLLABIC`, `FLAG_FOUND_ATTRIBUTES`, genericised `Initialize()` / `TextToPhonemes()` entry points — with no licence header in the file. That reads as a derivative of espeak-ng (GPL-3.0), and it is compiled **whether or not** `use-cmudict` is enabled: the feature only switches the Rust caller and the embedded `dict/espeak.dict` blob, not the C.

**Independently re-verified against upstream source** (not taken on faith — fetched from `mzdk100/kokoro@master` and `espeak-ng@master`):

- `build.rs` is 160 bytes and compiles `src/transcription/en_ipa.c` through `cc::Build` with **no conditional**.
- That file is 222,875 bytes with **no licence header** (opens on `#ifdef _MSC_VER`).
- It contains espeak-ng's internal identifiers — `phFLAGBIT_NONSYLLABIC` ×2, `LETTERGP_VOWEL2` ×2, `N_HASH_DICT` ×4, `INSTN_CONTINUE` ×4, `FLAG_FOUND_ATTRIBUTES` ×2, `TextToPhonemes` ×1 — and the same identifiers live in espeak-ng's `src/libespeak-ng/phoneme.h` (34 matches) and `synthesize.h`. `espeak` and `SetUpPhonemeTable` appear zero times: it is a *genericised* extraction, not a link against espeak-ng.
- `dict/espeak.dict` (168 KB) ships in the tree alongside `dict/cmudict.dict` (3.6 MB) and `dict/pinyin.dict` (8.7 MB).
- **`use-cmudict` is not a default feature** (`[features] use-cmudict = ["cmudict-fast"]`, no `default = [...]`), so an unmodified `cargo add kokoro-tts` builds the **espeak-derived C path by default**.

Verdict: treat the crate as carrying GPL-3.0-derived code until the C is removed from your build.

**No-espeak options, given the decision:** the fork-and-strip plan below is now moot — kiegen does not depend on this crate at all. Recording it only because the reasoning explains *why* the crate is off-limits: `default-features` aside, enabling `use-cmudict` and deleting the `cc::Build` call plus the `#[cfg(not(feature = "use-cmudict"))]` branch in `g2p.rs` would remove the C from *your* build — but you would still be vendoring/maintaining a crate whose advertised licence does not match its contents, and that is exactly the kind of thing `cargo deny check licenses` cannot see. Owning six small modules is cheaper than owning that ambiguity.

**The English quality cost:** on the cmudict path, an out-of-dictionary word falls through to `letters_to_ipa()` — i.e. the letters get spelled out. "Kiegen" becomes "kˈA ˈI ˈi ʤˈi ˈi ˈɛn". That is survivable only with a pronunciation-override file (`~/.config/kiegen/pronounce.json`), which is worth building anyway for names and brand words.

### Building on Kokoro directly (what "Kokoro only" costs)

If kiegen depends on *no* third-party Kokoro wrapper — neither the Rust crate nor a Python package — then these are the components you own. **This is not an option any more: the crate is licence-blocked (see the licence decision below), so this is the plan.** The MIT reference implementation (`thewh1teagle/kokoro-onnx`, `src/kokoro_onnx/`) is the checklist, and it is worth mirroring module-for-module in Rust:

| Reference module | What it does | Rust equivalent |
|---|---|---|
| `tokenizer.py` | IPA string → token ids | map through the 115-symbol vocab in `tokenizer.json` |
| `session.py` | load ONNX, resolve execution providers, discover input dtypes | `ort` session + EP selection |
| `chunker.py` | split phonemes at the least disruptive boundary, balanced | port verbatim (algorithm below) |
| `trim.py` | silence trim on the head/tail of a segment (librosa `trim`, vendored, ISC-ish licence) | `trim_in_place` equivalent |
| `pauses.py` / `sliding.py` | inter-chunk silence, continuous-mode joining | small |
| `config.py` | `MAX_PHONEME_LENGTH = 510`, `SAMPLE_RATE = 24000` | constants |

**The tensor contract, verified from the reference implementation and the model's own files:**

- `input_ids` — `int64`, shape `(1, L)`, `L ≤ 510`
- `style` — `float32`, shape `(1, 256)`; the voice file is **510 style rows × 256 dims of little-endian f32** (0.5 MB per voice), and the row is chosen by *token count*: `voice[len(ids) * 256 : (len(ids)+1) * 256]`
- `speed` — `float32` scalar. **Pitfall: exports disagree** — v1.0 takes a float speed, some non-English exports take an int (called out in `kokoro-onnx/session.py`)
- output — `float32` mono at 24 kHz
- **Voice blending is just arithmetic**: `np.add(a_nicole * 0.5, a_michael * 0.5)` on the style vectors (from `examples/with_blending.py`). Weighted blend in the config maps directly onto this, no model involvement.
- `kokoro-onnx` also uses a single `voices-v1.0.bin` (~27 MB) rather than 54 separate 0.5 MB files — either fits the same loader.

**Chunking is the part that is easy to get wrong.** The reference algorithm (verified in `chunker.py`) cuts at sentence marks `.!?…`, then clause marks `,;:`, then whitespace, then mid-word — and then does something non-obvious: it **balances** the batches by binary-searching the smallest limit that still yields the same batch count. Filling every batch to 510 leaves a short final batch, and *a short batch is spoken at a different rate and loudness than its neighbours* — that audible seam is the artefact. On top of that, `pause_after()` inserts sentence- or clause-length silence between batches. Implement both, or long selections will sound like two different narrators.

**And note the reference still needs espeak-ng**: `config.py` carries an `EspeakConfig { lib_path, data_path }`, and the repo ships `examples/with_espeak_data.py` / `with_espeak_lib.py`. The MIT wrapper is not a route around the G2P problem — it is the same problem with a friendlier licence in front of it.

### Licence decision for kiegen: open source is the answer, *which* licence is the question

The app's own licence is the only lever that matters, because GPL compatibility is one-way (GPL-3.0 code may be combined with Apache-2.0/MIT, never the reverse). Two self-consistent configurations:

| | **Option 1 — GPL-3.0** | **Option 2 — MIT / Apache-2.0** |
|---|---|---|
| kiegen's own licence | GPL-3.0 | MIT OR Apache-2.0 |
| `kokoro-tts` as published | ✅ use it as-is, no fork | ❌ must fork and strip the espeak-derived C |
| espeak-ng (the 5 espeak-only languages) | ✅ free to bundle, link, or subprocess | ⚠️ arm's-length only: user-installed (`brew install espeak-ng`), invoked as a subprocess, never linked or bundled |
| language coverage | all 9 language codes / 54 voices | English + Mandarin (+ Japanese only if you port a Rust G2P) |
| engineering cost | ≈ zero | one small fork plus a maintained patch |
| downstream appeal | copyleft — fine for an end-user utility, unattractive to anyone embedding it in a closed product | permissive, widest reuse, easiest for contributors |

**Decision: Option 2 — kiegen is `MIT OR Apache-2.0`, and GPL must not enter the repo.** The file-level consequence is that the published `kokoro-tts` crate is **unusable as-is** (its unconditional `cc::Build` of `en_ipa.c` puts GPL-3.0-derived code in your binary), so the direct-build path below stops being optional and becomes the plan. `say` stays as the bootstrap engine — it is a system binary invoked as a subprocess, so it costs nothing licence-wise.

**Licence hygiene, mechanically enforced (so this cannot drift back in):**

- **`cargo-deny` 0.20.2** (`EmbarkStudios/cargo-deny`) in CI with an explicit `licenses.deny` list — `GPL-3.0`, `AGPL-3.0`, `SSPL-1.0` and friends — and `cargo deny check licenses` as a required check. A policy in a document does not survive six months; a failing CI job does.
- **No vendored espeak-ng, ever** — not the binary, not the data, not a git submodule, not "just for tests". Its absence from the repo is the whole point.
- **Everything in the runtime path must be `MIT OR Apache-2.0`-compatible**: `ort`, `rodio`, `cmudict-fast` (CMUdict is BSD-2-Clause and the crate is MIT/Apache-2.0), Tauri, and the Kokoro weights themselves (Apache-2.0, downloaded not vendored).
- Keep a `NOTICE` file and an "Open-source licences" screen — the app-side obligation of Apache-2.0 is attribution, and it doubles as the required credit for Kokoro's two CC BY training corpora.
- Write the **ARPAbet → IPA table yourself** rather than copying it out of a crate of uncertain provenance. It is a small, standard mapping, and re-deriving it is an hour of work that removes the question entirely.

The residual cost, stated plainly: the other five languages become an optional `brew install espeak-ng` path rather than a shipped feature, and English out-of-dictionary words have no fallback unless the user installs espeak-ng or you ship a pronunciation-override file. That is the price of keeping the repo clean, and it is affordable because English is what v1 ships.

Everything else in the stack is permissive either way (Kokoro weights and ONNX exports Apache-2.0, `misaki` Apache-2.0, `kokoro-onnx` MIT, `ort`/`rodio` MIT OR Apache-2.0, `cmudict-fast` MIT/Apache-2.0, `jieba-rs`/`pinyin`/`chinese-number` MIT), so this single decision closes the whole question. Whichever you pick, add an **"Open-source licences"** screen listing Kokoro, those two CC BY training corpora, espeak-ng if present, and every crate — it doubles as the required attribution and as the answer to "why does this app want Accessibility?".

### Packaging and runtime shape

- **Do not bundle the model in the .app.** The DMG stays ~4 MB; the model is fetched on first use into `~/Library/Application Support/kiegen/models/`, sha256-pinned, resumable, with progress in the tray. Default `fp32` (325 MB — see the benchmark below for why not the smaller ones); offer `fp16` (163 MB) and `q8f16` (86 MB) as explicitly-labelled trade-offs.
- **Resident, not reloaded.** Load is seconds and hundreds of MB of RSS (a third-party M1 Pro benchmark of the MLX path measured ~600 MB after load, ~800 MB peak). So: lazy-load on first speak, **keep warm** by default in-process, and offer idle-unload after N minutes for battery- or RAM-sensitive users. This is the one place the "nothing resident" promise bends — say so plainly in the settings UI (a "Kokoro memory: 640 MB · unload" row).
- **Chunk and stream.** Split under the 510-phoneme cap on sentence boundaries; synthesize chunk *n+1* while *n* plays; cross-fade ~20 ms, because concatenated Kokoro segments have audible seams. This is what makes a long selection feel instant instead of one long pause.
- **Voice loading is cheap.** 0.5 MB per voice means the picker can preview every voice with no download — and it enables **weighted voice blending** (mix two voice embeddings, e.g. 70% `am_michael` / 30% `bm_george`) plus a `speed` float (~0.5–2.0). Blend + speed + per-app override *is* the "configure the audio generation" surface.
- **Text normalization before G2P** is the unglamorous 20% of this project: numbers, currency, times, URLs, filenames, emoji, ALL-CAPS into spoken form. Python's misaki gives this away; the Rust path does not. Budget real time for it, or ship a rule table plus a num2words-style expander.
- **Cache** on `sha256(text + voice + speed + model_hash)`, storing 24 kHz PCM.

### Benchmark results — measured on this machine (Apple Silicon, macOS 26.5.1, ONNX Runtime 1.30.0)

Method: one process per configuration (clean RSS), the same 43-token IPA sentence, voice `af_heart`, `intra_op_num_threads = cpu_count`, best of 3 runs, G2P excluded by feeding hand-built IPA phonemes. So these numbers are the **model only** — add G2P on top.

| quant | EP | load | synth latency | RTF | peak RSS | vs fp32 |
|---|---|---|---|---|---|---|
| **fp32** 325 MB | CPU | 0.43 s | **715 ms** | **0.206** | 478 MB | reference |
| fp32 | CoreML | 3.98 s | 768 ms | 0.221 | 630 MB | — |
| fp16 163 MB | CPU | 0.52 s | 746 ms | 0.215 | 578 MB | 2.66 dB |
| fp16 | CoreML | 1.10 s | 750 ms | 0.216 | 604 MB | — |
| q8f16 86 MB | CPU | 0.57 s | 1289 ms | 0.400 | **245 MB** | 5.53 dB |
| q8f16 | CoreML | 1.26 s | 1258 ms | 0.390 | 265 MB | — |

Four conclusions, three of them counter-intuitive:

1. **Quantising makes it ~1.8× slower, not faster.** q8f16/quantized land at RTF 0.40–0.54 against fp32's 0.21 — on CPU, float16 and int8 weights get upconverted per operator. Quantisation buys memory (245 MB vs 478 MB) and nothing else.
2. **fp16 is not a free size win**: identical speed to fp32 (0.215), *more* resident memory (578 MB vs 478 MB), and half the download (163 MB vs 325 MB). The trade is purely download size vs RAM.
3. **CoreML buys nothing.** Same RTF as CPU on every quant, a 4-second fp32 session load, plus CoreAnalytics "Context leak detected" spam. **Kill the CoreML/MLX branch of the design** — no GPU path, no MLX sidecar, one less thing to maintain.
4. **The real UX number is ~715 ms to first audio** for a 3.5 s sentence, and because the balanced chunker emits ~510-phoneme batches, latency is roughly constant per chunk regardless of selection length. Streaming chunk *n*+1 while *n* plays is what keeps perceived latency there.

**Decision: default `fp32` (325 MB).** Fastest, least RAM, reference quality. The instinct to shrink the download costs latency that nothing else in the stack can buy back. Offer `fp16` (163 MB) as the smaller-download option and `q8f16` (86 MB) as the low-memory option, both labelled slower.

Caveats on the evidence: the divergence column is a crude proxy (mean |Δ dB| of FFT magnitudes vs fp32), **not** a perceptual metric — and the quantised models render the same input **~9 % shorter** (3.17 s / 3.23 s vs 3.48 s), so they deviate in *timing*, not merely in noise. Ten 24 kHz WAVs of the same sentence, every quant × both execution providers, are in `~/Desktop/kokoro-samples/`. Listening is the only test that settles voice quality — do that before locking the default, and note that ~20 MB of RAM per warm voice row is the cost of blending.

Scaffolding for the spike lives in `/Users/home/.hermes/cache/scratch/kokoro_spike/` (`bench.py`, `probe.py`) — a Python harness, NOT the shipping implementation; it exists to produce the numbers above.

### Kokoro-specific risks

1. ~~Quantized quality is unverified here~~ **Measured** (see benchmark): q8f16 costs 1.8× latency and shifts timing ~9 %, so it is the *low-memory* option, not the default.
2. Chunk seams and prosody resets across sentence boundaries; cross-fade mitigates, does not eliminate.
3. A resident model in a menu-bar utility is a real energy/RAM cost — measured at **478 MB peak RSS for fp32**, 578 MB for fp16, 245 MB for q8f16.
4. The first-run 86–163 MB download sits awkwardly against a "no setup required" promise — mitigate with `say` as the immediate degraded engine plus a tray progress row.
5. Multilingual coverage is out of v1 by decision, and espeak-ng stays an arm's-length optional subprocess — so the licence surface of the repo stays `MIT OR Apache-2.0` no matter what the user installs.

### The engine lineup after this change: `say`, Kokoro, and Chatterbox Multilingual

Three engines, and none of them needs Python.

**Qwen3-TTS is dropped.** One reason, and it is sufficient: its ten languages are a strict
subset of Chatterbox Multilingual's twenty-three, so it added no coverage at all — and it
was the only engine in the catalogue kept alive by a Python sidecar. Removing it is what
makes the app zero-Python: `sidecar_python()` and the `KIEGEN_SIDECAR_PYTHON` env override
that existed to find that interpreter are gone from `engine_paths.rs`, and so is the
HuggingFace-cache probe the MLX engines used. Both remaining local engines are plain file
sets under `~/Library/Application Support/kiegen/models/`, fetched and sha256-verified by
the app itself.

**Chatterbox is now Chatterbox *Multilingual*, over ONNX in Rust** —
`onnx-community/chatterbox-multilingual-ONNX`, MIT and ungated, pinned by commit. It covers
the 23 languages in Resemble's own `SUPPORTED_LANGUAGES` table (`ar` Arabic, `da` Danish,
`de` German, `el` Greek, `en` English, `es` Spanish, `fi` Finnish, `fr` French, `he` Hebrew,
`hi` Hindi, `it` Italian, `ja` Japanese, `ko` Korean, `ms` Malay, `nl` Dutch, `no` Norwegian,
`pl` Polish, `pt` Portuguese, `ru` Russian, `sv` Swedish, `sw` Swahili, `tr` Turkish, `zh`
Chinese), and the engine offers one selectable entry per language — the id is the code, the
label is the language's name. It is a **zero-shot voice cloner**: the speaker comes from a
reference clip (`default_voice.wav` ships in the repo as the fallback), not from a speaker
table, which is exactly why the catalogue lists languages rather than named voices.

The download is **11 files, 1,508,858,027 bytes (~1.5 GB)**, pinned to commit
`452d3f43…` and asserted down to the byte in `download.rs`'s own tests. Each of the four
graphs is a tiny `.onnx` plus a large `*_onnx_data` external-weights sidecar, and neither
half loads without the other — so both are in the plan, always. Only the language model is
quantised (`language_model_q4f16`, 305 MB); the speech encoder (592 MB), the conditional
decoder (540 MB) and the token embedding (68 MB) are fp32-only in this export. The English-
only `ResembleAI/chatterbox-turbo-ONNX` is not used anywhere any more.

Three caveats, recorded rather than fixed:

1. **This ONNX export is the V2-era multilingual model** — its base model is
   `ResembleAI/chatterbox`. Resemble's current multilingual is **V3**, which exists in MLX
   (`mlx-community/chatterbox-multilingual-v3`) but has **no ONNX export yet**. So this
   choice is one model generation behind on quality, and it is deliberate: it is the only
   multilingual Chatterbox that runs without Python. When a V3 ONNX export appears, the
   change is the repo constant plus the plan's byte table, nothing structural.
2. **Two of the four language normalisers are not ported.** The reference implementation
   normalises text before tokenizing for four codes: `zh` (Cangjie conversion via
   `Cangjie5_TC.json` plus a character segmenter), `ja` (kanji → hiragana), `he` (add
   diacritics) and `ko` (jamo → syllable composition). Of these, **`zh` and `ko` are ported**
   in `chatterbox.rs`, and **`ja` and `he` are refused** — `[ja]` and `[he]` are gated off
   with a sentence naming the language, checked *before* any graph is loaded, because a
   kanji-to-kana dictionary and a Hebrew diacritiser are not something this build can
   substitute with a guess. Gating is the honest option: the checkpoint would otherwise be
   handed text it was never trained to read, and the failure mode would be a plausible-
   looking utterance in the wrong reading rather than an error.
3. **Voice cloning is a feature now.** Chatterbox is a zero-shot cloner, so the voice *is* a
   reference clip; `ref_audio` names a file inside the engine's own `voices/` directory and
   the shipped `default_voice.wav` is just the built-in entry. See §5.1.

### 5.1 Reference voices (cloning)

There is no speaker table anywhere in this checkpoint, so "choosing a voice" can only mean
"choosing a clip". `voices.rs` owns that:

| | |
|---|---|
| **Stored at** | `~/Library/Application Support/kiegen/models/chatterbox/voices/` — beside the weights, so deleting the engine deletes its voices with it |
| **Named by** | the file name, slugged from what the user called it |
| **Accepted** | a WAV of any rate or channel count: stereo is averaged down and any rate is linearly resampled to 24 kHz, which is what `speech_encoder` takes |
| **Refused** | under 1 s or over 60 s of audio, empty, or not a readable WAV — with the reason in the message |
| **Selected by** | `settings.chatterbox.ref_audio`, a file name; `null` means the shipped clip |
| **Deleted by** | a Delete button on each row; the built-in clip has none |

Adding is a Tauri file dialog for the *path* (`tauri-plugin-dialog`) followed by a Rust
command that reads, validates, resamples and copies — the copy is the app's, not the
window's, so a clip the user later moves or deletes does not take the voice with it.

The Rust ONNX runtime for these four graphs landed with this change: `chatterbox.rs` holds
the session, the KV cache and the generation loop, `spoken.rs` routes to it, and the engine
is `can_speak: true` once its weights are on disk.

---

## 6. Tauri v2 wiring (verified against the installed crates)

| Need | Use | Status |
|---|---|---|
| Tray icon + menu | core Tauri `TrayIconBuilder` (`tray-icon` feature, on by default) | present in tauri 2.11.6 |
| Hide Dock icon | `app.set_activation_policy(ActivationPolicy::Accessory)` in `setup`, and `LSUIElement` in a merged `Info.plist` | `set_activation_policy` present in tauri 2.11.6 |
| Global hotkeys | `tauri-plugin-global-shortcut` | 2.3.2 stable |
| Launch at login | `tauri-plugin-autostart` | 2.5.1 stable |
| Config persistence | `tauri-plugin-store` | 2.4.5 stable |
| Feedback toasts | `tauri-plugin-notification` | 2.4.0 stable |
| Open permission deep links / files | `tauri-plugin-opener` | already in the scaffold |
| Non-activating HUD overlay | `tauri-nspanel` | 2.1.0, actively maintained |
| AX + Cmd+C + pasteboard FFI | `objc2-app-kit`, `objc2-foundation`, `core-graphics`, `macos-accessibility-client` | all on crates.io |

Notes:

- **The HUD must not steal focus.** Use an `NSPanel` with the `NonactivatingPanel` style mask; a regular Tauri window would pull focus away from the user's caret and kill the selection. This is why `tauri-nspanel` matters.
- The settings window is declared `"visible": false`, created lazily on tray → Settings, and **destroyed** (not just hidden) on close so nothing is resident but the tray and the warm model.
- The shortcut recorder is something you build: a webview keydown listener that captures modifiers + key, validates (at least one modifier, not an OS-reserved combo), *tries* registration through the plugin, and rolls back if registration fails. Show conflicts instead of silently dropping them.
- Rust owns the shortcuts; the webview only edits config and emits change events. Apply = unregister-all + register-from-config (idempotent, so a half-broken config can't wedge the app).

---

## 7. UX flows

**Happy path:** select text → chord → ~50 ms for capture → tray icon animates → speech starts within ~200 ms for a sentence → chord again (or Stop) interrupts.

**Failure paths that must be designed, not discovered:**

- Nothing selected / app doesn't expose AX → "Couldn't read a selection" toast with a one-line hint, and don't touch the clipboard.
- Secure input active (password field) → `IsSecureEventInputEnabled()` is true; abort **before** posting any event and show "not available in password fields". Never log or cache text captured under ambiguity.
- Text too long → chunk on sentence boundaries, stream, stay cancellable.
- Accessibility permission revoked since last run → tray badge + one-click deep link.
- Model not downloaded yet → `say` covers it immediately, with a tray progress row for the Kokoro download.
- Offline + cloud profile (if ever added) → automatic local fallback with a note.

**Onboarding (first run only):** a small window that explains the one permission it needs, links to the right System Settings pane, and shows a live checkmark when the grant lands (poll `AXIsProcessTrusted` every ~1 s while visible). Do not block the app on it — the tray item exists regardless.

---

## 8. MVP cut

**v0 — ✅ built** (see README.md and docs/DEV.md)
tray menu (Speak / Stop / Settings… / Quit) · `Cmd+Shift+S` speak + `Cmd+Shift+X` stop, rebindable with an in-app recorder · AX-then-copy capture with the secure-input guard and a bounded 150 ms copy timeout · `say` playback (kept permanently as the bootstrap engine) · settings window that opens on demand and polls the Accessibility grant · one JSON config file · 8 unit tests · `scripts/check-licenses.sh` + `deny.toml` enforcing the licence decision in CI.

**v0.5 (Kokoro)**
Our own Rust pipeline on `ort` — **no third-party Kokoro crate** (licence-blocked): tokenizer, session/EP selection, chunker (balanced batching + inter-batch pauses), silence trim, `fp32` model download on first use, streaming playback through `rodio`, cache, voice picker with previews, speed slider. Plus the English front-end this creates: **text normalization**, a **cmudict lookup with our own ARPAbet→IPA table**, a **pronunciation-override file** (`~/.config/kiegen/pronounce.json`) for names and brand words, and an **optional espeak-ng subprocess** that handles out-of-dictionary words properly *if* the user has installed it — discovered on `PATH`, never bundled.

**v1**
voice blending, per-app overrides, HUD overlay, autostart, permissions health panel, keep-warm/idle-unload controls, speak-to-file.

**v2**
Services entry, auto-popup on mouse-up (Input Monitoring), history (opt-in). The multilingual question is answered by Chatterbox Multilingual (§5) rather than left open; the espeak-ng licence decision stands unchanged. Speaker cloning from a user-supplied clip is the remaining gap in the Chatterbox engine, and the Rust ONNX runtime for it is the next piece of work.

Explicitly *not* in v0: history, streaming word-highlight, per-app shortcuts, themes, export/import.

---

## 9. Open product questions for you

1. ~~Is kiegen closed-source/commercial?~~ **Decided: open source, `MIT OR Apache-2.0`** — the permissive option, with `cargo-deny` enforcing it so GPL cannot drift back in (§5).
2. ~~English-only for v1, or the full 9 languages?~~ **Decided: English only for v1.** Mandarin/Japanese remain reachable later; the other five (Spanish, French, Hindi, Italian, Portuguese) are an optional user-installed espeak-ng path, documented, never shipped. **Partly superseded:** a second engine — Chatterbox Multilingual — now offers 23 languages, none of them through espeak, so the "which languages" question is answered twice over: Kokoro's 9 codes, and Chatterbox's 23 for everything Kokoro cannot reach without GPL code (§5).
3. Does the audio **play** and/or get **written to a file** by default? "Speak" vs "Speak to file" as separate chords is the plan.
4. Should the user be able to configure **per-app** shortcuts/voices, or is one global chord enough for v1?