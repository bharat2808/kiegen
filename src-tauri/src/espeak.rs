//! The espeak-ng front end: arm's length, subprocess only.
//!
//! Kokoro has **no text front end of its own** for Spanish, French, Hindi, Italian and
//! Brazilian Portuguese. Upstream routes those five through espeak-ng, and nothing else
//! exists (`hexgrad/kokoro`'s `pipeline.py` falls through to
//! `espeak.EspeakG2P(language=LANG_CODES[lang_code])` for every language that is not English,
//! Japanese or Mandarin). espeak-ng is GPL-3.0, so it can never be bundled, linked or
//! vendored into this app.
//!
//! What this module does instead is **use** an install the user made: one subprocess per
//! text chunk, text in, IPA on stdout, then the same post-processing misaki applies on top.
//! Nothing here links the espeak-ng library — that is the actual copyleft trigger, and it is
//! why upstream's own path (`espeakng_loader.get_library_path()` feeding phonemizer) cannot
//! be copied wholesale.
//!
//! Three upstream behaviours are reproduced here, each of which changes the output:
//!
//! 1. **The tie character.** espeak writes a tie between the halves of one phoneme — `t͡ʃ` —
//!    and the tie character is a parameter. Upstream's phonemizer asks espeak for U+0361 and
//!    then rewrites it to whatever the caller requested: misaki passes `tie='^'` and writes
//!    its mapping table against the result (`'t^ʃ': 'ʧ'`). A plain `--ipa` yields `tʃ`, and
//!    every affricate and diphthong silently misses the table.
//! 2. **Punctuation is hidden from espeak and stitched back afterwards.** phonemizer's
//!    `preserve_punctuation=True` is *not* an espeak setting: it splits the line at the
//!    punctuation, phonemizes the bare chunks, then re-inserts the marks carrying the
//!    position they held. espeak alone cannot produce this, which is why `preserve`/`restore`
//!    are ported below rather than approximated with a CLI flag.
//! 3. **Bracket shuffling.** misaki swaps `«»` to curly quotes and `(`/`)` to `«»` before
//!    phonemizing, and back afterwards, so parentheses travel through the punctuation
//!    machinery above instead of being read as espeak clause markers.

use std::path::{Path, PathBuf};
use std::process::Command;

/// espeak IPA → Kokoro's phoneme inventory, ported from misaki's `EspeakG2P.e2m`
/// (Apache-2.0). Applied to the same text, in the same order: misaki sorts the mapping by
/// key, and that order is preserved here so a future key that *is* a prefix of another
/// cannot silently change behaviour.
const E2M: &[(&str, &str)] = &[
    ("a^ɪ", "I"),
    ("a^ʊ", "W"),
    ("d^z", "ʣ"),
    ("d^ʒ", "ʤ"),
    ("e^ɪ", "A"),
    ("o^ʊ", "O"),
    ("s^s", "S"),
    ("t^s", "ʦ"),
    ("t^ʃ", "ʧ"),
    ("ɔ^ɪ", "Y"),
    ("ə^ʊ", "Q"),
];

/// phonemizer's `_DEFAULT_MARKS`. That `«»` and the brackets are in here is exactly why
/// upstream shuffles the brackets before phonemizing them.
const PUNCTUATION_MARKS: &[char] = &[
    ';', ':', ',', '.', '!', '?', '¡', '¿', '—', '…', '"', '«', '»', '“', '”', '(', ')', '{', '}',
    '[', ']',
];

/// phonemizer's `Separator(word=' ')`, which `EspeakG2P` gets by not passing one.
const WORD_SEPARATOR: &str = " ";

/// A usable `espeak-ng`, found rather than shipped.
pub struct EspeakNg {
    binary: PathBuf,
    /// Passed as `ESPEAK_DATA_PATH` when known. A relocated binary fails with a message
    /// about voices rather than anything actionable without it.
    data: Option<PathBuf>,
}

impl EspeakNg {
    /// Detect an install, or `None`. Never downloads anything.
    pub fn detect() -> Option<Self> {
        let binary = crate::engine_paths::espeak_ng()?;
        let data = crate::engine_paths::espeak_data_dir(&binary);
        Some(Self { binary, data })
    }

    pub fn binary(&self) -> &Path {
        &self.binary
    }

