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

use crate::lexicon::{Lexicon, Tag, TokenContext, CURRENCIES};

/// Symbols that stand for words. The reference rewrites these before looking anything up.
const SYMBOLS: [(&str, &str); 5] = [
    ("%", "percent"),
    ("&", "and"),
    ("+", "plus"),
    ("@", "at"),
    ("=", "equals"),
];

/// Attaches each currency sign to the number that follows it. The sign itself is silent —
/// the unit is spoken after the amount.
fn attach_currency(tokens: &mut [Token]) {
    let mut pending: Option<&'static str> = None;
    for token in tokens.iter_mut() {
        if token.tag == Tag::Punct {
            if let Some((sign, _, _)) = CURRENCIES.iter().find(|(sign, _, _)| *sign == token.text) {
                pending = Some(sign);
                token.phonemes = Some(String::new());
            }
            continue;
        }
        // A sign only reaches a number if nothing else comes first.
        if let Some(sign) = pending.take() {
            token.currency = Some(sign);
        }
    }
}

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
    /// Set when the phonemes came from a symbol expansion ("%" -> "percent"), which must be
    /// separated from the number before it even though the source had no space there.
    force_space: bool,
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
                    // A decimal point or an abbreviation dot stays inside the token: "3.5"
                    // is one number and "U.S." is one word, but "dog." is a word followed by
                    // a full stop, and the stop belongs to the punctuation.
                    let mut lookahead = chars.clone();
                    lookahead.next();
                    let after = lookahead.peek().copied();
                    let all_numeric = word.chars().all(|c| c.is_ascii_digit() || c == ',');
                    let all_alpha = !word.is_empty() && word.chars().all(|c| c.is_alphabetic());
                    let continues_number = after.is_some_and(|c| c.is_ascii_digit()) && all_numeric;
                    let continues_word = after.is_some_and(|c| c.is_alphabetic()) && all_alpha;
                    if continues_number || continues_word {
                        word.push(next);
                        chars.next();
                    } else {
                        break;
                    }
                } else {
                    break;
                }
            }
            // A leading or trailing apostrophe is a quotation mark rather than part of the
            // word: "'hello'" is the word in quotes, while "don't" keeps its own. Without
            // this, a quoted word misses the dictionary and gets spelled out letter by
            // letter.
            let mut inner = word.as_str();
            let mut leading = String::new();
            let mut trailing = String::new();
            while let Some(rest) = inner.strip_prefix('\'') {
                leading.push('\'');
                inner = rest;
            }
            while let Some(rest) = inner.strip_suffix('\'') {
                trailing.push('\'');
                inner = rest;
            }
            let had_leading = !leading.is_empty();
            let mut push = |text: String, tag: Tag, leading_space: bool| {
                tokens.push(Token {
                    text,
                    trailing_space: false,
                    leading_space,
                    tag,
                    phonemes: None,
                    currency: None,
                    force_space: false,
                });
            };
            if !leading.is_empty() {
                push(leading, Tag::Punct, leading_space);
            }
            if !inner.is_empty() {
                // The word keeps the space only if no quote already took it.
                push(inner.to_string(), Tag::None, !had_leading && leading_space);
            }
            if !trailing.is_empty() {
                push(trailing, Tag::Punct, false);
            }
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
                force_space: false,
            });
        }
        at_start = false;
    }

    tokens
}

/// The tags this port can decide without a tagger. Everything else stays `None`, which the
/// lexicon resolves to the dictionary's `DEFAULT` pronunciation.
fn tag_for(text: &str) -> Tag {
    if text
        .chars()
        .all(|c| c.is_ascii_digit() || c == ',' || c == '.')
    {
        return Tag::Cd;
    }
    match text {
        "a" | "an" | "the" | "A" | "An" | "The" | "THE" | "AN" => Tag::Dt,
        "I" => Tag::Prp,
        "to" | "To" | "TO" => Tag::To,
        "in" | "In" | "IN" | "vs" | "vs." | "Vs" | "VS" => Tag::In,
        _ => {
            // A word that is entirely upper case and longer than one letter reads as an
            // acronym or a proper noun: "NASA", "EBITDA", but not "I".
            if text.chars().count() > 1 && text.chars().all(|c| c.is_uppercase()) {
                return Tag::Nnp;
            }
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
        attach_currency(&mut tokens);

        // Tags, then phonemes. Tags are decided from the word alone.
        for i in 0..tokens.len() {
            if tokens[i].tag == Tag::None {
                tokens[i].tag = tag_for(&tokens[i].text);
            }
            // "used" turns on whether it follows a form of "be": "is used to" is the
            // adjective (jˈuzd), "I used to" is the past habitual (jˈust). A tagger would
            // settle this from the parse; without one the neighbouring word is the evidence.
            if tokens[i].text.eq_ignore_ascii_case("used") {
                let be_form = i > 0
                    && matches!(
                        tokens[i - 1].text.to_lowercase().as_str(),
                        "is" | "are" | "was" | "were" | "am" | "be" | "been" | "being"
                    );
                let next_to = tokens
                    .get(i + 1)
                    .is_some_and(|next| next.text.eq_ignore_ascii_case("to"));
                tokens[i].tag = if be_form {
                    Tag::Jj
                } else if next_to {
                    Tag::Vbd
                } else {
                    Tag::None
                };
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
            let currency = tokens[i].currency;
            let stress = if text.chars().all(|c| !c.is_uppercase()) {
                None
            } else if text.chars().all(|c| c.is_uppercase()) {
                Some(2.0)
            } else {
                Some(0.5)
            };

            let phonemes = if tag == Tag::Punct {
                // A sign that stands for a word ("%", "&") is spoken, not punctuated.
                symbol_phonemes(&self.lexicon, &text, &ctx).or_else(|| punctuation_phonemes(&text))
            } else if tag == Tag::Cd {
                self.lexicon
                    .number(&text, currency)
                    .map(|(ps, _)| ps)
                    .or_else(|| self.spelled(&text))
            } else {
                self.lexicon
                    .word(&text, tag, stress, &ctx)
                    .map(|(ps, _)| ps)
                    .or_else(|| symbol_phonemes(&self.lexicon, &text, &ctx))
                    .or_else(|| self.lexicon.number(&text, currency).map(|(ps, _)| ps))
                    .or_else(|| self.spelled(&text))
            };

            let phonemes = phonemes.unwrap_or_default();
            ctx.advance(Some(&phonemes), tag == Tag::To);
            if SYMBOLS.iter().any(|(symbol, _)| *symbol == text) {
                // A symbol expands to a word, and that word needs its own separation: "5%"
                // is spoken as "five percent", not "fivepercent".
                tokens[i].force_space = true;
            }
            tokens[i].phonemes = Some(phonemes);
        }

        // Join. A space goes before a token only when the source had whitespace there, which
        // keeps "dog." as one run and "twenty-one" as one word.
        let mut out = String::new();
        for (i, token) in tokens.iter().enumerate() {
            let phonemes = token.phonemes.as_deref().unwrap_or("");
            if i > 0
                && (token.leading_space || token.force_space)
                && !out.is_empty()
                && !out.ends_with(' ')
            {
                out.push(' ');
            }
            out.push_str(phonemes);
        }
        // Collapse the runs of spaces that punctuation and silent currency signs leave, and
        // fold the two allophones the engine spells differently: the reference does this for
        // every version below 2.0, and the dictionary itself is written that way ("forty" is
        // stored as fˈɔɹTi), so producing ɾ here would disagree with every table entry.
        out.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .replace('ɾ', "T")
            .replace('ʔ', "t")
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
