//! Chatterbox Multilingual synthesis over ONNX Runtime — the second opt-in local engine.
//!
//! Chatterbox is **zero-shot**: there is no speaker table anywhere in the checkpoint. The
//! voice is a reference clip, fed once through `speech_encoder`, and every graph after that
//! is conditioned on what that clip produced. That is why this module takes a path to a WAV
//! rather than a voice name, and why `voices.rs` exists next to it to manage the clips.
//!
//! There is also no language *input* to any graph. The language is a text prefix — `[fr]` —
//! that the tokenizer turns into one of its own tokens, so the language reaches the model
//! through the text and nowhere else. `prepare_text` below is that whole mechanism.
//!
//! ## The contract, as it actually behaves
//!
//! ```text
//! speech_encoder(audio_values [1,N])
//!     -> audio_features [1,F,1024]      (cond_emb: the prompt, as embeddings)
//!        audio_tokens   [1,A]           (the prompt, as speech tokens)
//!        speaker_embeddings [1,192]
//!        speaker_features   [1,80,T]
//! embed_tokens(input_ids, position_ids, exaggeration) -> inputs_embeds [1,L,1024]
//! language_model(inputs_embeds, attention_mask, past_key_values.{0..29}.{key,value})
//!     -> logits [1,seq,8194], present.{0..29}.{key,value}   (30 layers, 16 KV heads, dim 64)
//! conditional_decoder(speech_tokens, speaker_embeddings, speaker_features) -> waveform [1,N]
//! ```
//!
//! Three things here are only knowable by running it, and each cost a wrong answer first:
//!
//! * **`logits` is `[1, seq, 8194]`, not `[1, 1, 8194]`.** The graph returns a row per
//!   position; only the last one is next-token logits. Slicing the wrong axis yields a
//!   confident token id from inside the middle of the tensor — in the turbo port's case,
//!   800497, a token that does not exist.
//! * **After the prefill, `inputs_embeds` is a single frame while `attention_mask` keeps
//!   growing.** They do not share a length, and tagging the embeds with the mask's length
//!   emits noise rather than failing.
//! * **The 8194-wide vocabulary is not filtered, and the speech codes are at the *bottom*.**
//!   `START_SPEECH` is 6561 and `STOP_SPEECH` is 6562, and the 1630 ids above them are dead:
//!   across three utterances (English, French, a cloned voice) nothing above 6562 was ever
//!   emitted, and the speech codes the decoder actually received lay in 0..6537. So of the two
//!   wrong "fixes" only one is obvious and the other is the trap: masking everything above
//!   6562 is pointless but harmless, while the tempting "6561 is where speech starts, keep
//!   6563..8193" reading would hand the decoder nothing at all. The reference does neither —
//!   it takes `logits[:, -1, :]` and argmaxes over all 8194 — and so does this, with
//!   `Utterance::speech_code_low`/`high`/`above_stop` carrying the measurement so a checkpoint
//!   that changes its mind about that range is a number in a test rather than a surprise.
//!
//! The generation loop mirrors `reference_multi_inference.py` from
//! `onnx-community/chatterbox-multilingual-ONNX` step for step, including the two details
//! that are easy to "fix" into a bug: the prefill `position_ids` are
//! `where(input_ids >= START_SPEECH, 0, arange - 1)` — so the embedding-position counter
//! starts at **−1** — and each subsequent single-frame position is the **loop counter**,
//! not the sequence length.

use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use ort::session::{Session, SessionInputValue};
use ort::value::{DynValue, Tensor, TensorElementType, ValueType};
use tokenizers::Tokenizer;

/// 24 kHz mono, fixed by the conditional decoder.
pub const SAMPLE_RATE: u32 = 24000;

/// First and last ids the sampler treats specially. The 1630 ids above `STOP` are dead —
/// across three utterances nothing above 6562 was ever emitted, and the codes the decoder
/// received lay in 0..6537 (see the module doc, which records the measurement).
pub const START_SPEECH_TOKEN: i64 = 6561;
pub const STOP_SPEECH_TOKEN: i64 = 6562;

/// 30 transformer layers, 16 KV heads of width 64 — read off the graph, and asserted at
/// load so a re-export that changes shape says so instead of producing noise.
const NUM_LAYERS: usize = 30;
const NUM_KV_HEADS: i64 = 16;
const HEAD_DIM: i64 = 64;
const HIDDEN: usize = 1024;

/// The reference's `repetition_penalty`, applied over every token generated so far
/// (including the start token) on the last position's logits only.
const REPETITION_PENALTY: f32 = 1.2;

/// Hard stop. The reference's own `max_new_tokens` default.
pub const MAX_NEW_TOKENS: usize = 256;

/// Bounds a reference clip may fall inside. Too short and the speaker embedding is noise;
/// too long and a five-minute file is pushed through a 591 MB encoder for nothing.
pub const MIN_REF_SECONDS: f32 = 0.5;
pub const MAX_REF_SECONDS: f32 = 30.0;

/// One utterance's audio plus what it took to make it. Returned rather than logged so the
/// caller — and the integration test — can report real numbers.
#[derive(Debug, Clone)]
pub struct Utterance {
    pub samples: Vec<f32>,
    /// Tokens the whole prompt grew to before `MAX_NEW_TOKENS` or a stop token ended it.
    pub steps: usize,
    /// Speech tokens handed to the decoder, prompt included.
    pub speech_tokens: usize,
    /// True when the loop hit `MAX_NEW_TOKENS` without ever emitting a stop token.
    pub hit_max: bool,
    /// The lowest and highest token id the loop generated, start token included.
    /// `START_SPEECH`, `STOP_SPEECH` and the speech codes all live in here; which is which is
    /// what `speech_code_low`/`high` and `above_stop` separate out.
    pub generated_low: i64,
    pub generated_high: i64,
    /// Lowest and highest *speech code*: generated ids that are neither the start nor the stop
    /// token. These are what the decoder is actually handed, and on this checkpoint they are
    /// 0..6560 — below the specials, not above them.
    pub speech_code_low: i64,
    pub speech_code_high: i64,
    /// How many generated ids landed above `STOP_SPEECH`, in 6563..8193. Measured rather than
    /// assumed: the module doc's claim about that range rests on this number being 0.
    pub above_stop: usize,
    pub encoder_seconds: f64,
    pub loop_seconds: f64,
    pub decoder_seconds: f64,
}

