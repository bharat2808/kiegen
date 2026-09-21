//! The pronunciation lexicon: a word and a tag in, IPA out.
//!
//! A port of misaki's `Lexicon` (Apache-2.0), which is what the reference pipeline uses. The
//! dictionary files are misaki's own `us_gold.json` / `us_silver.json` — they are already
//! IPA, so nothing is transliterated here and the output is directly comparable with the
//! reference.
//!
//! Two deliberate divergences from the original, both documented where they happen:
//!
//! * **No POS tagger.** misaki runs spaCy to tag every token, and the tag selects between
//!   `DEFAULT` and a part-of-speech variant for 790 words (`record` is `ɹˈɛkəɹd` as a noun
//!   and `ɹəkˈɔɹd` as a verb). Without a tagger those words get `DEFAULT`. Function words,
//!   which are the frequent case, are decided without a tagger and are *not* affected.
//! * **No `❓`.** misaki emits `❓` for a word it cannot pronounce, and Kokoro then drops it
//!   silently — "kiegen reads whatever you select" is spoken as "reads whatever you select".
//!   Here an unknown word is spelled out instead, so it is always audible.

use std::collections::HashMap;

use crate::numbers;

pub const PRIMARY_STRESS: char = 'ˈ';
pub const SECONDARY_STRESS: char = 'ˌ';

const VOWELS: &str = "AIOQWYaiuæɑɒɔəɛɜɪʊʌᵻ";
const STRESSES: [char; 2] = ['ˌ', 'ˈ'];

/// Single letters, for spelling an acronym or an unknown word. Every one of the 26 is
/// present in the gold dictionary, which is what makes spelling possible without a
/// letter-to-sound model.
pub const LETTERS: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZ";

/// A dictionary entry: either one pronunciation, or several chosen by part of speech.
#[derive(Debug, Clone)]
pub enum Entry {
    Plain(String),
    Tagged(HashMap<String, Option<String>>),
}

impl Entry {
    fn for_tag(&self, tag: Tag) -> Option<String> {
        match self {
            Entry::Plain(ps) => Some(ps.clone()),
            Entry::Tagged(variants) => {
                let key = tag.name();
                if let Some(found) = variants.get(key) {
                    return found.clone();
                }
                // Fall back through the tag's parent class, then to DEFAULT, exactly as the
                // reference does.
                if let Some(parent) = tag.parent() {
                    if let Some(found) = variants.get(parent) {
                        return found.clone();
                    }
                }
                variants.get("DEFAULT").cloned().flatten()
            }
        }
    }

    /// True when this entry has a pronunciation that depends on part of speech, which is
    /// the case a tagger-less implementation cannot get right.
    pub fn is_tag_dependent(&self) -> bool {
        matches!(self, Entry::Tagged(_))
    }
}

/// The part of speech, as far as this port can tell. `None` means "no tagger opinion",
/// which is the common case and resolves to `DEFAULT`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tag {
    Dt,
    Prp,
    To,
    In,
    Vbd,
    Jj,
    Cd,
    Nnp,
    Punct,
    None,
}

impl Tag {
    fn name(self) -> &'static str {
        match self {
            Tag::Dt => "DT",
            Tag::Prp => "PRP",
            Tag::To => "TO",
            Tag::In => "IN",
            Tag::Vbd => "VBD",
            Tag::Jj => "JJ",
            Tag::Cd => "CD",
            Tag::Nnp => "NNP",
            Tag::Punct => "PUNCT",
            Tag::None => "None",
        }
    }

    /// The coarse class a tag rolls up to, which is how the reference retries a lookup.
    fn parent(self) -> Option<&'static str> {
        match self {
            Tag::Vbd => Some("VERB"),
            Tag::Nnp => Some("NOUN"),
            _ => None,
        }
    }
}

#[derive(Debug, Default)]
pub struct Lexicon {
    gold: HashMap<String, Entry>,
    silver: HashMap<String, String>,
}

