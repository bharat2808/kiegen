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

/// Env override, used by the verification harnesses to point at a scratch tree.
const MODELS_ENV: &str = "KIEGEN_MODELS_DIR";
const SIDECAR_PYTHON_ENV: &str = "KIEGEN_SIDECAR_PYTHON";

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

/// The interpreter to run the sidecar with, if one has been installed. Shared by every MLX
/// engine (Qwen and Chatterbox both go through the same mlx-audio runtime). Checked in
/// order of decreasing specificity so a power user can point at a system Python without
/// touching anything else.
pub fn sidecar_python() -> Option<String> {
    if let Ok(explicit) = std::env::var(SIDECAR_PYTHON_ENV) {
        if Path::new(&explicit).is_file() {
            return Some(explicit);
        }
    }
    let managed = app_support_dir()?.join("runtime/sidecar/bin/python3");
    if managed.is_file() {
        return Some(managed.display().to_string());
    }
    None
}

/// Is a model present in the HuggingFace cache? That cache layout is owned by the Python
/// runtime (`huggingface_hub`), so this only *looks* — the app never places files there
/// itself, which is why the MLX engines are installed by their sidecar and not by us.
pub fn mlx_model_installed(repo_id: &str) -> bool {
    let dir_name = format!("models--{}", repo_id.replace('/', "--"));
    hf_hub_dir()
        .map(|hub| hub.join(dir_name).is_dir())
        .unwrap_or(false)
}

fn hf_hub_dir() -> Option<PathBuf> {
    match std::env::var("HF_HOME") {
        Ok(home) => Some(PathBuf::from(home).join("hub")),
        Err(_) => Some(PathBuf::from(std::env::var("HOME").ok()?).join(".cache/huggingface/hub")),
    }
}

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
    fn the_three_required_files_together_read_as_installed() {
        let dir = scratch("complete");
        std::fs::write(dir.join(KOKORO_MODEL_FILE), b"x").unwrap();
        std::fs::write(dir.join(KOKORO_TOKENIZER_FILE), b"x").unwrap();
        std::fs::write(dir.join("voices/af_heart.bin"), b"x").unwrap();
        assert!(kokoro_installed_at(&dir));
    }

    /// A directory that does not exist must read as not installed, not as an error.
    #[test]
    fn a_missing_directory_reads_as_not_installed() {
        let dir = std::env::temp_dir().join("kiegen-paths-does-not-exist-at-all");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(!kokoro_installed_at(&dir));
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
    }
}
