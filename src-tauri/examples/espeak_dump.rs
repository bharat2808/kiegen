//! Prints this implementation's phonemes for a corpus, so the exhaustive parity check can
//! diff it against upstream's espeak front end without either side knowing how the other
//! works.
//!
//! Reads `language<TAB>text` lines on stdin and writes `language<TAB>phonemes` lines on
//! stdout. The language is an espeak-ng voice name (`es`, `fr-fr`, `hi`, `it`, `pt-br`).
//!
//!     cargo run --quiet --example espeak_dump -- < corpus.tsv > mine.tsv
//!
//! espeak-ng is found the same way the app finds it, so the harness fails loudly rather than
//! quietly emitting empty phonemes if the binary is missing.

use kiegen_lib::espeak::EspeakNg;
use std::io::{BufRead, Write};

fn main() {
    let Some(engine) = EspeakNg::detect() else {
        eprintln!("no espeak-ng found — set KIEGEN_ESPEAK_NG or install it");
        std::process::exit(2);
    };
    eprintln!("using {}", engine.binary().display());

    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line.expect("read a corpus line");
        if line.trim().is_empty() {
            continue;
        }
        let Some((language, text)) = line.split_once('\t') else {
            eprintln!("skipping a line with no tab: {line:?}");
            continue;
        };
        match engine.phonemize(text, language) {
            Ok(phonemes) => {
                writeln!(stdout, "{language}\t{phonemes}").expect("write a result line");
            }
            Err(e) => {
                writeln!(stdout, "{language}\t!ERROR {e}").expect("write a result line");
            }
        }
    }
}