impl Utterance {
    pub fn seconds(&self) -> f32 {
        self.samples.len() as f32 / SAMPLE_RATE as f32
    }
}

/// The four graphs and everything the generation loop needs to build their inputs.
pub struct Chatterbox {
    encoder: Session,
    embed: Session,
    lm: Session,
    decoder: Session,
    tokenizer: Tokenizer,
    /// `past_key_values.L.key` / `.value` in the reference's own iteration order
    /// (layer-major, key before value). Positional: index `j` here is output `j + 1`, which
    /// is what `logits, *present = run()` relies on. The matching `present.*` names are
    /// checked at load, not carried: nothing after that needs them.
    kv_inputs: Vec<String>,
    /// Whether the cache is declared f16 (it is, for the q4f16 language model).
    kv_f16: bool,
    /// `speech_encoder`'s outputs in declared order.
    encoder_outputs: Vec<String>,
    /// Emotion intensity, an input to `embed_tokens` rather than a sampling knob.
    exaggeration: f32,
    /// The Chinese character mapping, when the install has it.
    cangjie: Option<CangjieMap>,
}

impl Chatterbox {
    /// Load all four graphs, the tokenizer and the Cangjie mapping from `dir`.
    ///
    /// `dir` is the engine's own directory (`models/chatterbox`): the graphs are in `onnx/`
    /// and the text front end at the root, matching the download plan exactly.
    pub fn load(dir: &Path, exaggeration: f32) -> Result<Self, String> {
        let t = Instant::now();
        let encoder = load_session(&dir.join(crate::engine_paths::CHATTERBOX_ENCODER_FILE))?;
        let embed = load_session(&dir.join(crate::engine_paths::CHATTERBOX_EMBED_FILE))?;
        let lm = load_session(&dir.join(crate::engine_paths::CHATTERBOX_LM_FILE))?;
        let decoder = load_session(&dir.join(crate::engine_paths::CHATTERBOX_DECODER_FILE))?;
        eprintln!(
            "[kiegen] chatterbox: 4 graphs in {:.2}s",
            t.elapsed().as_secs_f64()
        );

        let tokenizer_path = dir.join(crate::engine_paths::CHATTERBOX_TOKENIZER_FILE);
        let tokenizer = Tokenizer::from_file(&tokenizer_path)
            .map_err(|e| format!("load {tokenizer_path:?}: {e}"))?;

        let cangjie_path = dir.join(crate::engine_paths::CHATTERBOX_CANGJIE_FILE);
        let cangjie = match CangjieMap::load(&cangjie_path) {
            Ok(map) => Some(map),
            Err(error) => {
                // Not fatal: only `zh` needs it, and `zh` says so when it is asked for.
                eprintln!("[kiegen] chatterbox: no Cangjie mapping ({error}); zh will refuse");
                None
            }
        };

        // Build the cache name lists explicitly rather than reading `session.inputs()` in
        // order: the names are `past_key_values.10.key`, which sorts before `.2.`, so any
        // string-ordered traversal of 60 tensors pairs the wrong layer with the wrong
        // output. This is the reference's own loop order and the graph's.
        let mut kv_inputs = Vec::with_capacity(NUM_LAYERS * 2);
        let mut kv_outputs = Vec::with_capacity(NUM_LAYERS * 2);
        for layer in 0..NUM_LAYERS {
            for kv in ["key", "value"] {
                kv_inputs.push(format!("past_key_values.{layer}.{kv}"));
                kv_outputs.push(format!("present.{layer}.{kv}"));
            }
        }
        let declared: Vec<&str> = lm.inputs().iter().map(|input| input.name()).collect();
        let missing: Vec<&String> = kv_inputs
            .iter()
            .filter(|name| !declared.contains(&name.as_str()))
            .collect();
        if !missing.is_empty() {
            return Err(format!(
                "the language model has no {missing:?}; this is not the 30-layer x 2 export \
                 this engine was written against"
            ));
        }
        let outputs: Vec<&str> = lm.outputs().iter().map(|output| output.name()).collect();
        let present_missing: Vec<&String> = kv_outputs
            .iter()
            .filter(|name| !outputs.contains(&name.as_str()))
            .collect();
        if !present_missing.is_empty() {
            return Err(format!(
                "the language model returns no {present_missing:?}; the cache cannot be fed back"
            ));
        }

        let kv_f16 = f16_cache(&lm).ok_or_else(|| {
            "the language model does not declare a float16 KV cache; this build assumes the \
             q4f16 export"
                .to_string()
        })?;

        let encoder_outputs: Vec<String> = encoder
            .outputs()
            .iter()
            .map(|output| output.name().to_string())
            .collect();

        Ok(Self {
            encoder,
            embed,
            lm,
            decoder,
            tokenizer,
            kv_inputs,
            kv_f16,
            encoder_outputs,
            exaggeration,
            cangjie,
        })
    }

    pub fn exaggeration(&self) -> f32 {
        self.exaggeration
    }

    /// The tokenizer this engine loaded, for parity checks against the Python reference.
    pub fn tokenizer(&self) -> &Tokenizer {
        &self.tokenizer
    }

