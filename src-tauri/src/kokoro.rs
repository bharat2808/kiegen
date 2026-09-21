//! Kokoro-82M synthesis over ONNX Runtime — the opt-in local engine.
//!
//! Kokoro is **phoneme-in**: it never sees text. Turning text into phonemes is a separate
//! front end (`front_end.rs`), which matters because every packaged Kokoro ships espeak
//! for that step while Kokoro itself does not need it.
//!
//! The model contract, taken from the ONNX export and confirmed by the benchmark in
//! docs/DESIGN.md §5:
//!
//! ```text
//! input_ids   int64   (1, L)     phoneme token ids
//! style       float32 (1, 256)   one row from the voice table, chosen by L
//! speed       float32 (1,)       playback rate multiplier
//! ────────────────────────────────────────────────────────────────
//! waveform    float32 (1, N)     24 kHz mono
//! ```
//!
//! Two consequences shape this module:
//!
//! * The style row is selected by token count, so a voice file with 510 rows can only
//!   serve sequences of at most 509 tokens. Longer input must be chunked, and the chunk
//!   boundary is where seam artifacts appear.
//! * The style row's *index* is the token count, so two chunks of different lengths are
//!   spoken with different style rows — a short final chunk is not just shorter, it is
//!   rendered in a slightly different voice. The chunker therefore tries to keep chunks
//!   comparable in length rather than merely under the cap.

use std::collections::HashMap;
use std::path::Path;

use ort::session::Session;
use ort::value::Tensor;

/// 24 kHz mono, fixed by the model.
pub const SAMPLE_RATE: u32 = 24000;

/// Upper bound on tokens per forward pass. The real limit comes from the voice file: a
/// style table with N rows serves at most N−1 tokens, because the row is chosen by token
/// count and row 0 is never reachable. The shipped voices have 510 rows, so 509 usable
/// tokens — this constant only stops a malformed table from asking for more.
pub const MAX_TOKENS: usize = 510;

/// Width of one style row.
const STYLE_WIDTH: usize = 256;

/// Silence inserted between chunks. Kokoro does not reliably render leading/trailing
/// silence, so without this the joins run together.
const CHUNK_GAP_SECONDS: f32 = 0.25;

/// A loaded Kokoro model plus the assets it needs.
pub struct Kokoro {
    session: Session,
    /// Phoneme character → token id, from the model's `tokenizer.json`.
    vocab: HashMap<char, i64>,
    /// Voice style table, flattened: `rows * STYLE_WIDTH` floats.
    styles: Vec<f32>,
    row_count: usize,
    speed: f32,
}

impl Kokoro {
    /// Load the model, its tokenizer vocabulary and one voice style table.
    ///
    /// Weights are deliberately *not* bundled with the app: they are fetched separately
    /// and passed in as paths.
    pub fn load(
        model: &Path,
        tokenizer_json: &Path,
        voice: &Path,
        speed: f32,
    ) -> Result<Self, String> {
        let raw = std::fs::read_to_string(tokenizer_json)
            .map_err(|e| format!("read {tokenizer_json:?}: {e}"))?;
        let vocab = parse_vocab(&raw)?;

        let bytes = std::fs::read(voice).map_err(|e| format!("read {voice:?}: {e}"))?;
        if bytes.len() % 4 != 0 {
            return Err(format!("voice file {voice:?} is not a float32 array"));
        }
        let mut styles = Vec::with_capacity(bytes.len() / 4);
        for chunk in bytes.chunks_exact(4) {
            styles.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
        }
        let row_count = styles.len() / STYLE_WIDTH;
        if row_count < 2 {
            return Err(format!(
                "voice file {voice:?} holds {row_count} style rows; expected a table"
            ));
        }

        let session = Session::builder()
            .map_err(|e| format!("onnxruntime init: {e}"))?
            .commit_from_file(model)
            .map_err(|e| format!("load model {model:?}: {e}"))?;

        Ok(Self {
            session,
            vocab,
            styles,
            row_count,
            speed,
        })
    }

    /// Longest phoneme string this model accepts in one pass.
    pub fn max_tokens(&self) -> usize {
        (self.row_count - 1).min(MAX_TOKENS)
    }

