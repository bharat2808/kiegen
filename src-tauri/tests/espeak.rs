//! End-to-end checks for the espeak-ng front end, against a real install.
//!
//! These are the tests that would have caught the two mistakes that actually happened while
//! this was built: forgetting `--tie=^` (every affricate silently stops mapping) and
//! forgetting that punctuation is preserved by phonemizer in Python rather than by espeak.
//! Both produce *plausible* phonemes, so a test that only asserted "not empty" would pass
//! through either. The assertions below are on the shapes those bugs destroy.
//!
//! Skipped, loudly, when espeak-ng is not installed: it is a GPL-3.0 dependency the user
//! installs, so a machine without it is a supported configuration, not a broken one.

use kiegen_lib::espeak::EspeakNg;

fn engine_or_skip() -> Option<EspeakNg> {
    match EspeakNg::detect() {
        Some(engine) => Some(engine),
        None => {
            eprintln!("skipping: no espeak-ng installed (brew install espeak-ng)");
            None
        }
    }
}

/// The tie character must be rewritten and mapped, not passed through. `leche` contains the
/// affricate espeak writes as `t^ʃ`, which the mapping table turns into `ʧ` — and which
/// arrives as `tʃ` if the subprocess forgets `--tie=^`, matching nothing.
#[test]
fn an_affricate_is_mapped_the_way_upstream_maps_it() {
    let Some(engine) = engine_or_skip() else {
        return;
    };
    let phonemes = engine.phonemize("leche", "es").expect("phonemize");
    assert!(
        phonemes.contains('ʧ'),
        "expected the mapped affricate in {phonemes:?}"
    );
    assert!(
        !phonemes.contains('^'),
        "the tie marker must not reach the model: {phonemes:?}"
    );
}

/// Punctuation survives into the phoneme string, which is phonemizer's doing in Python and
/// therefore the part a pure subprocess would drop on the floor.
#[test]
fn punctuation_is_preserved_through_the_chunking() {
    let Some(engine) = engine_or_skip() else {
        return;
    };
    let phonemes = engine
        .phonemize("Hola mundo, ¿cómo estás?", "es")
        .expect("phonemize");
    assert!(phonemes.contains(','), "comma lost: {phonemes:?}");
    assert!(phonemes.contains('?'), "question mark lost: {phonemes:?}");
    assert!(phonemes.contains('¿'), "inverted mark lost: {phonemes:?}");
    // ...and the marks land where they belong rather than being appended somewhere.
    assert!(
        phonemes.contains("mˈundo,"),
        "the comma should butt against the word before it: {phonemes:?}"
    );
}

/// Affricates, diphthongs and punctuation all at once, in a language whose only front end is
/// espeak. This is the shape that gets synthesised, so if it is wrong the audio is wrong.
#[test]
fn a_portuguese_sentence_comes_back_as_kokoro_phonemes() {
    let Some(engine) = engine_or_skip() else {
        return;
    };
    let phonemes = engine
        .phonemize("O preço é 19,99 euros.", "pt-br")
        .expect("phonemize");
    assert!(!phonemes.trim().is_empty());
    assert!(
        phonemes.contains('.'),
        "the full stop should survive: {phonemes:?}"
    );
    assert!(!phonemes.contains('^'), "no tie markers: {phonemes:?}");
    // `19,99` is a clause break to this phonemizer, not a decimal: the pinned
    // phonemizer-fork has no decimal-separator exception (that upstream change is newer), so
    // the number is split, phonemized as two halves, and the comma put back between them.
    // The seam shows as a comma with no space either side — which is what upstream produces
    // too, and is why the parity check passes this line for both implementations.
    assert!(
        phonemes.contains(','),
        "the comma should survive: {phonemes:?}"
    );
    assert!(
        !phonemes.contains(", "),
        "the comma is rejoined mid-token, not a clause marker: {phonemes:?}"
    );
}

/// Every espeak-backed language the catalogue offers must actually produce phonemes — the
/// table in `engines` and the front end here have to agree on the voice names.
#[test]
fn every_espeak_backed_language_produces_phonemes() {
    let Some(engine) = engine_or_skip() else {
        return;
    };
    for (voice, text) in [
        ("es", "hola"),
        ("fr-fr", "bonjour"),
        ("hi", "नमस्ते"),
        ("it", "ciao"),
        ("pt-br", "olá"),
    ] {
        let phonemes = engine
            .phonemize(text, voice)
            .unwrap_or_else(|e| panic!("{voice} failed: {e}"));
        assert!(!phonemes.trim().is_empty(), "{voice} produced nothing");
    }
}

/// A bad voice name is an error the caller can report, not a panic and not empty audio.
#[test]
fn an_unknown_voice_is_an_error_rather_than_silence() {
    let Some(engine) = engine_or_skip() else {
        return;
    };
    let outcome = engine.phonemize("hola", "not-a-real-voice");
    // espeak may fall back to a default voice, in which case the phonemes are real output
    // and there is nothing to report; what must not happen is a panic or an empty string
    // that would synthesise as silence.
    if let Ok(phonemes) = outcome {
        assert!(!phonemes.trim().is_empty(), "fell back to silence");
    }
}