    /// Text to phonemes, in the inventory Kokoro was trained on.
    ///
    /// `voice` is an espeak-ng voice name, and for these five languages it is the same string
    /// Kokoro's own `LANG_CODES` uses: `es`, `fr-fr`, `hi`, `it`, `pt-br`.
    pub fn phonemize(&self, text: &str, voice: &str) -> Result<String, String> {
        if text.trim().is_empty() {
            return Ok(String::new());
        }

        let (chunks, marks) = preserve(&swap_brackets_in(text));
        let mut phonemized = Vec::with_capacity(chunks.len());
        for chunk in &chunks {
            phonemized.push(self.phonemize_chunk(chunk, voice)?);
        }

        // misaki takes `ps[0]` of the list phonemize returns. For a single input line the
        // restore step collapses to one element; taking the first mirrors upstream even if
        // some future input makes it emit more.
        let restored = restore(phonemized, marks)
            .into_iter()
            .next()
            .unwrap_or_default();

        Ok(swap_brackets_out(&clean(&restored)))
    }

    /// One espeak-ng call. Returns the chunk's phonemes followed by the word separator,
    /// which is the shape phonemizer hands to `restore`.
    fn phonemize_chunk(&self, chunk: &str, voice: &str) -> Result<String, String> {
        let mut command = Command::new(&self.binary);
        command
            .arg("-q") // no audio: phonemes only
            .arg("--ipa") // IPA rather than espeak's internal mnemonic alphabet
            .arg("--tie=^") // see the module note: without this every affricate misses E2M
            .arg("-v")
            .arg(voice)
            .arg(chunk);
        if let Some(data) = &self.data {
            command.env("ESPEAK_DATA_PATH", data);
        }

        let output = command
            .output()
            .map_err(|e| format!("run {}: {e}", self.binary.display()))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!(
                "espeak-ng exited {}: {}",
                output.status,
                stderr.trim()
            ));
        }

        // espeak breaks its output across lines and phonemizer joins them with the word
        // separator; splitting on whitespace and rejoining does that and the double-space
        // collapse in one step.
        let raw = String::from_utf8_lossy(&output.stdout);
        let mut words = raw
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(WORD_SEPARATOR);
        // phonemizer rewrites espeak's default tie to the requested one. With `--tie=^`
        // espeak already emits `^`, so this is a no-op — it stays because it makes the tie
        // assumption explicit rather than incidental.
        words = words.replace('\u{361}', "^");
        words.push_str(WORD_SEPARATOR);
        Ok(words)
    }
}

// ────────────────────────────── post-processing ──────────────────────────────

/// Everything misaki does to the phoneme string after phonemizing.
///
/// Public so the parity harness and the unit tests can drive it without a binary present.
pub fn clean(phonemes: &str) -> String {
    let mut out = phonemes.trim().to_string();
    for (from, to) in E2M {
        if out.contains(from) {
            out = out.replace(from, to);
        }
    }
    // What remains of the tie markers, and the hyphens espeak inserts between words: both
    // are punctuation to the model, not phonemes.
    out.replace(['^', '-'], "")
}

fn swap_brackets_in(text: &str) -> String {
    text.replace('«', "\u{201c}")
        .replace('»', "\u{201d}")
        .replace('(', "«")
        .replace(')', "»")
}

fn swap_brackets_out(text: &str) -> String {
    text.replace('«', "(").replace('»', ")")
}

// ────────────────────────── phonemizer's punctuation ─────────────────────────

/// Where a mark sat relative to the chunk it was split from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Position {
    /// The mark began the line.
    Begin,
    /// The mark ended the line.
    End,
    /// The mark sat between chunks.
    Inside,
    /// The line was nothing but marks.
    Alone,
}

/// A mark to re-insert, and enough context to place it. `index` is the input line it came
/// from; this module always phonemizes one line at a time, so it is always 0 — it exists
/// because the restore step branches on it, and dropping it would misread as a simplification
/// rather than a fixed precondition.
#[derive(Clone, Debug)]
struct Mark {
    index: usize,
    mark: String,
    position: Position,
}

fn is_mark(c: char) -> bool {
    PUNCTUATION_MARKS.contains(&c)
}

fn char_at(text: &str, byte: usize) -> Option<char> {
    text.get(byte..)?.chars().next()
}

fn advance(text: &str, byte: usize) -> usize {
    byte + char_at(text, byte).map_or(0, char::len_utf8)
}

fn skip_spaces(text: &str, mut byte: usize) -> usize {
    while let Some(c) = char_at(text, byte) {
        if c.is_whitespace() {
            byte = advance(text, byte);
        } else {
            break;
        }
    }
    byte
}