    /// Synthesize phonemes to 24 kHz mono samples, chunking anything over the cap.
    ///
    /// Returns the audio plus the characters the vocabulary could not encode — silently
    /// dropping those would hide words from the listener, so the caller can report them.
    pub fn synthesize(&mut self, phonemes: &str) -> Result<(Vec<f32>, Vec<char>), String> {
        let budget = self.max_tokens();
        let chunks = chunk_phonemes(phonemes, budget);
        let gap = (CHUNK_GAP_SECONDS * SAMPLE_RATE as f32) as usize;

        let mut audio: Vec<f32> = Vec::new();
        let mut dropped: Vec<char> = Vec::new();
        let mut spoke_something = false;

        for chunk in chunks {
            let (ids, unencodable) = encode(&self.vocab, &chunk);
            for c in unencodable {
                if !dropped.contains(&c) {
                    dropped.push(c);
                }
            }
            if ids.is_empty() {
                continue;
            }
            if spoke_something {
                audio.extend(std::iter::repeat_n(0.0, gap));
            }
            audio.extend(self.run_chunk(&ids)?);
            spoke_something = true;
        }

        if !spoke_something {
            return Err("nothing to synthesize: no phoneme survived tokenization".to_string());
        }
        Ok((audio, dropped))
    }

    /// One forward pass. `ids` must be at most `max_tokens()` long.
    fn run_chunk(&mut self, ids: &[i64]) -> Result<Vec<f32>, String> {
        let rows = self.row_count.min(MAX_TOKENS + 1);
        if ids.len() >= rows {
            return Err(format!(
                "chunk of {} tokens exceeds the {rows}-row style table",
                ids.len()
            ));
        }

        // The style row is indexed by token count — this lookup is the whole reason the
        // chunker exists.
        let offset = ids.len() * STYLE_WIDTH;
        let style = self.styles[offset..offset + STYLE_WIDTH].to_vec();

        let inputs = ort::inputs![
            "input_ids" => Tensor::from_array(([1, ids.len()], ids.to_vec()))
                .map_err(|e| format!("input_ids tensor: {e}"))?,
            "style" => Tensor::from_array(([1, STYLE_WIDTH], style))
                .map_err(|e| format!("style tensor: {e}"))?,
            "speed" => Tensor::from_array(([1], vec![self.speed]))
                .map_err(|e| format!("speed tensor: {e}"))?,
        ];

        let outputs = self
            .session
            .run(inputs)
            .map_err(|e| format!("inference failed: {e}"))?;
        let (_shape, data) = outputs["waveform"]
            .try_extract_tensor::<f32>()
            .map_err(|e| format!("output tensor: {e}"))?;
        Ok(data.to_vec())
    }
}

/// Parse the `model.vocab` half of a HuggingFace `tokenizer.json` into character → id.
///
/// Kokoro's tokenizer is character-level: one entry per phoneme symbol, plus the special
/// tokens for padding and unknown.
pub fn parse_vocab(json: &str) -> Result<HashMap<char, i64>, String> {
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("tokenizer.json: {e}"))?;
    let vocab = value
        .get("model")
        .and_then(|model| model.get("vocab"))
        .and_then(|vocab| vocab.as_object())
        .ok_or_else(|| "tokenizer.json has no model.vocab object".to_string())?;

    let mut out = HashMap::with_capacity(vocab.len());
    for (symbol, id) in vocab {
        let Some(id) = id.as_i64() else { continue };
        // Single-character symbols are the phonemes; the vocabulary also carries
        // multi-character specials which no phoneme string will contain.
        let mut chars = symbol.chars();
        if let (Some(c), None) = (chars.next(), chars.next()) {
            out.insert(c, id);
        }
    }
    Ok(out)
}

/// Map phonemes to token ids, returning the characters that had no entry.
pub fn encode(vocab: &HashMap<char, i64>, phonemes: &str) -> (Vec<i64>, Vec<char>) {
    let mut ids = Vec::with_capacity(phonemes.len());
    let mut dropped = Vec::new();
    for c in phonemes.chars() {
        match vocab.get(&c) {
            Some(id) => ids.push(*id),
            None => {
                if !dropped.contains(&c) {
                    dropped.push(c);
                }
            }
        }
    }
    (ids, dropped)
}