/// The consonants whose sound changes what a following `-s` or `-ed` becomes.
fn stress_weight(phonemes: &str) -> usize {
    // Diphthongs and affricates count double, so stress lands on the heavier syllable.
    const HEAVY: &str = "AIOQWYʤʧ";
    phonemes
        .chars()
        .map(|c| if HEAVY.contains(c) { 2 } else { 1 })
        .sum()
}

/// Rewrites the stress marks on a phoneme string. Directly ported, including the trick in
/// `restress` of moving a stress mark to the next vowel by renumbering positions in halves.
pub fn apply_stress(ps: &str, stress: Option<f32>) -> String {
    let stress = match stress {
        Some(stress) => stress,
        None => return ps.to_string(),
    };

    fn restress(ps: &str) -> String {
        let chars: Vec<char> = ps.chars().collect();
        let mut indexed: Vec<(f32, char)> = chars.iter().map(|c| (0.0, *c)).collect();
        // Each stress mark moves to sit immediately before the next vowel.
        let marks: Vec<usize> = chars
            .iter()
            .enumerate()
            .filter(|(_, c)| STRESSES.contains(c))
            .map(|(i, _)| i)
            .collect();
        for i in marks {
            if let Some(j) = chars
                .iter()
                .enumerate()
                .skip(i)
                .find(|(_, c)| VOWELS.contains(**c))
                .map(|(j, _)| j)
            {
                indexed[i].0 = j as f32 - 0.5;
                indexed[j].0 = j as f32;
            }
        }
        indexed.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        indexed.iter().map(|(_, c)| *c).collect()
    }

    if stress < -1.0 {
        // Reduced to nothing: an unstressed function word.
        return ps.replace(PRIMARY_STRESS, "").replace(SECONDARY_STRESS, "");
    }
    if stress == -1.0 || (stress == 0.0 || stress == -0.5) && ps.contains(PRIMARY_STRESS) {
        return ps
            .replace(SECONDARY_STRESS, "")
            .replace(PRIMARY_STRESS, &SECONDARY_STRESS.to_string());
    }
    let has_any_stress = ps.chars().any(|c| STRESSES.contains(&c));
    if [0.0, 0.5, 1.0].contains(&stress) && !has_any_stress {
        if !ps.chars().any(|c| VOWELS.contains(c)) {
            return ps.to_string();
        }
        return restress(&format!("{SECONDARY_STRESS}{ps}"));
    }
    if stress >= 1.0 && !ps.contains(PRIMARY_STRESS) && ps.contains(SECONDARY_STRESS) {
        return ps.replace(SECONDARY_STRESS, &PRIMARY_STRESS.to_string());
    }
    if stress > 1.0 && !has_any_stress {
        if !ps.chars().any(|c| VOWELS.contains(c)) {
            return ps.to_string();
        }
        return restress(&format!("{PRIMARY_STRESS}{ps}"));
    }
    ps.to_string()
}

impl Lexicon {
    /// Loads the two dictionary files. Both are JSON objects mapping a word to either a
    /// phoneme string or, for 790 words in the gold set, a map from tag to pronunciation.
    pub fn load(gold_json: &str, silver_json: &str) -> Result<Self, String> {
        let gold_raw: HashMap<String, serde_json::Value> =
            serde_json::from_str(gold_json).map_err(|e| format!("us_gold.json: {e}"))?;
        let silver_raw: HashMap<String, String> =
            serde_json::from_str(silver_json).map_err(|e| format!("us_silver.json: {e}"))?;

        let mut gold = HashMap::with_capacity(gold_raw.len() * 2);
        for (word, value) in gold_raw {
            let entry = match value {
                serde_json::Value::String(ps) => Entry::Plain(ps),
                serde_json::Value::Object(variants) => Entry::Tagged(
                    variants
                        .into_iter()
                        .map(|(tag, ps)| (tag, ps.as_str().map(str::to_string)))
                        .collect(),
                ),
                other => return Err(format!("{word}: unexpected dictionary value {other}")),
            };
            gold.insert(word, entry);
        }

        let mut silver = HashMap::with_capacity(silver_raw.len() * 2);
        for (word, ps) in silver_raw {
            silver.insert(word, ps);
        }

        let mut lexicon = Lexicon { gold, silver };
        lexicon.grow_dictionary();
        Ok(lexicon)
    }

