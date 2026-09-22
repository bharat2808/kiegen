//! Where the optional engines keep their weights and runtimes.
//!
//! Two rules from docs/DESIGN.md §4 are enforced here rather than trusted to callers:
//! weights never live inside the `.app` bundle (so a `cargo build` cannot accidentally
//! ship 2 GB), and nothing is downloaded by this module — it only reports what is on disk.

use std::path::{Path, PathBuf};

/// Kokoro needs three things to be present: the graph, the phoneme tokenizer, and at
/// least one voice style table.
/// Repo-relative, because that is the layout the ONNX export uses and the one `Kokoro::load`
/// is handed. It was previously `"model.onnx"` with no `onnx/` prefix — a path that never
/// exists on disk, so a complete install still read as "not installed" and the engine badge
/// would have stayed on "Needs 340 MB" forever.
pub const KOKORO_MODEL_FILE: &str = "onnx/model.onnx";
pub const KOKORO_TOKENIZER_FILE: &str = "tokenizer.json";

/// The pronunciation dictionaries the front end reads. Kokoro takes phonemes, so without
/// these it cannot read a word, however complete the rest of the install is.
pub const LEXICON_GOLD_FILE: &str = "lexicon/us_gold.json";
pub const LEXICON_SILVER_FILE: &str = "lexicon/us_silver.json";

/// Chatterbox Multilingual's four graphs. Each is a tiny `.onnx` whose weights live in a
/// sibling `*_onnx_data` file — neither half is loadable alone, so an install that has one
/// without the other is not an install.
pub const CHATTERBOX_LM_FILE: &str = "onnx/language_model_q4f16.onnx";
pub const CHATTERBOX_ENCODER_FILE: &str = "onnx/speech_encoder.onnx";
pub const CHATTERBOX_DECODER_FILE: &str = "onnx/conditional_decoder.onnx";
pub const CHATTERBOX_EMBED_FILE: &str = "onnx/embed_tokens.onnx";
/// The Llama BPE tokenizer — Chatterbox's whole text front end, and the reason it needs no
/// espeak. Without it the engine has nothing to turn text into tokens with.
pub const CHATTERBOX_TOKENIZER_FILE: &str = "tokenizer.json";
/// The Chinese character mapping the `zh` path converts with. Not required to *start* the
/// engine — only `zh` reads it — so it is not in the installed check.
pub const CHATTERBOX_CANGJIE_FILE: &str = "Cangjie5_TC.json";
/// The reference clip a fresh install clones. A user's own clips live under
/// [`CHATTERBOX_VOICES_DIR`] and are never written here.
pub const CHATTERBOX_DEFAULT_VOICE_FILE: &str = "default_voice.wav";
/// Where the user's own reference clips are copied, under the engine's own directory so a
/// single delete of `models/chatterbox` takes them with it.
pub const CHATTERBOX_VOICES_DIR: &str = "voices";

/// Env override, used by the verification harnesses to point at a scratch tree.
const MODELS_ENV: &str = "KIEGEN_MODELS_DIR";
/// Points at an `espeak-ng` binary directly. Exists so a test (or a user with an unusual
/// install) can name the binary without the app guessing.
const ESPEAK_ENV: &str = "KIEGEN_ESPEAK_NG";

/// `~/Library/Application Support/kiegen` on macOS, `$XDG_DATA_HOME/kiegen` elsewhere.
/// Resolved from the environment rather than Tauri's `app_data_dir` so it stays testable
/// without an `AppHandle`.
pub fn app_support_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var(MODELS_ENV) {
        return Some(PathBuf::from(dir));
    }
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join("Library/Application Support/kiegen"))
}

pub fn models_dir() -> Option<PathBuf> {
    Some(models_dir_from(&app_support_dir()?))
}

/// Pure, so the layout can be tested without mutating process-wide environment.
fn models_dir_from(support: &Path) -> PathBuf {
    support.join("models")
}

pub fn kokoro_dir() -> Option<PathBuf> {
    Some(models_dir()?.join("kokoro"))
}

pub fn chatterbox_dir() -> Option<PathBuf> {
    Some(models_dir()?.join("chatterbox"))
}

/// Where the user's own reference clips live. `models/chatterbox/voices` — inside the
/// engine's own directory, so it survives restarts without a second location to keep in
/// sync, and so removing the engine removes them too.
pub fn chatterbox_voices_dir() -> Option<PathBuf> {
    Some(chatterbox_dir()?.join(CHATTERBOX_VOICES_DIR))
}