    /// Text → speech tokens → 24 kHz audio, cloning `voice`.
    ///
    /// `language` is one of the 23 codes in `engines::CHATTERBOX_LANGUAGES`.
    pub fn synthesize(
        &mut self,
        text: &str,
        language: &str,
        voice: &Path,
    ) -> Result<Utterance, String> {
        let prompt = prepare_text(text, language, self.cangjie.as_ref())?;
        let encoding = self
            .tokenizer
            .encode(prompt.as_str(), true)
            .map_err(|e| format!("tokenize: {e}"))?;
        let ids: Vec<i64> = encoding.get_ids().iter().map(|id| *id as i64).collect();
        if ids.is_empty() {
            return Err("nothing to say: that text produced no tokens".to_string());
        }

        // ── the reference voice, once ────────────────────────────────────────────
        let audio = read_reference_mono_24k(voice)?;
        let samples_in = audio.len();

        let started = Instant::now();
        let enc_names = self.encoder_outputs.clone();
        let mut encoded = self
            .encoder
            .run(ort::inputs![
                "audio_values" => Tensor::from_array((vec![1i64, samples_in as i64], audio))
                    .map_err(|e| format!("audio_values tensor: {e}"))?
            ])
            .map_err(|e| format!("speech_encoder: {e}"))?;
        let mut features: Vec<DynValue> = Vec::with_capacity(enc_names.len());
        for name in &enc_names {
            features.push(
                encoded
                    .remove(name)
                    .ok_or_else(|| format!("speech_encoder returned no {name:?}"))?,
            );
        }
        if features.len() != 4 {
            return Err(format!(
                "speech_encoder returned {} outputs, expected 4",
                features.len()
            ));
        }
        drop(encoded);
        let encoder_seconds = started.elapsed().as_secs_f64();
        let (cond_emb, prompt_tokens, speaker_embeddings, speaker_features) =
            (&features[0], &features[1], &features[2], &features[3]);

        // ── text → embeddings, and the prompt in front of them ───────────────────
        let text_len = ids.len();
        let text_position_ids = prefill_position_ids(&ids);
        let mut embeds = self.embed_tokens(&ids, &text_position_ids)?;
        let dim = embeds.len() / text_len.max(1);
        if dim != HIDDEN {
            return Err(format!(
                "embed_tokens produced {dim}-wide vectors, expected {HIDDEN}"
            ));
        }
        let cond = as_f32(cond_emb)?;
        let cond_len = cond.len() / dim;
        let mut inputs_embeds = Vec::with_capacity(cond.len() + embeds.len());
        inputs_embeds.append(&mut cond.clone());
        inputs_embeds.append(&mut embeds);
        let mut mask_len = cond_len + text_len;

        // ── the cache, empty and typed as the graph declares it ──────────────────
        let mut past: Vec<DynValue> = (0..self.kv_inputs.len())
            .map(|_| zeros_typed(vec![1, NUM_KV_HEADS, 0, HEAD_DIM], self.kv_f16))
            .collect::<Result<_, String>>()?;

        let kv_inputs = self.kv_inputs.clone();
        let mut generated: Vec<i64> = vec![START_SPEECH_TOKEN];
        let mut speech_rows: Vec<i64> = Vec::new();
        let mut hit_max = true;

        let loop_started = Instant::now();
        for step in 0..MAX_NEW_TOKENS {
            let mut named: Vec<(std::borrow::Cow<'_, str>, SessionInputValue<'_>)> =
                Vec::with_capacity(3 + kv_inputs.len());
            let frames = inputs_embeds.len() / dim;
            named.push((
                std::borrow::Cow::Borrowed("inputs_embeds"),
                Tensor::from_array((
                    vec![1i64, frames as i64, dim as i64],
                    std::mem::take(&mut inputs_embeds),
                ))
                .map_err(|e| format!("inputs_embeds tensor: {e}"))?
                .into(),
            ));
            named.push((
                std::borrow::Cow::Borrowed("attention_mask"),
                Tensor::from_array((vec![1i64, mask_len as i64], vec![1i64; mask_len]))
                    .map_err(|e| format!("attention_mask tensor: {e}"))?
                    .into(),
            ));
            // The cache is fed back in the order the names were built in, which is exactly
            // the order the graph returns it in.
            for name in &kv_inputs {
                let value = past.remove(0);
                named.push((std::borrow::Cow::Borrowed(name.as_str()), value.into()));
            }

            let outputs = self
                .lm
                .run(named)
                .map_err(|e| format!("language_model step {step}: {e}"))?;
            let mut values: Vec<DynValue> = outputs.into_iter().map(|(_, value)| value).collect();
            if values.len() != kv_inputs.len() + 1 {
                return Err(format!(
                    "language_model returned {} outputs for {} cache inputs",
                    values.len(),
                    kv_inputs.len()
                ));
            }
            // `logits` is declared first, and the present tensors follow it — which is what
            // `logits, *present = run()` unpacks positionally in the reference.
            let present = values.split_off(1);
            let logits = values.pop().expect("one value was split off");
            past = present;

            let next = last_position_argmax(&logits, &generated)?;
            generated.push(next);
            if next == STOP_SPEECH_TOKEN {
                hit_max = false;
                break;
            }
            speech_rows.push(next);

            // One frame in, tagged with the *loop counter* — not the sequence length. The
            // reference's `position_ids = np.full((b, 1), i + 1)`.
            let position = vec![(step + 1) as i64];
            inputs_embeds = self.embed_tokens(&[next], &position)?;
            mask_len += 1;
        }
        let loop_seconds = loop_started.elapsed().as_secs_f64();

        // `speech_tokens = generate_tokens[:, 1:-1]`, then the prompt in front of it. The
        // stop token is dropped; the start token never had audio.
        let mut speech: Vec<i64> = as_i64(prompt_tokens)?;
        speech.extend_from_slice(&speech_rows);

        let decode_started = Instant::now();
        let len = speech.len();
        let mut decoded = self
            .decoder
            .run(ort::inputs![
                "speech_tokens" => Tensor::from_array((vec![1i64, len as i64], speech))
                    .map_err(|e| format!("speech_tokens tensor: {e}"))?,
                "speaker_embeddings" => speaker_embeddings.view(),
                "speaker_features" => speaker_features.view(),
            ])
            .map_err(|e| format!("conditional_decoder: {e}"))?;
        let name = decoded
            .keys()
            .next()
            .map(str::to_string)
            .ok_or("conditional_decoder returned nothing")?;
        let waveform = decoded
            .remove(&name)
            .ok_or_else(|| format!("conditional_decoder returned no {name:?}"))?;
        let samples = as_f32(&waveform)?;
        let decoder_seconds = decode_started.elapsed().as_secs_f64();

        if samples.is_empty() {
            return Err("the decoder returned no samples".to_string());
        }
        // Split the generated ids into the two ranges the doc describes: the speech codes the
        // decoder consumes, and the two specials. Everything else is counted rather than
        // rejected, so a checkpoint that starts using 6563..8193 shows up as a number.
        let codes = generated
            .iter()
            .copied()
            .filter(|id| *id != START_SPEECH_TOKEN && *id != STOP_SPEECH_TOKEN);
        let speech_code_low = codes.clone().min().unwrap_or(START_SPEECH_TOKEN);
        let speech_code_high = codes.clone().max().unwrap_or(START_SPEECH_TOKEN);
        Ok(Utterance {
            samples,
            steps: generated.len(),
            speech_tokens: len,
            hit_max,
            generated_low: generated
                .iter()
                .copied()
                .min()
                .unwrap_or(START_SPEECH_TOKEN),
            generated_high: generated
                .iter()
                .copied()
                .max()
                .unwrap_or(START_SPEECH_TOKEN),
            speech_code_low,
            speech_code_high,
            above_stop: generated
                .iter()
                .filter(|id| **id > STOP_SPEECH_TOKEN)
                .count(),
            encoder_seconds,
            loop_seconds,
            decoder_seconds,
        })
    }