    /// Adds the case variants the reference adds, which is what lets a sentence-initial
    /// "The" and an all-caps "THE" find the same entry as "the".
    fn grow_dictionary(&mut self) {
        let mut extra_gold: Vec<(String, Entry)> = Vec::new();
        for (word, entry) in &self.gold {
            if word.chars().count() < 2 {
                continue;
            }
            if word.chars().all(|c| !c.is_uppercase()) {
                let capitalized: String = {
                    let mut chars = word.chars();
                    match chars.next() {
                        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                        None => continue,
                    }
                };
                if capitalized != *word {
                    extra_gold.push((capitalized, entry.clone()));
                }
            } else if word.chars().next().is_some_and(|c| c.is_uppercase())
                && word.chars().skip(1).all(|c| !c.is_uppercase())
            {
                extra_gold.push((word.to_lowercase(), entry.clone()));
            }
        }
        for (word, entry) in extra_gold {
            self.gold.entry(word).or_insert(entry);
        }

        let mut extra_silver: Vec<(String, String)> = Vec::new();
        for (word, ps) in &self.silver {
            if word.chars().count() < 2 {
                continue;
            }
            if word.chars().all(|c| !c.is_uppercase()) {
                let capitalized: String = {
                    let mut chars = word.chars();
                    match chars.next() {
                        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                        None => continue,
                    }
                };
                if capitalized != *word {
                    extra_silver.push((capitalized, ps.clone()));
                }
            }
        }
        for (word, ps) in extra_silver {
            self.silver.entry(word).or_insert(ps);
        }
    }

    pub fn gold_len(&self) -> usize {
        self.gold.len()
    }

    pub fn silver_len(&self) -> usize {
        self.silver.len()
    }

    /// How many gold entries depend on part of speech — the measure of what a tagger-less
    /// port cannot resolve.
    pub fn tag_dependent_len(&self) -> usize {
        self.gold.values().filter(|e| e.is_tag_dependent()).count()
    }

    /// Spells a word out with the dictionary's letter names. Used for acronyms, which is
    /// what the reference does too, and as the last resort for a word nothing knows.
    fn spell_letters(&self, word: &str) -> Option<(String, u8)> {
        let mut phonemes = String::new();
        for c in word.chars().filter(|c| c.is_alphabetic()) {
            let upper = c.to_uppercase().to_string();
            match self.gold.get(&upper) {
                Some(Entry::Plain(ps)) => phonemes.push_str(ps),
                _ => return None,
            }
        }
        if phonemes.is_empty() {
            return None;
        }
        let stressed = apply_stress(&phonemes, Some(0.0));
        // Only the last syllable keeps a mark; the rest are flattened.
        let mut parts: Vec<&str> = stressed.rsplit(SECONDARY_STRESS).collect();
        parts.reverse();
        Some((parts.join(&PRIMARY_STRESS.to_string()), 3))
    }

    fn is_known(&self, word: &str) -> bool {
        if self.gold.contains_key(word) || self.silver.contains_key(word) {
            return true;
        }
        if !word.chars().all(|c| c.is_alphabetic()) {
            return false;
        }
        if word.chars().count() == 1 {
            return true;
        }
        if word.chars().all(|c| c.is_uppercase()) && self.gold.contains_key(&word.to_lowercase()) {
            return true;
        }
        // "iPhone" and friends: a single leading capital is not a reason to miss the entry.
        let mut chars = word.chars();
        chars.next();
        chars.as_str().to_uppercase() == chars.as_str() && !chars.as_str().is_empty()
    }

