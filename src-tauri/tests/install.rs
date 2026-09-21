//! Integration test: a fresh install, against the real network and the real filesystem,
//! ending in the downloaded weights actually producing audio.
//!
//! Ignored by default because it fetches ~340 MB. Run it deliberately:
//!
//!     cargo test --test install -- --ignored --nocapture
//!
//! This is the only test that answers "does an install produce a working engine?". The unit
//! tests check the range maths and the shape of the plan; the browser preview mocks the
//! backend entirely. Neither of those can tell you whether the bytes that land on disk load
//! in ONNX Runtime and make a sound.

use std::collections::HashSet;
use std::path::PathBuf;

/// A phoneme string the engine has produced audio from before, character for character —
/// taken from the Rust/Python parity run (docs/DESIGN.md, "engine parity"). Feeding a new
/// string here would conflate "the download is bad" with "this phoneme is unknown".
const PHONEMES: &str = "ðə kwˈɪk bɹˈWn fˈɑks ʤˈʌmps ˈOvəɹ ðə lˈAzi dˈɔɡ.";

fn scratch(name: &str) -> PathBuf {
    // Overridable so a re-run can reuse a previous install instead of re-downloading.
    if let Ok(dir) = std::env::var("KIEGEN_INSTALL_TEST_DIR") {
        return PathBuf::from(dir);
    }
    std::env::temp_dir().join(name)
}

#[test]
#[ignore = "downloads ~340 MB from HuggingFace"]
fn a_fresh_install_produces_weights_that_load_and_speak() {
    let support = scratch("kiegen-install-it");
    // `engine_paths` resolves the support directory from the environment precisely so this
    // can point somewhere disposable. Set before the first call.
    std::env::set_var("KIEGEN_MODELS_DIR", &support);

    let root = kiegen_lib::engine_paths::kokoro_dir().expect("a models directory");
    println!("installing into {root:?}");
    // Start from nothing, so this cannot pass on files left by an earlier run.
    let _ = std::fs::remove_dir_all(&root);

    // 1. Install, through the same function the Tauri command calls.
    let mut reports = 0usize;
    let mut last_done = 0u64;
    let mut total_seen = 0u64;
    let mut seen_files = HashSet::new();
    let started = std::time::Instant::now();

    kiegen_lib::download::install_kokoro_into(&root, &mut |path, done, total| {
        reports += 1;
        assert!(
            done >= last_done,
            "progress went backwards: {done} after {last_done}"
        );
        assert!(done <= total, "progress {done} exceeded the total {total}");
        last_done = done;
        total_seen = total;
        seen_files.insert(path.to_string());
    })
    .expect("the install must succeed");

    let elapsed = started.elapsed().as_secs_f32();
    println!(
        "downloaded {:.1} MB in {elapsed:.2}s ({:.2} MB/s) over {reports} progress reports",
        total_seen as f32 / 1e6,
        total_seen as f32 / 1e6 / elapsed.max(0.001)
    );

    // 2. Everything the plan promised is on disk, at the size the plan promised, and
    //    nothing was left half-written.
    let plan = kiegen_lib::download::kokoro_plan().expect("plan");
    for entry in &plan {
        let path = root.join(&entry.path);
        let meta = std::fs::metadata(&path)
            .unwrap_or_else(|error| panic!("{} was not written: {error}", entry.path));
        assert_eq!(meta.len(), entry.bytes, "{} is the wrong size", entry.path);
        assert!(
            !path.with_extension("part").exists(),
            "{} left a .part file behind",
            entry.path
        );
    }
    assert_eq!(
        seen_files.len(),
        plan.len(),
        "every planned file must have been reported"
    );

    let voices = std::fs::read_dir(root.join("voices"))
        .expect("voices/")
        .flatten()
        .count();
    println!("{} files on disk, {voices} voice tables", plan.len());

    // 3. The app must now consider the engine installed. This is the exact predicate the
    //    UI's badge reads, so the two cannot disagree.
    assert!(
        kiegen_lib::engine_paths::kokoro_installed(),
        "kokoro_installed() must flip once the weights are present"
    );

    // 4. And the downloaded weights must actually load and make a sound.
    let mut engine = kiegen_lib::kokoro::Kokoro::load(
        &root.join("onnx/model.onnx"),
        &root.join("tokenizer.json"),
        &root.join("voices/af_heart.bin"),
        1.0,
    )
    .expect("the downloaded graph must load in ONNX Runtime");

    assert_eq!(
        engine.max_tokens(),
        509,
        "510 style rows leave 509 usable tokens; a different number means voices/af_heart.bin \
         is not the file this code was written against"
    );

    let synth_started = std::time::Instant::now();
    let (samples, dropped) = engine.synthesize(PHONEMES).expect("synthesize");
    let synth_elapsed = synth_started.elapsed().as_secs_f32();
    let seconds = samples.len() as f32 / kiegen_lib::kokoro::SAMPLE_RATE as f32;

    // An unknown word must never vanish silently: whatever was dropped has to be the
    // out-of-dictionary marker and nothing else.
    let unexpected: Vec<char> = dropped.iter().copied().filter(|c| *c != '❓').collect();
    assert!(
        unexpected.is_empty(),
        "real phonemes were dropped: {unexpected:?}"
    );

    assert!(
        seconds > 1.0,
        "produced {seconds:.2}s of audio, which is too little to be the sentence"
    );

    let wav = std::env::temp_dir().join("kiegen-install-it.wav");
    kiegen_lib::kokoro::write_wav(&wav, &samples, kiegen_lib::kokoro::SAMPLE_RATE)
        .expect("write the wav");

    println!(
        "synthesized {:.2}s of audio in {synth_elapsed:.2}s (RTF {:.3}); dropped {:?}",
        seconds,
        synth_elapsed / seconds,
        dropped
    );
    println!("wav {wav:?}");
}