    /// One `embed_tokens` pass. Its three inputs are all required in this export — the
    /// English-only turbo graphs took `input_ids` alone.
    fn embed_tokens(&mut self, ids: &[i64], position_ids: &[i64]) -> Result<Vec<f32>, String> {
        let len = ids.len() as i64;
        let mut run = self
            .embed
            .run(ort::inputs![
                "input_ids" => Tensor::from_array((vec![1i64, len], ids.to_vec()))
                    .map_err(|e| format!("input_ids tensor: {e}"))?,
                "position_ids" => Tensor::from_array((vec![1i64, position_ids.len() as i64], position_ids.to_vec()))
                    .map_err(|e| format!("position_ids tensor: {e}"))?,
                "exaggeration" => Tensor::from_array((vec![1i64], vec![self.exaggeration]))
                    .map_err(|e| format!("exaggeration tensor: {e}"))?,
            ])
            .map_err(|e| format!("embed_tokens: {e}"))?;
        let name = run
            .keys()
            .next()
            .map(str::to_string)
            .ok_or("embed_tokens returned nothing")?;
        let value = run
            .remove(&name)
            .ok_or_else(|| format!("embed_tokens returned no {name:?}"))?;
        as_f32(&value)
    }
}

// ──────────────────────────── graph plumbing ────────────────────────────

/// Load one graph. Its `.onnx` is useless without the sibling `*_onnx_data` blob, so a
/// failure here is as likely to be a half-finished download as a bad export.
fn load_session(path: &Path) -> Result<Session, String> {
    Session::builder()
        .map_err(|e| format!("onnxruntime init: {e}"))?
        .commit_from_file(path)
        .map_err(|e| format!("load {path:?}: {e}"))
}

fn dtype_name(value: &ValueType) -> String {
    value
        .tensor_type()
        .map(|t| t.to_string())
        .unwrap_or_else(|| "?".to_string())
}

fn shape_of(value: &ValueType) -> Vec<i64> {
    value.tensor_shape().map(|s| s.to_vec()).unwrap_or_default()
}

/// Does this session declare a float16 KV cache? Read off the graph rather than assumed,
/// because `ort` needs an f16 tensor to satisfy it and an f32 one is rejected at run time.
fn f16_cache(session: &Session) -> Option<bool> {
    session
        .inputs()
        .iter()
        .find(|input| input.name().starts_with("past_key_values."))
        .map(|input| {
            matches!(
                input.dtype().tensor_type(),
                Some(TensorElementType::Float16)
            )
        })
}

/// A zero tensor in whichever of the two cache dtypes this graph declares.
fn zeros_typed(shape: Vec<i64>, f16: bool) -> Result<DynValue, String> {
    let count: usize = shape
        .iter()
        .map(|dim| usize::try_from(*dim).unwrap_or(0))
        .product();
    if f16 {
        Ok(Tensor::from_array((shape, vec![half::f16::ZERO; count]))
            .map_err(|e| format!("cache tensor: {e}"))?
            .upcast()
            .into_dyn())
    } else {
        Ok(Tensor::from_array((shape, vec![0f32; count]))
            .map_err(|e| format!("cache tensor: {e}"))?
            .upcast()
            .into_dyn())
    }
}

/// Read any float tensor as f32, converting from f16 when that is what the graph returned.
fn as_f32(value: &DynValue) -> Result<Vec<f32>, String> {
    match value.dtype().tensor_type() {
        Some(TensorElementType::Float32) => Ok(value
            .try_extract_tensor::<f32>()
            .map_err(|e| format!("read float32 tensor: {e}"))?
            .1
            .to_vec()),
        Some(TensorElementType::Float16) => Ok(value
            .try_extract_tensor::<half::f16>()
            .map_err(|e| format!("read float16 tensor: {e}"))?
            .1
            .iter()
            .map(|x| x.to_f32())
            .collect()),
        other => Err(format!("tensor dtype {other:?} is neither f32 nor f16")),
    }
}