    fn get_special_case(&self, word: &str, tag: Tag, ctx: &TokenContext) -> Option<(String, u8)> {
        // The function words English reduces. These are decided from the word itself rather
        // than from a tagger, because they are the most frequent tokens in any text and
        // getting them wrong is the most audible error.
        match word {
            "a" | "A" => return Some(("ɐ".to_string(), 4)),
            "an" | "An" => return Some(("ɐn".to_string(), 4)),
            "I" if tag == Tag::Prp => return Some((format!("{SECONDARY_STRESS}I"), 4)),
            "the" | "The" => {
                return Some((
                    if ctx.future_vowel == Some(true) {
                        "ði"
                    } else {
                        "ðə"
                    }
                    .to_string(),
                    4,
                ))
            }
            "to" | "To" => {
                let ps = match ctx.future_vowel {
                    None => "tə",
                    Some(false) => "tə",
                    Some(true) => "tʊ",
                };
                return Some((ps.to_string(), 4));
            }
            "in" | "In" => {
                let stress = if ctx.future_vowel == Some(true) {
                    ""
                } else {
                    "ˈ"
                };
                return Some((format!("{stress}ɪn"), 4));
            }
            "am" | "Am" => {
                if let Some(Entry::Plain(ps)) = self.gold.get("am") {
                    return Some((ps.clone(), 4));
                }
            }
            "by" | "By" => {
                if tag == Tag::None {
                    return self.lookup("by", tag, None, ctx);
                }
            }
            "vs" | "vs." | "Vs" | "VS" => {
                if tag == Tag::In {
                    return self.lookup("versus", tag, None, ctx);
                }
            }
            "used" | "Used" | "USED" => {
                if let Some(Entry::Tagged(variants)) = self.gold.get("used") {
                    // "used to" is the verb form; otherwise the adjective.
                    let key = if ctx.future_to { "VBD" } else { "DEFAULT" };
                    return variants.get(key).cloned().flatten().map(|ps| (ps, 4));
                }
            }
            _ => {}
        }
        None
    }

    pub fn lookup(
        &self,
        word: &str,
        tag: Tag,
        stress: Option<f32>,
        _ctx: &TokenContext,
    ) -> Option<(String, u8)> {
        // An all-caps word that is not itself in the dictionary is an acronym: spell it.
        if word.chars().all(|c| c.is_uppercase()) && !self.gold.contains_key(word) {
            if word.chars().count() > 1 {
                if let Some(result) = self.spell_letters(word) {
                    return Some(result);
                }
            }
        }

        let (entry, rating) = match self.gold.get(word) {
            Some(entry) => (Some(entry), 4u8),
            None => (None, 4),
        };
        let (phonemes, rating) = match entry {
            Some(entry) => (entry.for_tag(tag), rating),
            None => match self.silver.get(word) {
                Some(ps) => (Some(ps.clone()), 3),
                None => (None, 3),
            },
        };
        phonemes.map(|ps| (apply_stress(&ps, stress), rating))
    }

    /// `-s`: cats, boxes, watches. The ending depends on the final sound of the stem.
    fn add_s(&self, stem: &str) -> Option<String> {
        let last = stem.chars().last()?;
        if "ptkfθ".contains(last) {
            return Some(format!("{stem}s"));
        }
        if "szʃʒʧʤ".contains(last) {
            return Some(format!("{stem}ᵻz"));
        }
        Some(format!("{stem}z"))
    }

    /// `-ed`: walked, wanted, played.
    fn add_ed(&self, stem: &str) -> Option<String> {
        let last = stem.chars().last()?;
        if "pkfθʃsʧ".contains(last) {
            return Some(format!("{stem}t"));
        }
        if last == 'd' {
            return Some(format!("{stem}ᵻd"));
        }
        if last != 't' {
            return Some(format!("{stem}d"));
        }
        // A stem ending in /t/: an American flapping of the double-t.
        const US_FLAP: &str = "AIOWYiuæɑəɛɪɹʊʌ";
        let mut chars: Vec<char> = stem.chars().collect();
        if chars.len() >= 2 && US_FLAP.contains(chars[chars.len() - 2]) {
            chars.pop();
            return Some(format!("{}ɾᵻd", chars.into_iter().collect::<String>()));
        }
        Some(format!("{stem}ᵻd"))
    }

