//! Text to phonemes, in Rust, without eSpeak.
//!
//! This is the front end Kokoro needs: the engine is phoneme-in, so something has to turn a
//! selection into IPA. The reference implementation is misaki's `en.py`, and this port is
//! measured against it — see `the_reference_corpus_matches` in the tests, which scores this
//! implementation against misaki's own output sentence by sentence.
//!
//! What is deliberately *better* than the reference: an unknown word is never deleted.
//! misaki emits `❓` for a word it cannot pronounce, and Kokoro has no token for `❓`, so the
//! word vanishes from the audio — the app's own name became "reads whatever you select"
//! instead of "kiegen reads whatever you select". Here an unknown word is spelled out with
//! the dictionary's letter names, which is audible and is exactly what the reference itself
//! does for acronyms.

use crate::lexicon::{apply_stress, Lexicon, Tag, TokenContext};

/// Symbols that stand for words. The reference rewrites these before looking anything up.
const SYMBOLS: [(&str, &str); 5] = [
    ("%", "percent"),
    ("&", "and"),
    ("+", "plus"),
    ("@", "at"),
    ("=", "equals"),
];

const CURRENCIES: [(&str, &str, &str); 3] = [
    ("$", "dollar", "cent"),
    ("£", "pound", "pence"),
    ("€", "euro", "cent"),
];

pub struct G2p {
    lexicon: Lexicon,
}

/// One token of the input, carrying everything the pipeline needs to pronounce it.
#[derive(Debug, Clone)]
struct Token {
    text: String,
    /// Whitespace that followed this token in the source, preserved so punctuation stays
    /// attached to the word before it.
    trailing_space: bool,
    /// Whether whitespace preceded it, which is what puts spaces between words.
    leading_space: bool,
    tag: Tag,
    phonemes: Option<String>,
    currency: Option<&'static str>,
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '\'' || c == '\u{2019}'
}

/// Splits text into word and punctuation tokens, recording the whitespace around each.
///
/// This is not spaCy's tokenizer. It splits the same way on ordinary prose, and it keeps a
/// contraction together ("don't" stays one token) rather than splitting off the clitic,
/// because the dictionary carries the contraction whole.
fn tokenize(text: &str) -> Vec<Token> {
    let mut tokens: Vec<Token> = Vec::new();
    let mut chars = text.chars().peekable();
    let mut pending_space = false;
    let mut at_start = true;

    while let Some(c) = chars.next() {
        if c.is_whitespace() {
            pending_space = true;
            if let Some(last) = tokens.last_mut() {
                last.trailing_space = true;
            }
            continue;
        }

        let leading_space = pending_space || at_start;
        pending_space = false;

        if is_word_char(c) {
            let mut word = String::from(c);
            // A word may contain apostrophes and digits: "don't", "1990s", "o'clock".
            while let Some(&next) = chars.peek() {
                if is_word_char(next) {
                    word.push(next);
                    chars.next();
                } else if next == '.' {
                    // Only a decimal point stays inside a word: "3.5", not "dog."
                    let mut lookahead = chars.clone();
                    lookahead.next();
                    if lookahead.peek().is_some_and(|c| c.is_ascii_digit())
                        && word.chars().all(|c| c.is_ascii_digit() || c == ',')
                    {
                        word.push(next);
                        chars.next();
                    } else {
                        break;
                    }
                } else {
                    break;
                }
            }
            tokens.push(Token {
                text: word,
                trailing_space: false,
                leading_space,
                tag: Tag::None,
                phonemes: None,
                currency: None,
            });
        } else {
            // Punctuation to its own token. Consecutive marks like "?!" stay together.
            let mut punct = String::from(c);
            while let Some(&next) = chars.peek() {
                if !is_word_char(next) && !next.is_whitespace() {
                    punct.push(next);
                    chars.next();
                } else {
                    break;
                }
            }
            tokens.push(Token {
                text: punct,
                trailing_space: false,
                leading_space,
                tag: Tag::Punct,
                phonemes: None,
                currency: None,
            });
        }
        at_start = false;
    }

    tokens
}

/// The tags this port can decide without a tagger. Everything else stays `None`, which the
/// lexicon resolves to the dictionary's `DEFAULT` pronunciation.
fn tag_for(text: &str, previous: Option<&Token>) -> Tag {
    if text
        .chars()
        .all(|c| c.is_ascii_digit() || c == ',' || c == '.')
    {
        return Tag::Cd;
    }
    match text {
        "a" | "an" | "the" | "A" | "An" | "The" | "THE" | "A" | "AN" => Tag::Dt,
        "I" => Tag::Prp,
        "to" | "To" | "TO" => Tag::To,
        "in" | "In" | "IN" | "vs" | "vs." | "Vs" | "VS" => Tag::In,
        _ => {
            // A word that is entirely upper case and longer than one letter reads as an
            // acronym or a proper noun: "NASA", "EBITDA", but not "I".
            if text.chars().count() > 1 && text.chars().all(|c| c.is_uppercase()) {
                return Tag::Nnp;
            }
            let _ = previous;
            Tag::None
        }
    }
}

impl G2p {
    pub fn load(gold_json: &str, silver_json: &str) -> Result<Self, String> {
        Ok(G2p {
            lexicon: Lexicon::load(gold_json, silver_json)?,
        })
    }