fn as_i64(value: &DynValue) -> Result<Vec<i64>, String> {
    Ok(value
        .try_extract_tensor::<i64>()
        .map_err(|e| format!("read int64 tensor: {e}"))?
        .1
        .to_vec())
}

/// Next-token id from a `[1, seq, vocab]` logits tensor: the **last position**, over the
/// **whole** vocabulary, with the repetition penalty applied to every token that has already
/// been produced.
///
/// This is `logits[:, -1, :]` followed by `RepetitionPenaltyLogitsProcessor` and `argmax`,
/// and the two shapes it gets right are the whole reason it is a named function.
fn last_position_argmax(logits: &DynValue, generated: &[i64]) -> Result<i64, String> {
    let shape = shape_of(logits.dtype());
    let vocab = shape
        .last()
        .copied()
        .filter(|v| *v > 0)
        .ok_or_else(|| format!("logits have no vocabulary axis: {shape:?}"))?
        as usize;
    let all = as_f32(logits)?;
    if all.len() < vocab {
        return Err(format!(
            "logits hold {} floats for a {vocab}-token vocabulary",
            all.len()
        ));
    }
    // The last `vocab` floats are the last position's row, whatever the leading dims are.
    let mut row = all[all.len() - vocab..].to_vec();
    if row.iter().any(|value| !value.is_finite()) {
        return Err("the language model produced a non-finite logit".to_string());
    }
    for token in generated {
        let index = *token as usize;
        if index < vocab {
            row[index] = if row[index] < 0.0 {
                row[index] * REPETITION_PENALTY
            } else {
                row[index] / REPETITION_PENALTY
            };
        }
    }
    let mut best = 0usize;
    for (index, value) in row.iter().enumerate() {
        if *value > row[best] {
            best = index;
        }
    }
    Ok(best as i64)
}

/// `np.where(input_ids >= START_SPEECH_TOKEN, 0, arange(n) - 1)` — the prefill positions.
///
/// The −1 is not a mistake in the reference and not one here: the first text token is
/// position −1 because the prompt embeddings are prepended in front of it later. Can the LM
/// take a negative index at all? It does not receive positions; only `embed_tokens` does,
/// and an embedding lookup is unaffected by the sign.
pub fn prefill_position_ids(ids: &[i64]) -> Vec<i64> {
    ids.iter()
        .enumerate()
        .map(|(index, id)| {
            if *id >= START_SPEECH_TOKEN {
                0
            } else {
                index as i64 - 1
            }
        })
        .collect()
}

/// Diagnostics for a loaded graph set — what the export actually declares.
pub fn describe(session: &Session) -> String {
    let render = |outlets: &[ort::value::Outlet]| {
        outlets
            .iter()
            .map(|outlet| {
                format!(
                    "{} {} {:?}",
                    outlet.name(),
                    dtype_name(outlet.dtype()),
                    shape_of(outlet.dtype())
                )
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    format!(
        "in[{}]: {}\nout[{}]: {}",
        session.inputs().len(),
        render(session.inputs()),
        session.outputs().len(),
        render(session.outputs())
    )
}

// ──────────────────────────── text front end ────────────────────────────

/// Chatterbox's whole language mechanism: normalise, then prefix `[xx]`.
///
/// The prefix is a real token in the tokenizer's own vocabulary (`[fr]` = 634), which is how
/// a graph with no language input still speaks French.
pub fn prepare_text(
    text: &str,
    language: &str,
    cangjie: Option<&CangjieMap>,
) -> Result<String, String> {
    let code = language.trim().to_ascii_lowercase();
    if crate::engines::chatterbox_language(&code).is_none() {
        return Err(format!(
            "'{language}' is not one of Chatterbox's 23 languages"
        ));
    }
    if let Some(reason) = crate::engines::chatterbox_language_blocked(&code) {
        return Err(format!(
            "Chatterbox cannot read {} yet: {reason}",
            crate::engines::chatterbox_language(&code).unwrap_or(&code)
        ));
    }
    let body = match code.as_str() {
        "ko" => korean_normalize(text),
        "zh" => {
            let map = cangjie.ok_or_else(|| {
                "Chatterbox cannot read Chinese yet: this install has no Cangjie5_TC.json"
                    .to_string()
            })?;
            cangjie_convert(text, map)
        }
        _ => text.to_string(),
    };
    Ok(format!("[{code}]{body}"))
}

/// Korean: decompose each precomposed syllable into its Jamo components, which are what the
/// tokenizer has entries for. Pure arithmetic, so it is ported exactly.
pub fn korean_normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        let code = character as u32;
        if (0xAC00..=0xD7A3).contains(&code) {
            let base = code - 0xAC00;
            let initial = char::from_u32(0x1100 + base / (21 * 28)).unwrap_or(character);
            let medial = char::from_u32(0x1161 + (base % (21 * 28)) / 28).unwrap_or(character);
            out.push(initial);
            out.push(medial);
            if !base.is_multiple_of(28) {
                if let Some(final_) = char::from_u32(0x11A7 + base % 28) {
                    out.push(final_);
                }
            }
        } else {
            out.push(character);
        }
    }
    out.trim().to_string()
}

/// Is this a character a Cangjie code could exist for?
///
/// The reference gates on `unicodedata.category(t) == "Lo"`, which in Rust has no std
/// equivalent. The Cangjie table only covers CJK ideographs, and every ideograph the table
/// does *not* cover is passed through unchanged by both implementations — so testing the
/// ideograph ranges is the same predicate for every character that can differ.
fn is_cjk_ideograph(character: char) -> bool {
    matches!(character as u32,
        0x3400..=0x4DBF      // CJK Unified Ideographs Extension A
        | 0x4E00..=0x9FFF    // CJK Unified Ideographs
        | 0xF900..=0xFAFF    // Compatibility Ideographs
        | 0x20000..=0x2FA1F  // Extensions B..F and Compatibility Supplement
    )
}

/// The reference's Cangjie conversion, character by character.
///
/// One deliberate gap: the Python version runs the text through `pkuseg` first and rejoins
/// the words with spaces. pkuseg is a Python package with a trained model and is not
/// ported, which puts this on the path the reference *itself* takes when pkuseg is missing
/// (`full_text = text`). So the mapping is complete and the word segmentation is not — the
/// `zh` catalogue row says so.
pub fn cangjie_convert(text: &str, map: &CangjieMap) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        if !is_cjk_ideograph(character) {
            out.push(character);
            continue;
        }
        match map.encode(character) {
            Some(code) => {
                for symbol in code.chars() {
                    out.push_str("[cj_");
                    out.push(symbol);
                    out.push(']');
                }
                out.push_str("[cj_.]");
            }
            None => out.push(character),
        }
    }
    out
}