    fn add_ing(&self, stem: &str) -> Option<String> {
        const US_FLAP: &str = "AIOWYiuæɑəɛɪɹʊʌ";
        let mut chars: Vec<char> = stem.chars().collect();
        if chars.len() > 1
            && chars[chars.len() - 1] == 't'
            && US_FLAP.contains(chars[chars.len() - 2])
        {
            chars.pop();
            return Some(format!("{}ɾɪŋ", chars.into_iter().collect::<String>()));
        }
        Some(format!("{stem}ɪŋ"))
    }

    /// Tries to reach a known word by stripping an inflection, so a word the dictionary does
    /// not list directly can still be pronounced from its stem.
    pub fn by_inflection(
        &self,
        word: &str,
        tag: Tag,
        stress: Option<f32>,
        ctx: &TokenContext,
    ) -> Option<(String, u8)> {
        let chars: Vec<char> = word.chars().collect();
        let endswith = |suffix: &str| word.ends_with(suffix);

        // -s / -es / -ies
        if chars.len() >= 3 && endswith("s") {
            let candidates: Vec<String> = if endswith("ies") && chars.len() > 4 {
                vec![format!("{}y", &word[..word.len() - 3])]
            } else if endswith("es") && chars.len() > 4 && !endswith("ss") {
                vec![
                    word[..word.len() - 2].to_string(),
                    word[..word.len() - 1].to_string(),
                ]
            } else {
                vec![word[..word.len() - 1].to_string()]
            };
            for stem in candidates {
                if !stem.ends_with("ss") && self.is_known(&stem) {
                    if let Some((ps, rating)) = self.lookup(&stem, tag, stress, ctx) {
                        return self.add_s(&ps).map(|ps| (ps, rating));
                    }
                }
            }
        }

        // -ed
        if chars.len() >= 4 && endswith("ed") && !endswith("eed") {
            let stem = &word[..word.len() - 2];
            if self.is_known(stem) {
                if let Some((ps, rating)) = self.lookup(stem, tag, stress, ctx) {
                    return self.add_ed(&ps).map(|ps| (ps, rating));
                }
            }
        }
        if chars.len() >= 4 && endswith("d") && !endswith("dd") {
            let stem = &word[..word.len() - 1];
            if self.is_known(stem) {
                if let Some((ps, rating)) = self.lookup(stem, tag, stress, ctx) {
                    return self.add_ed(&ps).map(|ps| (ps, rating));
                }
            }
        }

        // -ing
        if chars.len() >= 5 && endswith("ing") {
            let base = &word[..word.len() - 3];
            let mut candidates = vec![base.to_string(), format!("{base}e")];
            // A doubled final consonant: running, sitting.
            let b: Vec<char> = base.chars().collect();
            if b.len() >= 2 && b[b.len() - 1] == b[b.len() - 2] {
                candidates.push(b[..b.len() - 1].iter().collect());
            }
            for stem in candidates {
                if self.is_known(&stem) {
                    if let Some((ps, rating)) = self.lookup(&stem, tag, stress, ctx) {
                        return self.add_ing(&ps).map(|ps| (ps, rating));
                    }
                }
            }
        }

        None
    }

    /// The entry point: a word to phonemes, or `None` if nothing can pronounce it.
    pub fn word(
        &self,
        word: &str,
        tag: Tag,
        stress: Option<f32>,
        ctx: &TokenContext,
    ) -> Option<(String, u8)> {
        if let Some(result) = self.get_special_case(word, tag, ctx) {
            return Some(result);
        }
        if self.is_known(word) {
            return self.lookup(word, tag, stress, ctx);
        }
        // A possessive or a bare apostrophe happens constantly in real text.
        if let Some(stem) = word.strip_suffix("'s") {
            if self.is_known(&format!("{stem}'s")) {
                return self.lookup(&format!("{stem}'s"), tag, stress, ctx);
            }
        }
        if let Some(stem) = word.strip_suffix('\'') {
            if self.is_known(stem) {
                return self.lookup(stem, tag, stress, ctx);
            }
        }
        if let Some(result) = self.by_inflection(word, tag, stress, ctx) {
            return Some(result);
        }
        None
    }