    /// Loads the dictionaries from disk, preferring `KIEGEN_G2P_DIR` when set.
    pub fn from_dir(dir: &std::path::Path) -> Result<Self, String> {
        let gold = std::fs::read_to_string(dir.join("us_gold.json"))
            .map_err(|e| format!("read us_gold.json: {e}"))?;
        let silver = std::fs::read_to_string(dir.join("us_silver.json"))
            .map_err(|e| format!("read us_silver.json: {e}"))?;
        Self::load(&gold, &silver)
    }

    pub fn lexicon(&self) -> &Lexicon {
        &self.lexicon
    }

    /// The one entry point: a string to IPA, ready for the Kokoro engine.
    pub fn phonemize(&self, text: &str) -> String {
        let mut tokens = tokenize(text);
        if tokens.is_empty() {
            return String::new();
        }

        // Attach a currency sign to the number that follows it, the way the reference does.
        let mut pending_currency: Option<&'static str> = None;
        for token in tokens.iter_mut() {
            if token.tag == Tag::Punct {
                if let Some((_, _, _)) = CURRENCIES.iter().find(|(s, _, _)| *s == token.text) {
                    pending_currency = CURRENCIES
                        .iter()
                        .find(|(s, _, _)| *s == token.text)
                        .map(|(_, singular, _)| *singular);
                    // The sign itself is silent; the unit is spoken after the number.
                    token.phonemes = Some(String::new());
                    continue;
                }
            } else if token.currency.is_none() {
                if let Some(singular) = pending_currency.take() {
                    token.currency = Some(singular);
                }
            }
        }

        // Tags, then phonemes. Tags are decided from the word alone; the neighbours only
        // matter for the symbols.
        let preceding: Vec<Option<Token>> = (0..tokens.len())
            .map(|i| {
                if i == 0 {
                    None
                } else {
                    Some(tokens[i - 1].clone())
                }
            })
            .collect();
        for i in 0..tokens.len() {
            if tokens[i].tag == Tag::None {
                tokens[i].tag = tag_for(&tokens[i].text, preceding[i].as_ref());
            }
        }

        // Right to left, because a token's pronunciation depends on whether the *next* one
        // starts with a vowel: "the apple" is ði, "the dog" is ðə.
        let mut ctx = TokenContext::default();
        for i in (0..tokens.len()).rev() {
            if tokens[i].phonemes.is_some() {
                let ps = tokens[i].phonemes.clone();
                ctx.advance(ps.as_deref(), tokens[i].tag == Tag::To);
                continue;
            }

            let text = tokens[i].text.clone();
            let tag = tokens[i].tag;
            let stress = if text.chars().all(|c| !c.is_uppercase()) {
                None
            } else if text.chars().all(|c| c.is_uppercase()) {
                Some(2.0)
            } else {
                Some(0.5)
            };

            let phonemes = if tag == Tag::Punct {
                punctuation_phonemes(&text)
            } else if tag == Tag::Cd {
                self.lexicon
                    .number(&text)
                    .map(|(ps, _)| ps)
                    .or_else(|| self.spelled(&text))
            } else {
                self.lexicon
                    .word(&text, tag, stress, &ctx)
                    .map(|(ps, _)| ps)
                    .or_else(|| symbol_phonemes(&self.lexicon, &text, &ctx))
                    .or_else(|| self.lexicon.number(&text).map(|(ps, _)| ps))
                    .or_else(|| self.spelled(&text))
            };

            let phonemes = phonemes.unwrap_or_default();
            ctx.advance(Some(&phonemes), tag == Tag::To);
            tokens[i].phonemes = Some(phonemes);
        }

        // Join. A space goes before a token only when the source had whitespace there, which
        // keeps "dog." as one run and "twenty-one" as one word.
        let mut out = String::new();
        for (i, token) in tokens.iter().enumerate() {
            let phonemes = token.phonemes.as_deref().unwrap_or("");
            if i > 0 && token.leading_space && !out.is_empty() && !out.ends_with(' ') {
                out.push(' ');
            }
            out.push_str(phonemes);
            if let Some(unit) = token.currency {
                if !phonemes.is_empty() && !unit.is_empty() {
                    out.push(' ');
                    if let Some(Entry) = self.lexicon.word(unit, Tag::None, None, &ctx) {
                        out.push_str(&Entry.0);
                    }
                }
            }
        }
        // Collapse the runs of spaces that punctuation and silent currency signs leave.
        out.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// Spells a word with the dictionary's letter names. This is the last resort, and the
    /// reason nothing is ever silently dropped.
    fn spelled(&self, text: &str) -> Option<String> {
        self.lexicon.spell_or_give_up(text).map(|(ps, _)| ps)
    }
}

/// A symbol such as `%` or `&`, looked up as the word it stands for.
fn symbol_phonemes(lexicon: &Lexicon, text: &str, ctx: &TokenContext) -> Option<String> {
    let word = SYMBOLS
        .iter()
        .find(|(symbol, _)| *symbol == text)
        .map(|(_, word)| *word)?;
    lexicon.word(word, Tag::None, None, ctx).map(|(ps, _)| ps)
}

/// Punctuation is carried through so the engine can shape the phrase with it. Anything the
/// engine has no token for is dropped here rather than being passed on as a marker.
fn punctuation_phonemes(text: &str) -> Option<String> {
    const KEPT: &str = ".,!?;:—…'\"";
    let kept: String = text.chars().filter(|c| KEPT.contains(*c)).collect();
    Some(kept)
}