/// `Cangjie5_TC.json`: one `word<TAB>code` line per entry, which is why this parses a JSON
/// array of strings rather than an object.
///
/// Kept as a pair of maps because the reference's disambiguation index is
/// `cj2word[code].index(word)` — the position of the character within its own code's word
/// list, which is a property of how the file was written and cannot be recomputed.
pub struct CangjieMap {
    word2cj: HashMap<String, String>,
    cj2word: HashMap<String, Vec<String>>,
}

impl CangjieMap {
    pub fn load(path: &Path) -> Result<Self, String> {
        let raw = std::fs::read_to_string(path).map_err(|e| format!("read {path:?}: {e}"))?;
        Self::parse(&raw)
    }

    pub fn parse(raw: &str) -> Result<Self, String> {
        let entries: Vec<String> =
            serde_json::from_str(raw).map_err(|e| format!("Cangjie5_TC.json: {e}"))?;
        let mut word2cj = HashMap::with_capacity(entries.len());
        let mut cj2word: HashMap<String, Vec<String>> = HashMap::new();
        for entry in entries {
            let mut parts = entry.split('\t');
            let (Some(word), Some(code)) = (parts.next(), parts.next()) else {
                continue;
            };
            if word.is_empty() || code.is_empty() {
                continue;
            }
            word2cj.insert(word.to_string(), code.to_string());
            cj2word
                .entry(code.to_string())
                .or_default()
                .push(word.to_string());
        }
        if word2cj.is_empty() {
            return Err("Cangjie5_TC.json holds no entries".to_string());
        }
        Ok(Self { word2cj, cj2word })
    }

    pub fn len(&self) -> usize {
        self.word2cj.len()
    }

    pub fn is_empty(&self) -> bool {
        self.word2cj.is_empty()
    }

    /// `code + str(index)`, with index 0 written as the empty string — the reference's
    /// `str(index) if index > 0 else ""`.
    fn encode(&self, character: char) -> Option<String> {
        let word = character.to_string();
        let code = self.word2cj.get(&word)?;
        let index = self.cj2word.get(code)?.iter().position(|w| *w == word)?;
        Some(if index > 0 {
            format!("{code}{index}")
        } else {
            code.to_string()
        })
    }
}

// ──────────────────────────── reference clips ────────────────────────────

/// Read a reference clip as mono f32 at 24 kHz, converting whatever it is into that.
///
/// The graphs will happily accept a buffer at any rate and produce confident, wrong speech,
/// so nothing is passed through un-normalised: stereo is averaged down and a different
/// sample rate is linearly resampled. Linear is not a great resampler, but a wrong rate is
/// not a slightly-worse voice, it is the wrong voice — pitch-shifted and mis-segmented.
pub fn read_reference_mono_24k(path: &Path) -> Result<Vec<f32>, String> {
    let mut reader =
        hound::WavReader::open(path).map_err(|e| format!("read reference clip {path:?}: {e}"))?;
    let spec = reader.spec();
    if spec.channels == 0 {
        return Err(format!("{path:?} declares no channels"));
    }
    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("read {path:?}: {e}"))?,
        hound::SampleFormat::Int => {
            if spec.bits_per_sample == 0 || spec.bits_per_sample > 32 {
                return Err(format!(
                    "{path:?} holds {}-bit samples, which this cannot scale",
                    spec.bits_per_sample
                ));
            }
            let scale = (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|sample| sample.map(|value| value as f32 / scale))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| format!("read {path:?}: {e}"))?
        }
    };
    if interleaved.is_empty() {
        return Err(format!("{path:?} holds no samples"));
    }

    let channels = spec.channels as usize;
    let mono: Vec<f32> = if channels <= 1 {
        interleaved
    } else {
        interleaved
            .chunks(channels)
            .map(|frame| frame.iter().sum::<f32>() / channels as f32)
            .collect()
    };
    Ok(resample_linear(&mono, spec.sample_rate, SAMPLE_RATE))
}

/// Linear interpolation between the two neighbouring input samples. No anti-aliasing
/// filter: this exists to stop a 44.1 kHz file being read as if it were 24 kHz, and a
/// filtered resampler is a dependency this does not currently need.
pub fn resample_linear(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || input.is_empty() || from == 0 || to == 0 {
        return input.to_vec();
    }
    let ratio = from as f64 / to as f64;
    let out_len = ((input.len() as f64) / ratio).round() as usize;
    let mut out = Vec::with_capacity(out_len);
    for index in 0..out_len {
        let position = index as f64 * ratio;
        let left = position.floor() as usize;
        let fraction = (position - left as f64) as f32;
        let a = input[left.min(input.len() - 1)];
        let b = input[(left + 1).min(input.len() - 1)];
        out.push(a + (b - a) * fraction);
    }
    out
}