    /// Numbers, delegated to the ported `num2words` in `crate::numbers`.
    pub fn number(&self, word: &str) -> Option<(String, u8)> {
        let mut digits_end = 0;
        for (i, c) in word.char_indices() {
            if c.is_ascii_digit() || c == ',' || c == '.' {
                digits_end = i + c.len_utf8();
            } else {
                break;
            }
        }
        if digits_end == 0 {
            return None;
        }
        let digits = &word[..digits_end];
        let suffix = &word[digits_end..];
        let cleaned = digits.replace(',', "");

        let words = if suffix == "st" || suffix == "nd" || suffix == "rd" || suffix == "th" {
            match cleaned.parse::<u64>() {
                Ok(n) => numbers::ordinal(n),
                Err(_) => return None,
            }
        } else if let Some((whole, fraction)) = cleaned.split_once('.') {
            if whole.is_empty() {
                // ".5" is said as "point five".
                format!(
                    "point {}",
                    numbers::decimal(0, fraction).split(" point ").last()?
                )
            } else {
                match whole.parse::<u64>() {
                    Ok(n) => numbers::decimal(n, fraction),
                    Err(_) => return None,
                }
            }
        } else {
            match cleaned.parse::<u64>() {
                Ok(n) if (1000..10000).contains(&n) => numbers::year(n),
                Ok(n) => numbers::cardinal(n),
                Err(_) => return None,
            }
        };

        // Each word of the number is looked up, and they are joined by spaces.
        let mut parts = Vec::new();
        for piece in words.split(|c: char| !c.is_alphabetic() && c != '-') {
            if piece.is_empty() {
                continue;
            }
            let (ps, _) = self
                .lookup(piece, Tag::None, None, &TokenContext::default())
                .or_else(|| {
                    self.lookup(
                        &piece.to_lowercase(),
                        Tag::None,
                        None,
                        &TokenContext::default(),
                    )
                })?;
            parts.push(ps);
        }
        if parts.is_empty() {
            return None;
        }
        let joined = parts.join(" ");
        let phonemes = match suffix {
            "s" | "'s" => self.add_s(&joined)?,
            "ed" | "'d" => self.add_ed(&joined)?,
            "ing" => self.add_ing(&joined)?,
            _ => joined,
        };
        Some((phonemes, 3))
    }

    /// The last resort, and the reason this port never deletes a word: spell it.
    pub fn spell_or_give_up(&self, word: &str) -> Option<(String, u8)> {
        if word.chars().all(|c| c.is_uppercase()) {
            return self.spell_letters(word);
        }
        self.spell_letters(word)
    }
}

/// Carried forward through the sentence: whether the next token begins with a vowel (which
/// decides "the" and "to"), and whether the next token is "to" (which decides "used").
#[derive(Debug, Default, Clone, Copy)]
pub struct TokenContext {
    pub future_vowel: Option<bool>,
    pub future_to: bool,
}

impl TokenContext {
    pub fn advance(&mut self, phonemes: Option<&str>, is_to: bool) {
        const VOWELS_AND_CONSONANTS: &str = "AIOQWYaiuæɑɒɔəɛɜɪʊʌᵻbdfhjklmnpstvwzðŋɡɹɾʃʒʤʧθ";
        const NON_QUOTE_PUNCT: &str = ";:,.!?—…";
        if let Some(ps) = phonemes {
            if let Some(c) = ps
                .chars()
                .find(|c| VOWELS_AND_CONSONANTS.contains(*c) || NON_QUOTE_PUNCT.contains(*c))
            {
                if NON_QUOTE_PUNCT.contains(c) {
                    // Punctuation carries no vowel; keep what we had.
                } else {
                    self.future_vowel = Some(VOWELS.contains(c));
                }
            }
        }
        self.future_to = is_to;
    }
}
