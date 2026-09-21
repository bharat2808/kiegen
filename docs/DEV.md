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

### The install path is an integration test, and it is not run by default

`tests/install.rs` is the only test that answers "does a download produce a working
engine?". It fetches the real 346 MB, writes it to a real directory, then loads the graph in
ONNX Runtime and synthesizes from a known phoneme string. It is `#[ignore]`d because it needs
the network and a few hundred megabytes:

```
cargo test --test install -- --ignored --nocapture
```

Run it after touching `download.rs`, `engine_paths.rs` or the Kokoro file set. It caught two
bugs that every unit test passed straight through, which is the argument for keeping it:

- `probe` gave up on files whose HEAD carries no `x-linked-size`. `tokenizer.json` is one,
  and the engine cannot start without it.
- `KOKORO_MODEL_FILE` was `"model.onnx"` while the file lands at `onnx/model.onnx`, so
  `kokoro_installed()` never returned true — the engine badge would have stayed on "Needs
  340 MB" after a *successful* install. Its unit tests agreed with it, because they wrote
  their fixtures to the same wrong path. A test that shares the implementation's mistake
  cannot catch it.
- `Content-Length` is not the file's length when the response is compressed. GitHub's raw
  host answers `content-encoding: gzip` with `content-length: 737890` for a 3,000,469-byte
  dictionary, and `ureq` accepts gzip by default — so the pre-write size check compared a
  compressed length against an uncompressed expectation and rejected a perfectly good file.
  Every request now asks for `identity`: ranges over an encoded stream are meaningless
  anyway, and the hash at the end has to be over the bytes as stored. This is the same family
  as the redirect trap above — a length header that means something other than "the size of
  the file".

### The front end is measured against the reference, not eyeballed

Kokoro does not take text. It takes phonemes, and the table it is scored against came from
`misaki` — so `lexicon.rs` + `g2p.rs` are held to misaki's own output sentence by sentence
rather than to a sample someone listened to. Two fixtures make that checkable offline:

- `tests/fixtures/g2p_corpus.json` — 42 sentences with misaki's phonemes for each, including
  the cases that break naive ports: numbers, money, years, acronyms, contractions, quotes,
  hyphenated words, `$3.50` (cents), `555-1234`, unknown words.
- `tests/fixtures/numbers_fixture.json` — `num2words` output, so number spelling is checked
  against the library the reference calls rather than against intuition.

```bash
cargo test --test g2p_parity -- --ignored --nocapture   # needs the two 3 MB dictionaries
cargo test --lib numbers::                              # number spelling, no download needed
```

Numbers are verified exhaustively (100,000 values, 100% agreement with `num2words`); the
sentence corpus sits at **86.4% word agreement, 27/42 sentences byte-identical**, and the
test fails below 85%. The residue is not a mystery:

- **Most of it is the missing tagger.** "record" is `ɹˈɛkəɹd` as a noun and `ɹəkˈɔɹd` as a
  verb; misaki reads spaCy's parse to decide, and a word list cannot. Same for the weak forms
  of *a/the/to* and for tag-keyed dictionary variants. A tagger is a v1 question, not a bug
  to paper over with a guess.
- **Unknown words are handled better here, not worse.** misaki emits `❓` for "kiegen" and
  even for "Kokoro"; Kokoro silently deletes anything marked that way, so the word vanishes
  from the audio. This port spells the letters instead, which is audible and wrong in a
  smaller way.
- Cosmetic: the reference re-quotes with curly marks and drops a trailing period after an
  abbreviation.

The dictionary is **Apache-2.0** and pinned by commit, not by `main`. Both files are pure
string tables (`us_gold.json` 90,201 entries, `us_silver.json` 93,361), so nothing copyleft
crosses into the repo — which is the whole reason the front end is a port rather than a
wrapper around `misaki` and its `espeak-ng` extra.

### The shortcut is tested end to end, and the artifact is the proof

`tests/speak.rs` is the one that answers the question the app exists for: **text in, audio
out, through the same code the shortcut calls.** Not a copy of it, and not a mock. It needs
a real install:

```bash
KIEGEN_MODELS_DIR=<a dir holding models/kokoro> \
  cargo test --test speak -- --ignored --nocapture
```

To get that directory, install into somewhere durable first — `install.rs` accepts
`KIEGEN_INSTALL_TEST_DIR` so a re-run reuses the install instead of re-downloading it:

```bash
KIEGEN_INSTALL_TEST_DIR=~/kiegen-models cargo test --test install -- --ignored --nocapture
KIEGEN_MODELS_DIR=~/kiegen-models      cargo test --test speak   -- --ignored --nocapture
```

It asserts that the text reaches the engine as phonemes, that nothing was silently dropped,
that the file on disk matches the duration the code reported, that the audio is not silence
or a DC offset, that `speak` spawns a player and `stop` kills it, and that switching voice
reloads rather than reusing the previous style table.

**The assertions are not the proof.** The file it leaves behind is, and so is what a speech
recogniser makes of it — the last two words are the ones a listener would have to catch:

```bash
whisper-cli -m ~/.cache/whisper/ggml-base.en.bin -otxt <the wav it printed>
```

The current run of that, on the text *"The quick brown fox jumps over the lazy dog. It costs
$3.50 and the record was 21 degrees in November 2005."*:

> Brown Fox jumps over the lazy dog. It costs $3.50, and the record was 21 degrees in November 2005.

The recogniser normalises the spoken "three dollars and fifty cents" back into `$3.50`, so
the money path is covered by the phoneme-level check in `g2p_parity.rs` rather than visible
here. The missing "The quick" is the recogniser, not the audio: `ggml-base.en` drops the
opening of short clips, and it did the same to the earlier install test.

One trap worth knowing if you extend this test: it renders twice (once per voice). The second
render must write to **its own path**. Reusing the first one silently overwrites the artifact
underneath the assertions, and the file you transcribe afterwards is no longer the file the
test measured.

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
  engines.rs     the engine catalogue: what can speak, and why one cannot
  engine_paths.rs where each engine's files live, and whether they are there
  download.rs    fetch + verify model weights (nothing is bundled or committed)
  numbers.rs     number and money spelling, matching num2words
  lexicon.rs     pronunciation lexicon (port of misaki's, Apache-2.0)
  g2p.rs         text -> phonemes, without eSpeak
  kokoro.rs      ONNX Runtime + the Kokoro graph
  spoken.rs      which engine speaks, and the refusal when one cannot
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