/// The ranges matched by phonemizer's `(\s*[marks]+\s*)+`, found greedily left to right.
///
/// Hand-rolled rather than pulling in the `regex` crate: the pattern is this small, and every
/// dependency added here also has to be justified to `check-licenses.sh`. The greedy
/// behaviour is the point — `, ¿` is one match, not two — because the mark is re-inserted
/// verbatim, spaces and all.
fn mark_ranges(line: &str) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut at = 0;
    while at < line.len() {
        match match_marks_at(line, at) {
            Some(end) if end > at => {
                ranges.push((at, end));
                at = end;
            }
            _ => at = advance(line, at),
        }
    }
    ranges
}

/// One attempt at `(\s*[marks]+\s*)+` starting at `start`, consuming repetitions greedily.
/// `None` when the match would contain no mark at all, which is how the regex engine rejects
/// a position as a start.
fn match_marks_at(line: &str, start: usize) -> Option<usize> {
    let mut position = start;
    let mut marks_seen = 0usize;
    loop {
        let after_spaces = skip_spaces(line, position);
        let mut after_marks = after_spaces;
        while let Some(c) = char_at(line, after_marks) {
            if is_mark(c) {
                after_marks = advance(line, after_marks);
            } else {
                break;
            }
        }
        if after_marks == after_spaces {
            break; // no mark in this iteration, so the repetition ends here
        }
        marks_seen += 1;
        position = skip_spaces(line, after_marks);
    }
    (marks_seen > 0).then_some(position)
}

/// phonemizer's `Punctuation._preserve_line`: hide the marks from the backend, remembering
/// enough to put them back.
fn preserve(line: &str) -> (Vec<String>, Vec<Mark>) {
    let ranges = mark_ranges(line);
    if ranges.is_empty() {
        return (vec![line.to_string()], Vec::new());
    }

    // A line that is nothing but marks produces no chunk at all.
    if ranges.len() == 1 && &line[ranges[0].0..ranges[0].1] == line {
        return (
            Vec::new(),
            vec![Mark {
                index: 0,
                mark: line.to_string(),
                position: Position::Alone,
            }],
        );
    }

    let marks: Vec<Mark> = ranges
        .iter()
        .enumerate()
        .map(|(i, (start, end))| {
            let group = &line[*start..*end];
            let position = if i == 0 && line.starts_with(group) {
                Position::Begin
            } else if i == ranges.len() - 1 && line.ends_with(group) {
                Position::End
            } else {
                Position::Inside
            };
            Mark {
                index: 0,
                mark: group.to_string(),
                position,
            }
        })
        .collect();

    // Split the line into the chunks the backend actually sees.
    let mut chunks = Vec::new();
    let mut remaining = line.to_string();
    for mark in &marks {
        let split: Vec<&str> = remaining.split(mark.mark.as_str()).collect();
        chunks.push(split[0].to_string());
        remaining = split[1..].join(mark.mark.as_str());
    }
    chunks.push(remaining);

    (
        chunks.into_iter().filter(|c| !c.is_empty()).collect(),
        marks,
    )
}

/// phonemizer's `Punctuation.restore`: re-insert the marks between the phonemized chunks.
fn restore(mut chunks: Vec<String>, mut marks: Vec<Mark>) -> Vec<String> {
    let mut out = Vec::new();
    let mut position = 0usize;

    while !chunks.is_empty() || !marks.is_empty() {
        if marks.is_empty() {
            // Nothing left to re-insert: hand back what remains, separator-terminated.
            for chunk in chunks.iter() {
                let mut chunk = chunk.clone();
                if !chunk.ends_with(WORD_SEPARATOR) {
                    chunk.push_str(WORD_SEPARATOR);
                }
                out.push(chunk);
            }
            chunks.clear();
        } else if chunks.is_empty() {
            // Nothing was phonemized at all, so the marks stand alone.
            let joined: String = marks.iter().map(|m| m.mark.as_str()).collect();
            out.push(joined.replace(' ', WORD_SEPARATOR));
            marks.clear();
        } else if marks[0].index == position {
            let Mark {
                mark,
                position: kind,
                ..
            } = marks.remove(0);
            let mark = mark.replace(' ', WORD_SEPARATOR);
            // The chunk already ends with the word separator; drop it so the mark butts up
            // against the last phoneme.
            if chunks[0].ends_with(WORD_SEPARATOR) {
                let keep = chunks[0].len() - WORD_SEPARATOR.len();
                chunks[0].truncate(keep);
            }
            match kind {
                Position::Begin => chunks[0] = format!("{mark}{}", chunks[0]),
                Position::End => {
                    let mut line = format!("{}{mark}", chunks[0]);
                    if !mark.ends_with(WORD_SEPARATOR) {
                        line.push_str(WORD_SEPARATOR);
                    }
                    out.push(line);
                    chunks.remove(0);
                    position += 1;
                }
                Position::Alone => {
                    let mut line = mark;
                    if !line.ends_with(WORD_SEPARATOR) {
                        line.push_str(WORD_SEPARATOR);
                    }
                    out.push(line);
                    position += 1;
                }
                Position::Inside => {
                    if chunks.len() == 1 {
                        chunks[0] = format!("{}{mark}", chunks[0]);
                    } else {
                        let first = chunks.remove(0);
                        chunks[0] = format!("{first}{mark}{}", chunks[0]);
                    }
                }
            }
        } else {
            out.push(chunks.remove(0));
            position += 1;
        }
    }

    out
}