/// Length of a clip in seconds, without decoding it. Used to list voices cheaply.
pub fn wav_seconds(path: &Path) -> Option<f32> {
    let reader = hound::WavReader::open(path).ok()?;
    let spec = reader.spec();
    if spec.sample_rate == 0 {
        return None;
    }
    Some(reader.duration() as f32 / spec.sample_rate as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &str = "The quick brown fox jumps over the lazy dog.";

    /// The prefix is the entire language mechanism, so it is asserted directly: the code
    /// must reach the tokenizer in lower case and in brackets.
    #[test]
    fn the_language_prefix_is_written_in_front_of_the_text() {
        assert_eq!(prepare_text("bonjour", "fr", None).unwrap(), "[fr]bonjour");
        assert_eq!(prepare_text("hello", "en", None).unwrap(), "[en]hello");
        // A language the checkpoint does not have must not silently read as English.
        assert!(prepare_text("hola", "xx", None).is_err());
        assert!(prepare_text("hola", "", None).is_err());
    }

    /// The two languages this build cannot normalise must refuse by name rather than
    /// emitting unnormalised text the model was never trained to read.
    #[test]
    fn the_unnormalised_languages_refuse_instead_of_writing_something_else() {
        for (code, name) in [("ja", "Japanese"), ("he", "Hebrew")] {
            let error = prepare_text("test", code, None).expect_err("must refuse");
            assert!(
                error.contains(name),
                "the refusal must name the language: {error}"
            );
            assert!(
                crate::engines::chatterbox_language_blocked(code).is_some(),
                "{code} must be marked blocked in the catalogue too"
            );
        }
        // Every other code is accepted, so the gate is a gate and not a blanket refusal.
        for (code, _) in crate::engines::CHATTERBOX_LANGUAGES {
            if matches!(*code, "ja" | "he") {
                continue;
            }
            let prepared = prepare_text("text", code, None);
            if *code == "zh" {
                // zh needs the mapping file; without it the refusal must say which file.
                assert!(prepared.unwrap_err().contains("Cangjie5_TC.json"));
            } else {
                assert_eq!(prepared.unwrap(), format!("[{code}]text"));
            }
        }
    }

    /// Hangul decomposition, checked against the arithmetic in the reference. `한` is
    /// U+D55C: base 0x1F5C, initial 0x1112, medial 0x1161, final 0x11AB.
    #[test]
    fn korean_syllables_decompose_into_jamo() {
        assert_eq!(
            korean_normalize("한글"),
            "\u{1112}\u{1161}\u{11ab}\u{1100}\u{1173}\u{11af}"
        );
        // A syllable with no final consonant gets no third component: `가` is U+AC00.
        assert_eq!(korean_normalize("가"), "\u{1100}\u{1161}");
        // Latin and punctuation pass through untouched, and the result is trimmed exactly
        // as the reference's `.strip()` trims it.
        assert_eq!(korean_normalize("  ok! "), "ok!");
        assert_eq!(korean_normalize(""), "");
    }

    /// The Cangjie path, against the shipped table. `我` is `hqi` and is the first word for
    /// that code, so it takes no disambiguation index.
    #[test]
    fn cangjie_encodes_ideographs_and_passes_everything_else_through() {
        let json = r#"["我\thqi","你\tnf","你好\tnf"]"#;
        let map = CangjieMap::parse(json).unwrap();
        assert_eq!(map.len(), 3);
        assert_eq!(cangjie_convert("我", &map), "[cj_h][cj_q][cj_i][cj_.]");
        // `炸` is not in this table: leaving it alone is right, mangling it is not.
        assert_eq!(cangjie_convert("炸", &map), "炸");
        // Latin, digits and punctuation are not Lo and are copied.
        assert_eq!(cangjie_convert("a1, ", &map), "a1, ");
        // Two characters sharing a code: the second carries its index.
        let shared = CangjieMap::parse(r#"["你\tnf","妳\tnf"]"#).unwrap();
        assert_eq!(cangjie_convert("你", &shared), "[cj_n][cj_f][cj_.]");
        assert_eq!(cangjie_convert("妳", &shared), "[cj_n][cj_f][cj_1][cj_.]");
    }

    #[test]
    fn a_cangjie_file_that_is_not_the_mapping_is_an_error() {
        assert!(CangjieMap::parse("{}").is_err());
        assert!(CangjieMap::parse("not json").is_err());
        assert!(CangjieMap::parse("[]").is_err());
    }

    /// The prefill positions, which are the reference's `where(...)` including its −1.
    #[test]
    fn prefill_positions_start_at_minus_one_and_zero_the_speech_tokens() {
        assert_eq!(prefill_position_ids(&[708, 296, 62]), vec![-1, 0, 1]);
        assert_eq!(
            prefill_position_ids(&[6563, 255, 708, 0, 6561, 6561]),
            vec![0, 0, 1, 2, 0, 0]
        );
        assert!(prefill_position_ids(&[]).is_empty());
    }

    /// A wrong sample rate is the difference between a voice and a chipmunk, so the
    /// resampler is measured rather than assumed: 48 kHz down to 24 kHz halves the length,
    /// and a ramp stays a ramp.
    #[test]
    fn resampling_halves_the_length_and_keeps_the_signal() {
        let input: Vec<f32> = (0..100).map(|index| index as f32 / 100.0).collect();
        let out = resample_linear(&input, 48000, 24000);
        assert_eq!(out.len(), 50);
        assert!((out[0] - input[0]).abs() < 1e-6);
        // Linear interpolation of a linear ramp is the ramp: output index 1 sits at input
        // position 2, which is 0.02.
        assert!((out[1] - 0.02).abs() < 1e-6, "got {}", out[1]);
        // Same rate is a copy, and nothing about an empty buffer panics.
        assert_eq!(resample_linear(&input, 24000, 24000), input);
        assert!(resample_linear(&[], 44100, 24000).is_empty());
    }

    /// An arbitrary sample rate must not be accepted as if it were 24 kHz.
    #[test]
    fn a_44k_stereo_clip_is_downmixed_and_resampled() {
        let dir = std::env::temp_dir().join("kiegen-chatterbox-wav");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("stereo-44k.wav");
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 44100,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&path, spec).unwrap();
        // Half a second, hard left in one channel and hard right in the other: the mono
        // average must be silence, which proves the downmix rather than merely counting
        // samples.
        for _ in 0..22050 {
            writer.write_sample(20000i16).unwrap();
            writer.write_sample(-20000i16).unwrap();
        }
        writer.finalize().unwrap();

        let samples = read_reference_mono_24k(&path).unwrap();
        assert_eq!(samples.len(), 12000, "44.1k -> 24k for half a second");
        assert!(
            samples.iter().all(|sample| sample.abs() < 1e-3),
            "opposite channels did not cancel, so the downmix is wrong"
        );
        let seconds = wav_seconds(&path).unwrap();
        assert!((seconds - 0.5).abs() < 0.01, "got {seconds}");
        let _ = std::fs::remove_file(&path);
    }

    /// The last position of a `[1, seq, vocab]` row, not the first — the bug that produced
    /// a token id of 800497 in the turbo port.
    #[test]
    fn argmax_reads_the_last_position_of_a_three_dimensional_tensor() {
        let vocab = 4usize;
        let seq = 3usize;
        // Row 0 and 1 are junk; row 2 (the last) peaks at index 2. Slicing axis 1 instead of
        // axis 2 returns an id out of this tensor's own range, which is what the bug did.
        let mut data = vec![9.0f32; seq * vocab];
        data[2 * vocab + 1] = 42.0;
        let tensor = Tensor::from_array((vec![1i64, seq as i64, vocab as i64], data)).unwrap();
        let value = tensor.upcast().into_dyn();
        assert_eq!(last_position_argmax(&value, &[]).unwrap(), 1);
    }

    /// The repetition penalty, over everything generated including the start token, and its
    /// sign-dependent direction: a *positive* logit is divided, a *negative* one multiplied,
    /// so the penalty always makes an already-produced token less likely but never flips its
    /// sign. Both directions are asserted with a value the wrong rule would get wrong.
    #[test]
    fn the_repetition_penalty_pushes_the_tokens_already_generated_down() {
        let make = |data: Vec<f32>| {
            Tensor::from_array((vec![1i64, 1i64, data.len() as i64], data))
                .unwrap()
                .upcast()
                .into_dyn()
        };

        // A positive logit is divided: 2.5/1.2 = 2.08, so 2.3 overtakes it. Multiplying
        // instead would give 3.0 and leave index 0 the winner.
        let value = make(vec![2.5, 2.3, 0.0]);
        assert_eq!(last_position_argmax(&value, &[]).unwrap(), 0);
        assert_eq!(last_position_argmax(&value, &[0]).unwrap(), 1);

        // A negative logit is multiplied: -1.0*1.2 = -1.2, which is *worse* than -1.1, so
        // the penalised token loses. Dividing would give -0.83 and wrongly keep it.
        let value = make(vec![-1.0, -1.1, -5.0]);
        assert_eq!(last_position_argmax(&value, &[]).unwrap(), 0);
        assert_eq!(last_position_argmax(&value, &[0]).unwrap(), 1);

        // Every token in the list is penalised, not just the last one.
        let value = make(vec![3.0, 3.2, 3.1]);
        assert_eq!(last_position_argmax(&value, &[]).unwrap(), 1);
        assert_eq!(last_position_argmax(&value, &[0, 1]).unwrap(), 2);
        // ...and a token id that is not in the vocabulary is ignored rather than panicking.
        assert_eq!(last_position_argmax(&value, &[99]).unwrap(), 1);
    }

    #[test]
    fn a_non_finite_logit_is_reported_rather_than_sampled() {
        let data = vec![f32::NAN, 1.0, 2.0];
        let tensor = Tensor::from_array((vec![1i64, 1i64, 3i64], data)).unwrap();
        let value = tensor.upcast().into_dyn();
        assert!(last_position_argmax(&value, &[]).is_err());
    }

    /// A missing reference clip must name the file, not panic inside ONNX Runtime.
    #[test]
    fn a_missing_reference_clip_is_an_error_that_names_it() {
        let error = read_reference_mono_24k(Path::new("/nonexistent/voice.wav")).unwrap_err();
        assert!(error.contains("voice.wav"), "got: {error}");
    }

    /// The tokenizer must reproduce the Python reference's ids exactly, prefix, special
    /// tokens and all. Skipped when no tokenizer is on disk.
    #[test]
    fn the_tokenizer_matches_the_python_reference() {
        let Some(dir) = crate::engine_paths::chatterbox_dir() else {
            return;
        };
        let path = dir.join(crate::engine_paths::CHATTERBOX_TOKENIZER_FILE);
        if !path.is_file() {
            eprintln!("skipping: no tokenizer.json at {path:?}");
            return;
        }
        let tokenizer = Tokenizer::from_file(&path).expect("tokenizer");
        let ids: Vec<u32> = tokenizer
            .encode(format!("[en]{TEXT}"), true)
            .expect("encode")
            .get_ids()
            .to_vec();
        // From `Tokenizer.from_file("tokenizer.json").encode("[en]The quick brown fox …")`
        // in the reference, run on this machine: 36 ids, starting with the EXAGGERATION
        // placeholder, BOS and the `[en]` token, ending with EOS and two START_SPEECH.
        assert_eq!(ids.len(), 36, "got {ids:?}");
        assert_eq!(ids[0], 6563, "EXAGGERATION");
        assert_eq!(ids[1], 255, "BOS");
        assert_eq!(ids[2], 708, "the [en] token");
        assert_eq!(ids[ids.len() - 1], 6561, "START_SPEECH");
        assert_eq!(ids[ids.len() - 2], 6561, "START_SPEECH");
        assert_eq!(ids[ids.len() - 3], 0, "EOS");
    }
}
