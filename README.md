# TextHalo

**Select text. Press a shortcut. Hear it read aloud.**

TextHalo is a macOS menu bar text-to-speech app with Apple system voices and local AI speech through Kokoro and Chatterbox. Built with Rust, Tauri, React, and TypeScript, it keeps playback controls close without taking focus away from your work.

## Features

- **Read selected text** from other apps with a configurable global shortcut.
- **Choose between three speech engines:** Apple system voices, Kokoro 82M, and Chatterbox Multilingual.
- **Preview voices** directly in settings before using them.
- **Start listening sooner:** Kokoro and Chatterbox play short phrases while the next phrase is generated.
- **Control playback from a floating overlay** at the top center of the screen, with speech status and a Stop button.
- **Use reference voices with Chatterbox** by importing a local WAV clip.
- **Keep models loaded** between requests to avoid repeated model initialization.
- **Choose how text is captured:** Accessibility, copying, or Accessibility with a copy fallback.

## Get started

1. Launch TextHalo and open **Settings…** from its menu bar icon.
2. Enable TextHalo in **System Settings → Privacy & Security → Accessibility**.
3. Open **Voice**, choose an engine, and preview a voice. Apple system voices work without downloading a model; Kokoro and Chatterbox require a model download through the app.
4. Select text in another app and press **Cmd+Shift+S**.
5. Press **Cmd+Shift+X** or click **Stop** in the overlay to stop playback.

| Action | Default control |
| --- | --- |
| Read selected text | `Cmd+Shift+S` |
| Stop speech | `Cmd+Shift+X` |
| Open settings | Menu bar icon → **Settings…** |
| Preview a voice | **Voice** → **Preview** |

Change the keyboard shortcuts in **Shortcuts**. Selection length is limited to 5,000 characters by default and can be adjusted in **Capture**.

## Speech engines

| Engine | Voices and languages | Setup |
| --- | --- | --- |
| **Apple system voices** | Voices installed in macOS, with language selection and speaking-rate controls | Available immediately; the default engine |
| **Kokoro 82M** | American and British English; additional supported languages through espeak-ng | Download the model and voices in the app |
| **Chatterbox Multilingual** | 23 languages, a built-in reference voice, and imported WAV reference clips | Download the model in the app |

Kokoro supports Spanish, French, Hindi, Italian, and Brazilian Portuguese through an installed espeak-ng executable. Japanese and Mandarin Kokoro voices are currently unavailable because their text front ends are not implemented.

Chatterbox has controls for emotion intensity and keeping the model loaded. Use **Add voice…** to import a WAV reference clip; TextHalo validates and converts the clip for the model.

### Optional espeak-ng support

TextHalo detects an existing `espeak-ng` installation and invokes it as a separate CLI process. It does not bundle or link the espeak library.

For English Kokoro voices, dictionary pronunciations are tried first. Unknown words use espeak-ng only when it is detected. If it is absent or cannot produce a pronunciation, TextHalo retains its letter-spelling behavior. Known words and acronyms keep their dictionary handling.

Detection includes standard Homebrew locations and `PATH`. For a custom installation, set `KIEGEN_ESPEAK_NG` to the executable's full path in the environment used to launch TextHalo.

## Text capture and privacy

Speech synthesis runs locally. Selected text is not sent to a cloud speech service. Internet access is used to download model assets, including files from Hugging Face and GitHub.

TextHalo requires Accessibility permission to capture another app's selection. In **Capture**, choose Accessibility-only capture to avoid using the clipboard, or use copying for apps that do not expose their selection through Accessibility. Copy-based capture can restore the previous clipboard contents; restoration is enabled by default.

Models and imported reference voices are stored locally. Both local engines keep their continuous playback samples in memory.

| Data | Default macOS location |
| --- | --- |
| Settings | `~/Library/Application Support/com.kiegen.app/settings.json` |
| Models and reference voices | `~/Library/Application Support/kiegen/models/` |
| Generated audio cache | `~/Library/Application Support/kiegen/cache/` |

These existing Kiegen-named locations are retained so the TextHalo rebrand preserves
Accessibility access, settings, models, and cached audio.

## Build from source

Use macOS with Node.js 22+, npm, Rust stable, and Xcode Command Line Tools.

```bash
git clone https://github.com/bharat2808/texthalo.git
cd texthalo
npm ci
npm run tauri dev
```

To build the macOS application:

```bash
npm run tauri build -- --bundles app
```

The application is written to:

```text
src-tauri/target/release/bundle/macos/TextHalo.app
```

Copy `TextHalo.app` to **Applications**, launch it, and enable Accessibility access. Rebuilding can invalidate the previous permission grant. If capture stops working after a rebuild, remove the stale TextHalo entry in Accessibility settings and enable the rebuilt app again.

## Development checks

```bash
npm run build
cargo fmt --manifest-path src-tauri/Cargo.toml --check
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings

# Use an empty model directory for the standard suite.
KIEGEN_MODELS_DIR="$(mktemp -d)" cargo test --manifest-path src-tauri/Cargo.toml

./scripts/check-licenses.sh
```

Real-model and download tests require additional assets and are not all run by the standard suite. To exercise Kokoro streaming with the models installed in their default location:

```bash
cargo test --manifest-path src-tauri/Cargo.toml --lib kokoro_pcm_latency_and_stop -- --ignored --nocapture
```

This test plays audio and measures cold/warm playback startup, buffer underruns, and Stop. To measure synthesis at different sentence lengths, run the ignored `benchmark_kokoro_sentence_lengths` test. The equivalent Chatterbox measurement is `chatterbox_pcm_latency_and_stop` and requires its installed model.

## Project layout

| Path | Purpose |
| --- | --- |
| `src/` | React settings interface and speech overlay |
| `src-tauri/src/` | Native app, selection capture, speech engines, downloads, and playback |
| `src-tauri/tests/` | Integration and model verification tests |
| `scripts/` | Development and dependency-license checks |
| `docs/` | Development notes and design history |
| `spikes/` | Experimental implementations and investigation notes |

## License

TextHalo is licensed under [Apache-2.0](LICENSE-APACHE). Downloaded models and separately installed tools retain their own licenses. Dependency-license checks are defined in [`deny.toml`](deny.toml) and [`scripts/check-licenses.sh`](scripts/check-licenses.sh).

Maintained by **bharat2808**.