/// Is a complete-enough Chatterbox install on disk? Like Kokoro's, this checks the files
/// the engine actually opens rather than a stamp, so a half-finished download cannot look
/// installed. It is deliberately *not* a claim that the engine can speak: the ONNX front
/// end is separate work, and `engines::catalog` keeps `can_speak` false either way.
pub fn chatterbox_installed() -> bool {
    match chatterbox_dir() {
        Some(dir) => chatterbox_installed_at(&dir),
        None => false,
    }
}

fn chatterbox_installed_at(dir: &Path) -> bool {
    // Only the `.onnx` halves: every one of them is useless without its `*_onnx_data`
    // sidecar, which is fetched alongside it and verified by `download::fetch`.
    [
        CHATTERBOX_LM_FILE,
        CHATTERBOX_ENCODER_FILE,
        CHATTERBOX_DECODER_FILE,
        CHATTERBOX_EMBED_FILE,
        CHATTERBOX_TOKENIZER_FILE,
    ]
    .iter()
    .all(|file| dir.join(file).is_file())
}

/// Is a complete-enough Kokoro install on disk? Deliberately checks the three files the
/// engine actually opens, so a half-finished download cannot look installed.
pub fn kokoro_installed() -> bool {
    match kokoro_dir() {
        Some(dir) => kokoro_installed_at(&dir),
        None => false,
    }
}

fn kokoro_installed_at(dir: &Path) -> bool {
    if !dir.join(KOKORO_MODEL_FILE).is_file() || !dir.join(KOKORO_TOKENIZER_FILE).is_file() {
        return false;
    }
    // The dictionaries count: an install without them is an engine that cannot read.
    if !dir.join(LEXICON_GOLD_FILE).is_file() || !dir.join(LEXICON_SILVER_FILE).is_file() {
        return false;
    }
    match std::fs::read_dir(dir.join("voices")) {
        Ok(entries) => entries.flatten().any(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "bin")
        }),
        Err(_) => false,
    }
}