/// Split phonemes into pieces that each encode to at most `budget` tokens.
///
/// Every phoneme character maps to at most one token, so a character budget is a safe
/// upper bound on the token budget. Cuts prefer the strongest boundary available inside
/// the window — sentence, then clause, then whitespace — so chunks break where a reader
/// would pause rather than mid-word.
pub fn chunk_phonemes(phonemes: &str, budget: usize) -> Vec<String> {
    let chars: Vec<char> = phonemes.chars().collect();
    if budget == 0 {
        return Vec::new();
    }
    let mut chunks = Vec::new();
    let mut start = 0;

    while start < chars.len() {
        let remaining = chars.len() - start;
        if remaining <= budget {
            break;
        }
        let window = &chars[start..start + budget];

        // Strongest boundary wins; the last occurrence of the best class found.
        let cut = [".!?", ";:,", " "]
            .iter()
            .find_map(|class| {
                window
                    .iter()
                    .rposition(|c| class.contains(*c))
                    .filter(|index| *index > 0)
                    .map(|index| index + 1)
            })
            // No boundary at all: a single unbroken run longer than the budget. Cut it
            // hard rather than dropping it.
            .unwrap_or(budget);

        chunks.push(chars[start..start + cut].iter().collect());
        start += cut;
    }
    if start < chars.len() {
        chunks.push(chars[start..].iter().collect());
    }
    chunks
}

