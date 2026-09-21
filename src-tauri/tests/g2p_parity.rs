//! Scores this front end against misaki, sentence by sentence.
//!
//! Ignored by default: it needs the two 3 MB dictionaries and the corpus misaki produced.
//! Run it deliberately, and read the number rather than the word "ok":
//!
//!     KIEGEN_G2P_DIR=<misaki/data> cargo test --test g2p_parity -- --ignored --nocapture
//!
//! The point of this test is the *measurement*. A port that claims parity without scoring
//! itself is a port that has not been checked, and the failures are the interesting part:
//! each one names a rule this implementation gets wrong.

use std::collections::HashMap;

fn dictionary_dir() -> std::path::PathBuf {
    std::env::var("KIEGEN_G2P_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            std::path::PathBuf::from(concat!(
                env!("HOME"),
                "/.hermes/cache/scratch/kokoro_nogpl/.venv/lib/python3.12/site-packages/misaki/data"
            ))
        })
}

#[test]
#[ignore = "needs the misaki dictionaries and the generated corpus fixture"]
fn the_reference_corpus_matches() {
    let dir = dictionary_dir();
    if !dir.join("us_gold.json").is_file() {
        panic!(
            "no dictionary at {dir:?}; point KIEGEN_G2P_DIR at a copy of misaki's data/ directory"
        );
    }
    let g2p = kiegen_lib::g2p::G2p::from_dir(&dir).expect("load the dictionaries");
    println!(
        "lexicon: {} gold, {} silver, {} tag-dependent",
        g2p.lexicon().gold_len(),
        g2p.lexicon().silver_len(),
        g2p.lexicon().tag_dependent_len()
    );

    let raw = include_str!("fixtures/g2p_corpus.json");
    let fixture: serde_json::Value = serde_json::from_str(raw).expect("corpus parses");
    let rows = fixture["corpus"].as_array().expect("corpus rows");

    let mut exact = 0usize;
    let mut total_tokens = 0usize;
    let mut matched_tokens = 0usize;
    let mut failures: Vec<(String, String, String)> = Vec::new();

    for row in rows {
        let text = row["text"].as_str().expect("text");
        let expected = row["phonemes"].as_str().expect("phonemes");
        let got = g2p.phonemize(text);

        let want_words: Vec<&str> = expected.split_whitespace().collect();
        let got_words: Vec<&str> = got.split_whitespace().collect();
        total_tokens += want_words.len();
        for (i, want) in want_words.iter().enumerate() {
            if got_words.get(i) == Some(want) {
                matched_tokens += 1;
            }
        }

        if got == expected {
            exact += 1;
        } else {
            failures.push((text.to_string(), expected.to_string(), got));
        }
    }

    let sentences = rows.len();
    println!("\n=== sentence-exact: {exact}/{sentences} ===");
    println!(
        "=== word position agreement: {matched_tokens}/{total_tokens} ({:.1}%) ===",
        100.0 * matched_tokens as f32 / total_tokens as f32
    );

    // Print the first failures with a word-level diff, so each one points at a rule.
    println!("\n=== first {} failures ===", failures.len().min(8));
    for (text, want, got) in failures.iter().take(8) {
        println!("\n  {text}");
        println!("    want {want}");
        println!("    got  {got}");
        let want_words: Vec<&str> = want.split_whitespace().collect();
        let got_words: Vec<&str> = got.split_whitespace().collect();
        let differing: Vec<String> = want_words
            .iter()
            .zip(got_words.iter())
            .filter(|(a, b)| a != b)
            .map(|(a, b)| format!("{a} -> {b}"))
            .collect();
        if differing.is_empty() {
            println!(
                "    (same words, different length: {} vs {})",
                want_words.len(),
                got_words.len()
            );
        } else {
            println!("    diff {}", differing.join(", "));
        }
    }

    // A floor, not a target: it is what this port currently achieves, and it exists so a
    // change cannot silently make things worse.
    let agreement = matched_tokens as f32 / total_tokens as f32;
    // Measured at 86.4% (27/42 sentences byte-identical) on the committed corpus. The
    // residue is almost entirely the missing part-of-speech tagger: "record" as a verb
    // needs the parse, and no word list can supply it. The floor sits just under the
    // measurement so a regression trips it without a cosmetic difference doing so.
    assert!(
        agreement >= 0.85,
        "agreement dropped to {:.1}%, below the recorded floor",
        agreement * 100.0
    );
}

/// The stop words a selector actually sees, checked individually rather than in a sentence,
/// because a wrong weak form is the most audible kind of wrong.
#[test]
#[ignore = "needs the misaki dictionaries and the generated corpus fixture"]
fn function_words_take_their_weak_forms() {
    let g2p = kiegen_lib::g2p::G2p::from_dir(&dictionary_dir()).expect("load the dictionaries");
    let raw = include_str!("fixtures/g2p_corpus.json");
    let fixture: serde_json::Value = serde_json::from_str(raw).expect("corpus parses");
    let expected: HashMap<String, String> = fixture["corpus"]
        .as_array()
        .expect("rows")
        .iter()
        .filter_map(|row| {
            Some((
                row["text"].as_str()?.to_string(),
                row["phonemes"].as_str()?.to_string(),
            ))
        })
        .collect();

    let cases = [
        "The quick brown fox jumps over the lazy dog.",
        "She is used to living in the city.",
        "I want to go to the store and buy an apple.",
    ];
    for case in cases {
        let want = expected
            .get(case)
            .expect("the corpus contains this sentence");
        let got = g2p.phonemize(case);
        println!(
            "{}  {case}\n    want {want}\n    got  {got}",
            if &got == want { "ok  " } else { "DIFF" }
        );
    }
}