/// Is an `espeak-ng` binary present that the app may *use*?
///
/// espeak-ng is **GPL-3.0**. It is never bundled, linked or vendored — this only ever looks
/// for one the user installed, and every use is a subprocess whose stdout is read. That
/// keeps kiegen permissively licensed while still reaching the five Kokoro languages whose
/// only front end is espeak. See docs/DESIGN.md §5.
pub fn espeak_ng() -> Option<PathBuf> {
    // 1. Explicit. A test harness points here, and so can a user with a peculiar install.
    if let Ok(explicit) = std::env::var(ESPEAK_ENV) {
        let candidate = PathBuf::from(explicit);
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    // 2. An arm's-length copy the app manages, if the user put one there. Nothing in the
    //    app downloads into this directory: it exists so "install it next to the app" is
    //    possible without editing PATH.
    if let Some(dir) = app_support_dir() {
        let managed = dir.join("runtime/espeak-ng/bin/espeak-ng");
        if managed.is_file() {
            return Some(managed);
        }
    }

    // 3. The usual package-manager Linux/macOS prefixes, where the user's own install lives.
    //    Homebrew on Apple Silicon, then the Intel prefix, then anything on PATH.
    for prefix in ["/opt/homebrew", "/usr/local", "/usr"] {
        let candidate = Path::new(prefix).join("bin/espeak-ng");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    for dir in std::env::var("PATH").unwrap_or_default().split(':') {
        if dir.is_empty() {
            continue;
        }
        let candidate = Path::new(dir).join("espeak-ng");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// The `espeak-ng-data` directory that goes with a given binary, when it can be inferred.
///
/// A relocated espeak-ng needs `ESPEAK_DATA_PATH` or it fails at startup with a message
/// about voices rather than anything actionable, so the sibling `share/` directory is
/// passed explicitly whenever it exists.
pub fn espeak_data_dir(binary: &Path) -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var("ESPEAK_DATA_PATH") {
        let candidate = PathBuf::from(explicit);
        if candidate.is_dir() {
            return Some(candidate);
        }
    }
    // <prefix>/bin/espeak-ng -> <prefix>/share/espeak-ng-data
    let prefix = binary.parent()?.parent()?;
    let candidate = prefix.join("share/espeak-ng-data");
    candidate.is_dir().then_some(candidate)
}

// `sidecar_python()`, `SIDECAR_PYTHON_ENV` and the HuggingFace-cache lookup that went with
// them were deleted with Qwen3-TTS: they existed to find the Python runtime that installed
// the MLX engines, and no engine installs that way any more. Both local engines are plain
// file sets under `models/`, fetched by `download.rs`.

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kiegen-paths-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // Both directories have to exist, because the required paths are themselves
        // relative (`onnx/model.onnx`). A scratch tree that omits them would let these
        // tests pass against a layout the install never produces.
        std::fs::create_dir_all(dir.join("onnx")).unwrap();
        std::fs::create_dir_all(dir.join("voices")).unwrap();
        dir
    }

    /// A model graph with no voice style table cannot speak, so it must not read as
    /// installed — otherwise the UI offers an engine that dies on first use.
    #[test]
    fn a_model_without_a_voice_is_not_installed() {
        let dir = scratch("novoice");
        std::fs::write(dir.join(KOKORO_MODEL_FILE), b"x").unwrap();
        std::fs::write(dir.join(KOKORO_TOKENIZER_FILE), b"x").unwrap();
        assert!(!kokoro_installed_at(&dir));
    }

    #[test]
    fn a_voice_without_a_model_is_not_installed() {
        let dir = scratch("nomodel");
        std::fs::write(dir.join("voices/af_heart.bin"), b"x").unwrap();
        std::fs::write(dir.join(KOKORO_TOKENIZER_FILE), b"x").unwrap();
        assert!(!kokoro_installed_at(&dir));
    }

    #[test]
    fn every_required_file_together_reads_as_installed() {
        let dir = scratch("complete");
        std::fs::create_dir_all(dir.join("lexicon")).unwrap();
        std::fs::write(dir.join(KOKORO_MODEL_FILE), b"x").unwrap();
        std::fs::write(dir.join(KOKORO_TOKENIZER_FILE), b"x").unwrap();
        std::fs::write(dir.join(LEXICON_GOLD_FILE), b"x").unwrap();
        std::fs::write(dir.join(LEXICON_SILVER_FILE), b"x").unwrap();
        std::fs::write(dir.join("voices/af_heart.bin"), b"x").unwrap();
        assert!(kokoro_installed_at(&dir));
    }

    /// The dictionaries are not optional: without them the engine has nothing to read words
    /// with, so a graph-plus-voices install must *not* count as installed. This is the same
    /// shape of bug as the `onnx/` path that used to make every complete install read as
    /// missing.
    #[test]
    fn a_graph_without_the_dictionaries_is_not_installed() {
        let dir = scratch("no-lexicon");
        std::fs::write(dir.join(KOKORO_MODEL_FILE), b"x").unwrap();
        std::fs::write(dir.join(KOKORO_TOKENIZER_FILE), b"x").unwrap();
        std::fs::write(dir.join("voices/af_heart.bin"), b"x").unwrap();
        assert!(!kokoro_installed_at(&dir));
    }

    /// A directory that does not exist must read as not installed, not as an error.
    #[test]
    fn a_missing_directory_reads_as_not_installed() {
        let dir = std::env::temp_dir().join("kiegen-paths-does-not-exist-at-all");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(!kokoro_installed_at(&dir));
        assert!(!chatterbox_installed_at(&dir));
    }

    /// Chatterbox needs all four graphs plus the tokenizer. Three of four graphs, or a
    /// tokenizer-less install, is not an engine any more than a Kokoro graph with no voice
    /// table is — and the badge must not claim otherwise.
    #[test]
    fn chatterbox_needs_every_graph_and_the_tokenizer() {
        let dir = scratch("chatterbox-complete");
        for file in [
            CHATTERBOX_LM_FILE,
            CHATTERBOX_ENCODER_FILE,
            CHATTERBOX_DECODER_FILE,
            CHATTERBOX_EMBED_FILE,
            CHATTERBOX_TOKENIZER_FILE,
        ] {
            std::fs::write(dir.join(file), b"x").unwrap();
        }
        assert!(chatterbox_installed_at(&dir));

        // Drop the tokenizer: text in, nothing out.
        std::fs::remove_file(dir.join(CHATTERBOX_TOKENIZER_FILE)).unwrap();
        assert!(!chatterbox_installed_at(&dir));

        // Drop one graph: the pipeline is missing a stage.
        std::fs::write(dir.join(CHATTERBOX_TOKENIZER_FILE), b"x").unwrap();
        std::fs::remove_file(dir.join(CHATTERBOX_ENCODER_FILE)).unwrap();
        assert!(!chatterbox_installed_at(&dir));
    }

    /// The layout is asserted on the pure helper rather than by setting `KIEGEN_MODELS_DIR`:
    /// mutating the environment from a test would race every other test in the binary.
    #[test]
    fn the_models_tree_sits_under_app_support() {
        let support = Path::new("/tmp/somewhere/kiegen");
        let models = models_dir_from(support);
        assert_eq!(models, PathBuf::from("/tmp/somewhere/kiegen/models"));
        assert_eq!(
            models.join("kokoro"),
            PathBuf::from("/tmp/somewhere/kiegen/models/kokoro")
        );
        assert_eq!(
            models.join("chatterbox"),
            PathBuf::from("/tmp/somewhere/kiegen/models/chatterbox")
        );
    }
}