// ──────────────────────────────── installing it ───────────────────────────────

/// Homebrew's executable, if one is installed. Apple Silicon prefix, then Intel.
fn homebrew() -> Option<PathBuf> {
    ["/opt/homebrew/bin/brew", "/usr/local/bin/brew"]
        .iter()
        .map(PathBuf::from)
        .find(|candidate| candidate.is_file())
}

/// Where someone without a package manager is sent to read about installing espeak-ng.
const ESPEAK_UPSTREAM: &str = "https://github.com/espeak-ng/espeak-ng#installation";

/// Install espeak-ng by asking the user's own package manager to do it.
///
/// This is the only action in kiegen that results in GPL-3.0 software arriving on the
/// machine, and it is deliberately the *user's* action: the app runs their package manager,
/// which fetches from upstream and accepts the licence under their own settings. Nothing is
/// downloaded into the app's directory, and nothing is copied afterwards — kiegen only ever
/// execs the install that is already there, which is what keeps it a user of espeak-ng
/// rather than a distributor of it.
///
/// With no package manager it opens upstream's page instead of fetching a copy itself, for
/// exactly that reason: fetching one would make this app the distributor.
pub fn install() -> Result<String, String> {
    let Some(brew) = homebrew() else {
        std::process::Command::new("/usr/bin/open")
            .arg(ESPEAK_UPSTREAM)
            .status()
            .map_err(|e| format!("could not open a browser: {e}"))?;
        return Ok("Opened espeak-ng's install instructions".to_string());
    };

    let output = std::process::Command::new(&brew)
        .args(["install", "espeak-ng"])
        .output()
        .map_err(|e| format!("could not run {}: {e}", brew.display()))?;

    if output.status.success() {
        // Re-detect rather than trusting the exit code: the point of the whole exercise is
        // that the app only uses what it can actually find.
        return match EspeakNg::detect() {
            Some(engine) => Ok(format!(
                "Installed — ready at {}",
                engine.binary().display()
            )),
            None => Err("brew reported success but espeak-ng is still not found".to_string()),
        };
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    let detail = stderr
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("no output");
    Err(format!("brew install espeak-ng failed: {detail}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn affricates_and_diphthongs_map_to_kokoro_symbols() {
        // The exact shape espeak produces with `--tie=^`, taken from misaki's own table.
        let cases = [
            ("t^ʃ", "ʧ"),
            ("d^ʒ", "ʤ"),
            ("a^ɪ", "I"),
            ("a^ʊ", "W"),
            ("e^ɪ", "A"),
            ("o^ʊ", "O"),
            ("ə^ʊ", "Q"),
            ("ɔ^ɪ", "Y"),
            ("t^s", "ʦ"),
            ("d^z", "ʣ"),
            ("s^s", "S"),
        ];
        for (from, to) in cases {
            assert_eq!(clean(from), to, "{from} should become {to}");
        }
    }

    #[test]
    fn tie_and_hyphen_markers_do_not_reach_the_model() {
        // A tie that matched nothing in the table still has to go: Kokoro was never trained
        // on '^', and it is not in the vocabulary.
        assert_eq!(clean("k^a"), "ka");
        assert_eq!(clean("bʎ-ˈa"), "bʎˈa");
        assert!(!clean("t^ʃ ɔ^ɪ").contains('^'));
    }

    #[test]
    fn the_tie_character_phonemizer_actually_emits_is_replaced_not_left_behind() {
        // phonemizer rewrites U+0361 to the caller's tie. Feeding the pre-rewrite shape
        // straight to `clean` is *not* the same thing, and this pins that down: the mapping
        // table only matches the rewritten form, so the chunk step has to do the rewrite.
        assert_eq!(clean("t\u{361}ʃ"), "t\u{361}ʃ");
        assert_ne!(clean("t\u{361}ʃ"), "ʧ");
        assert_eq!(clean(&"t\u{361}ʃ".replace('\u{361}', "^")), "ʧ");
    }

    #[test]
    fn a_matching_run_of_marks_is_one_mark_not_two() {
        // The greedy detail that decides the output: `, ¿` is re-inserted verbatim.
        let line = "Hola mundo, ¿cómo estás?";
        let groups: Vec<&str> = mark_ranges(line)
            .iter()
            .map(|(start, end)| &line[*start..*end])
            .collect();
        assert_eq!(groups, vec![", ¿", "?"]);
    }

    #[test]
    fn preserve_keeps_only_the_text_chunks() {
        let (chunks, marks) = preserve("Hola mundo, ¿cómo estás?");
        assert_eq!(chunks, vec!["Hola mundo", "cómo estás"]);
        assert_eq!(marks.len(), 2);
        assert!(marks.iter().all(|m| m.index == 0));
    }

    #[test]
    fn a_line_of_only_marks_produces_no_chunk() {
        let (chunks, marks) = preserve("...");
        assert!(chunks.is_empty());
        assert_eq!(marks.len(), 1);
        assert_eq!(marks[0].position, Position::Alone);
    }

    #[test]
    fn beginning_and_end_marks_are_recognised() {
        let (chunks, marks) = preserve("¡Qué día!");
        assert_eq!(chunks, vec!["Qué día"]);
        assert_eq!(marks[0].position, Position::Begin);
        assert_eq!(marks[1].position, Position::End);
    }

    #[test]
    fn marks_are_stitched_back_between_chunks() {
        // The shape the oracle produces for this sentence, driven by hand.
        let restored = restore(
            vec!["ˈola mˈundo ".to_string(), "kˈomo estˈas ".to_string()],
            vec![
                Mark {
                    index: 0,
                    mark: ", ¿".to_string(),
                    position: Position::Inside,
                },
                Mark {
                    index: 0,
                    mark: "?".to_string(),
                    position: Position::End,
                },
            ],
        );
        assert_eq!(restored.concat().trim(), "ˈola mˈundo, ¿kˈomo estˈas?");
    }

    #[test]
    fn a_leading_mark_butts_against_the_first_chunk() {
        let restored = restore(
            vec!["kˈe ðˈia ".to_string()],
            vec![Mark {
                index: 0,
                mark: "¡".to_string(),
                position: Position::Begin,
            }],
        );
        assert_eq!(restored.concat().trim(), "¡kˈe ðˈia");
    }

    #[test]
    fn a_sentence_without_punctuation_keeps_its_own_chunk() {
        let (chunks, marks) = preserve("La niña juega");
        assert_eq!(chunks, vec!["La niña juega"]);
        assert!(marks.is_empty());
    }

    #[test]
    fn brackets_survive_the_round_trip() {
        // A parenthesis must come back out as a parenthesis, not as the angle bracket the
        // shuffle substitutes for it — that is the whole point of the shuffle.
        assert_eq!(swap_brackets_out(&swap_brackets_in("(hola)")), "(hola)");
        // Angle quotes are *not* symmetric, and that is upstream's behaviour, not an
        // oversight here: misaki maps « to a curly quote on the way in and only maps « back
        // to a parenthesis on the way out, so an « in the source text leaves as “.
        assert_eq!(
            swap_brackets_out(&swap_brackets_in("«hola»")),
            "\u{201c}hola\u{201d}"
        );
    }

    #[test]
    fn clean_trims_but_does_not_collapse_inner_whitespace() {
        // Collapsing belongs to the per-chunk step, which is where phonemizer does it. If
        // `clean` also collapsed, the chunk step could not be tested in isolation and any
        // future divergence between the two would be invisible.
        assert_eq!(clean("  ola\n  mundo  "), "ola\n  mundo");
        assert_eq!(clean(""), "");
        assert_eq!(clean("  ˈola mˈundo  "), "ˈola mˈundo");
    }

    #[test]
    fn detection_never_invents_a_binary() {
        // Whatever this machine has, the function must agree with the filesystem: a path was
        // only returned if something is actually there.
        if let Some(binary) = crate::engine_paths::espeak_ng() {
            assert!(
                binary.is_file(),
                "{binary:?} was returned but does not exist"
            );
        }
    }
}