/// Write 16-bit mono PCM. Used for `speak to file` and by the tests.
pub fn write_wav(path: &Path, samples: &[f32], sample_rate: u32) -> Result<(), String> {
    let data_len = (samples.len() * 2) as u32;
    let byte_rate = sample_rate * 2;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // PCM chunk size
    out.extend_from_slice(&1u16.to_le_bytes()); // format: PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // channels
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes()); // block align
    out.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for sample in samples {
        let scaled = (sample.clamp(-1.0, 1.0) * 32767.0) as i16;
        out.extend_from_slice(&scaled.to_le_bytes());
    }
    std::fs::write(path, out).map_err(|e| format!("write {path:?}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vocab_parses_the_model_section() {
        let json = r#"{
            "model": { "vocab": { "a": 1, "b": 2, "[pad]": 0 }, "type": "WordLevel" },
            "added_tokens": []
        }"#;
        let vocab = parse_vocab(json).unwrap();
        assert_eq!(vocab.get(&'a'), Some(&1));
        assert_eq!(vocab.get(&'b'), Some(&2));
        // Multi-character specials are not phonemes and must not land in the map.
        assert_eq!(vocab.len(), 2);
    }

    #[test]
    fn vocab_parsing_rejects_a_tokenizer_that_is_not_a_tokenizer() {
        assert!(parse_vocab("{}").is_err());
        assert!(parse_vocab("not json").is_err());
        assert!(parse_vocab(r#"{"model":{"type":"WordLevel"}}"#).is_err());
    }

    #[test]
    fn encoding_reports_rather_than_swallows_unknown_symbols() {
        let json = r#"{"model":{"vocab":{"a":1,"b":2}}}"#;
        let vocab = parse_vocab(json).unwrap();
        let (ids, dropped) = encode(&vocab, "ab?ba");
        assert_eq!(ids, vec![1, 2, 2, 1]);
        // U+2753 is what misaki emits for a word it cannot resolve; Kokoro has no token
        // for it, so the word would vanish from the audio unless we say so.
        assert_eq!(dropped, vec!['?']);
    }

    #[test]
    fn chunks_never_exceed_the_budget() {
        let text: String = (0..50).map(|i| format!("word{i} ")).collect();
        for budget in [4, 10, 33, 100] {
            let chunks = chunk_phonemes(&text, budget);
            assert!(!chunks.is_empty(), "budget {budget} produced nothing");
            for chunk in &chunks {
                assert!(
                    chunk.chars().count() <= budget,
                    "chunk of {} chars exceeds budget {budget}",
                    chunk.chars().count()
                );
            }
            let rejoined: String = chunks.concat();
            assert_eq!(rejoined, text, "chunking lost or reordered characters");
        }
    }

    #[test]
    fn chunks_break_at_punctuation_when_they_can() {
        let text = "hello there. goodbye now. and again please.";
        let chunks = chunk_phonemes(text, 20);
        // The first cut should land after the sentence stop, not mid-word.
        assert!(
            chunks[0].ends_with(' ') || chunks[0].ends_with('.'),
            "cut mid-word: {:?}",
            chunks[0]
        );
        assert!(
            chunks[0].contains('.'),
            "expected a sentence boundary in {:?}",
            chunks[0]
        );
    }

    #[test]
    fn an_unbroken_run_is_cut_hard_rather_than_dropped() {
        // No whitespace or punctuation to break on: the chunker must still make progress.
        let text = "a".repeat(25);
        let chunks = chunk_phonemes(&text, 10);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks.concat().chars().count(), 25);
    }

    #[test]
    fn short_input_is_a_single_chunk() {
        let chunks = chunk_phonemes("həlˈoʊ", 510);
        assert_eq!(chunks, vec!["həlˈoʊ".to_string()]);
    }

    #[test]
    fn a_zero_budget_is_not_an_infinite_loop() {
        assert!(chunk_phonemes("anything", 0).is_empty());
    }

    #[test]
    fn wav_header_is_well_formed() {
        let dir = std::env::temp_dir().join("kiegen-wav-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("probe.wav");
        let samples: Vec<f32> = (0..100).map(|i| (i as f32 / 100.0) - 0.5).collect();
        write_wav(&path, &samples, SAMPLE_RATE).unwrap();

        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");
        assert_eq!(&bytes[36..40], b"data");
        // 44-byte header plus one 16-bit sample each.
        assert_eq!(bytes.len(), 44 + samples.len() * 2);
        let rate = u32::from_le_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]);
        assert_eq!(rate, SAMPLE_RATE);
        let _ = std::fs::remove_file(&path);
    }

    /// End-to-end against the real weights. Ignored by default because CI has no 325 MB
    /// model; run locally with:
    ///
    /// ```text
    /// KOKORO_MODEL_DIR=... cargo test kokoro::tests::synthesizes -- --ignored --nocapture
    /// ```
    ///
    /// The directory must hold `model.onnx`, `tokenizer.json` and `voice.bin`.
    #[test]
    #[ignore = "needs the 325 MB Kokoro weights on disk"]
    fn synthesizes_real_audio_from_phonemes() {
        let dir = std::env::var("KOKORO_MODEL_DIR")
            .expect("set KOKORO_MODEL_DIR to a folder with model.onnx/tokenizer.json/voice.bin");
        let dir = Path::new(&dir);
        let mut kokoro = Kokoro::load(
            &dir.join("model.onnx"),
            &dir.join("tokenizer.json"),
            &dir.join("voice.bin"),
            1.0,
        )
        .expect("load");

        // Derived from the voice file, not from documentation: the shipped voices have
        // 510 style rows, so row-by-token-count reaches at most 509 tokens. An earlier
        // note in docs/DESIGN.md said 511 rows; the file disagreed and the test won.
        assert_eq!(kokoro.max_tokens(), 509);
        // Same phoneme strings misaki produced in the Python verification run, so the two
        // implementations can be compared sample-for-sample on identical input.
        let phonemes = match std::env::var("KOKORO_PHONEMES_FILE") {
            Ok(path) => std::fs::read_to_string(&path).expect("read KOKORO_PHONEMES_FILE"),
            Err(_) => "ðə kwˈɪk bɹˈWn fˈɑks ʤˈʌmps ˈOvəɹ ðə lˈAzi dˈɔɡ.".to_string(),
        };
        let phonemes = phonemes.trim();
        let started = std::time::Instant::now();
        let (audio, dropped) = kokoro.synthesize(phonemes).expect("synthesize");
        let elapsed = started.elapsed();

        // Write the artifact before asserting, so a failing assertion still leaves
        // something to listen to.
        let out = dir.join("rust_kokoro.wav");
        write_wav(&out, &audio, SAMPLE_RATE).expect("write wav");

        let seconds = audio.len() as f32 / SAMPLE_RATE as f32;
        println!(
            "{} samples = {seconds:.2}s of audio in {elapsed:?} (rtf {:.3}), dropped {dropped:?}",
            audio.len(),
            elapsed.as_secs_f32() / seconds
        );
        println!("wrote {}", out.display());

        // `❓` is misaki's marker for a word it could not resolve, and Kokoro has no token
        // for it — dropping it is correct. Anything *else* being dropped means the vocab
        // parse or the tokenizer file is wrong, which would silently mangle speech.
        let unexpected: Vec<char> = dropped.iter().copied().filter(|c| *c != '❓').collect();
        assert!(
            unexpected.is_empty(),
            "real phoneme symbols were dropped: {unexpected:?}"
        );
        assert!(seconds > 1.0, "implausibly short output: {seconds}s");
        assert!(
            audio.iter().any(|s| s.abs() > 0.01),
            "output is silent — wrong style row or token ids"
        );
    }
}
