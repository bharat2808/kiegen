//! Throwaway spike: Chatterbox-Turbo TTS as four chained ONNX graphs via `ort`.
//!
//! See README.md. Question: can the ~2 GB Python/MLX sidecar be replaced by pure Rust?
//!
//! Modes:
//!   inspect          dump every session's declared inputs/outputs, dtypes and shapes
//!   synth [out.wav]  run the full pipeline; times the second (warm) run
//!
//! Pipeline constants and the generation loop are transcribed from Resemble's own
//! inference script for ResembleAI/chatterbox-turbo-ONNX.

use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use ort::session::{Session, SessionInputValue};
use ort::value::{DynValue, Tensor, TensorElementType, ValueType};

// ── Constants, from Resemble's inference script ────────────────────────────────
const SAMPLE_RATE: u32 = 24000;
const START_SPEECH_TOKEN: i64 = 6561;
const STOP_SPEECH_TOKEN: i64 = 6562;
const SILENCE_TOKEN: i64 = 4299;
const NUM_KV_HEADS: usize = 16;
const HEAD_DIM: usize = 64;
const REPETITION_PENALTY: f32 = 1.2;
const MAX_NEW_TOKENS: usize = 256;

const TEXT: &str = "The quick brown fox jumps over the lazy dog.";

fn models_dir() -> PathBuf {
    std::env::var("CHATTERBOX_MODEL_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/Users/home/.hermes/cache/scratch/chatterbox_onnx"))
}

fn graph(dir: &Path, name: &str) -> PathBuf {
    dir.join("onnx").join(format!("{name}_q4f16.onnx"))
}

// ── helpers ────────────────────────────────────────────────────────────────────

fn type_name(v: &ValueType) -> String {
    v.tensor_type().map(|t| t.to_string()).unwrap_or_else(|| "?".into())
}

fn shape_of(v: &ValueType) -> Vec<i64> {
    v.tensor_shape().map(|s| s.to_vec()).unwrap_or_default()
}

fn describe(label: &str, session: &Session) {
    println!("--- {label}: {} inputs, {} outputs ---", session.inputs().len(), session.outputs().len());
    for (i, o) in session.inputs().iter().enumerate() {
        println!("  in [{i:2}] {:<32} {:<6} {:?}", o.name(), type_name(o.dtype()), shape_of(o.dtype()));
    }
    for (i, o) in session.outputs().iter().enumerate() {
        println!("  out[{i:2}] {:<32} {:<6} {:?}", o.name(), type_name(o.dtype()), shape_of(o.dtype()));
    }
}

fn is_f16(dt: &ValueType) -> bool {
    matches!(dt.tensor_type(), Some(TensorElementType::Float16))
}

/// A zero tensor of the requested dtype, shaped `shape`. Used for the initially
/// empty KV cache (past_len = 0).
fn zeros_typed(shape: Vec<i64>, f16: bool) -> Result<DynValue> {
    let n: usize = shape.iter().map(|d| *d as usize).product();
    if f16 {
        Ok(Tensor::from_array((shape, vec![half::f16::ZERO; n]))?.upcast().into_dyn())
    } else {
        Ok(Tensor::from_array((shape, vec![0f32; n]))?.upcast().into_dyn())
    }
}

/// Read any tensor as f32, converting from f16 when that is what the graph declared.
fn as_f32(v: &DynValue) -> Result<Vec<f32>> {
    match v.dtype().tensor_type() {
        Some(TensorElementType::Float32) => Ok(v.try_extract_tensor::<f32>()?.1.to_vec()),
        Some(TensorElementType::Float16) => {
            Ok(v.try_extract_tensor::<half::f16>()?.1.iter().map(|x| x.to_f32()).collect())
        }
        other => Err(anyhow!("tensor dtype {other:?} is neither f32 nor f16")),
    }
}

fn as_i64(v: &DynValue) -> Result<Vec<i64>> {
    Ok(v.try_extract_tensor::<i64>()?.1.to_vec())
}

/// Take the single named output of a run as an owned value (refcounted, shares the
/// session's buffer — no copy of the samples).
fn sole_output(mut outputs: ort::session::SessionOutputs<'_>) -> Result<(String, DynValue)> {
    let name = outputs.keys().next().ok_or_else(|| anyhow!("session returned no outputs"))?.to_string();
    let value = outputs.remove(&name).ok_or_else(|| anyhow!("missing output {name:?}"))?;
    Ok((name, value))
}

fn read_wav_mono(path: &Path) -> Result<Vec<f32>> {
    let mut reader = hound::WavReader::open(path).with_context(|| format!("open {path:?}"))?;
    let spec = reader.spec();
    println!(
        "reference voice: {path:?} -> {} Hz, {} ch, {:?}, {} bits",
        spec.sample_rate, spec.channels, spec.sample_format, spec.bits_per_sample
    );
    let chans = spec.channels as usize;
    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().collect::<Result<_, _>>()?,
        hound::SampleFormat::Int => {
            let max = (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader.samples::<i32>().map(|s| s.map(|v| v as f32 / max)).collect::<Result<_, _>>()?
        }
    };
    // Downmix to mono. The reference uses librosa.load(..., sr=SAMPLE_RATE), i.e. mono.
    Ok(if chans <= 1 {
        interleaved
    } else {
        interleaved.chunks(chans).map(|c| c.iter().sum::<f32>() / chans as f32).collect()
    })
}

fn write_wav(path: &Path, samples: &[f32]) -> Result<usize> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(path, spec)?;
    for s in samples {
        w.write_sample((s.clamp(-1.0, 1.0) * 32767.0) as i16)?;
    }
    w.finalize()?;
    Ok(std::fs::metadata(path)?.len() as usize)
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(String::as_str).unwrap_or("synth");
    let dir = models_dir();
    match mode {
        "inspect" => {
            for name in ["speech_encoder", "embed_tokens", "language_model", "conditional_decoder"] {
                let path = graph(&dir, name);
                let session = Session::builder()?
                    .commit_from_file(&path)
                    .with_context(|| format!("load {path:?}"))?;
                describe(name, &session);
            }
            Ok(())
        }
        "synth" => synth(&dir, args.get(2).map(PathBuf::from)),
        other => Err(anyhow!("unknown mode {other:?} (use `inspect` or `synth`)")),
    }
}

// ── the pipeline ───────────────────────────────────────────────────────────────

fn synth(dir: &Path, out_arg: Option<PathBuf>) -> Result<()> {
    let out_path = out_arg.unwrap_or_else(|| dir.join("out_rust.wav"));

    // ── load ──────────────────────────────────────────────────────────────────
    let mut load_s = 0f64;
    let mut load = |name: &str| -> Result<Session> {
        let path = graph(dir, name);
        let t = Instant::now();
        let s = Session::builder()?.commit_from_file(&path).with_context(|| format!("load {path:?}"))?;
        load_s += t.elapsed().as_secs_f64();
        Ok(s)
    };
    let mut speech_encoder = load("speech_encoder")?;
    let mut embed_tokens = load("embed_tokens")?;
    let mut language_model = load("language_model")?;
    let mut cond_decoder = load("conditional_decoder")?;
    println!("load: {load_s:.3}s (4 graphs, q4f16)");

    // ── 1. reference voice ────────────────────────────────────────────────────
    let audio = read_wav_mono(&dir.join("default_voice.wav"))?;
    let n = audio.len();
    println!("voice: {n} samples ({:.2}s)", n as f32 / SAMPLE_RATE as f32);

    // ── 2. speech encoder ─────────────────────────────────────────────────────
    // Its four outputs are constants for the whole utterance, so run it once.
    let t = Instant::now();
    let enc_names: Vec<String> = speech_encoder.outputs().iter().map(|o| o.name().to_string()).collect();
    println!("speech_encoder output names: {enc_names:?}");
    let mut enc_run = speech_encoder.run(ort::inputs![
        "audio_values" => Tensor::from_array((vec![1i64, n as i64], audio))?
    ])?;
    let mut enc: Vec<DynValue> = Vec::new();
    // Take them by declared order: the script unpacks positionally as
    // (cond_emb, prompt_token, speaker_embeddings, speaker_features).
    for name in &enc_names {
        enc.push(enc_run.remove(name).ok_or_else(|| anyhow!("missing encoder output {name}"))?);
    }
    drop(enc_run);
    if enc.len() != 4 {
        return Err(anyhow!("speech_encoder returned {} outputs, expected 4", enc.len()));
    }
    for (i, v) in enc.iter().enumerate() {
        println!("  [{i}] {} {:?}", v.dtype().tensor_type().map(|t| t.to_string()).unwrap_or_default(), shape_of(v.dtype()));
    }
    let encoder_s = t.elapsed().as_secs_f64();
    let (cond_emb, prompt_token, speaker_embeddings, speaker_features) = (&enc[0], &enc[1], &enc[2], &enc[3]);

    // ── 3. tokenize ───────────────────────────────────────────────────────────
    let tok_path = dir.join("tokenizer.json");
    let tokenizer = tokenizers::Tokenizer::from_file(&tok_path).map_err(|e| anyhow!("{tok_path:?}: {e}"))?;
    let encoding = tokenizer.encode(TEXT, false).map_err(|e| anyhow!("encode: {e}"))?;
    let text_ids: Vec<i64> = encoding.get_ids().iter().map(|&i| i as i64).collect();
    let t_len = text_ids.len();
    println!("text {TEXT:?}\n  -> {t_len} ids {text_ids:?}");

    // ── 4. embed_tokens ───────────────────────────────────────────────────────
    let mut embeds_run = embed_tokens.run(ort::inputs![
        "input_ids" => Tensor::from_array((vec![1i64, t_len as i64], text_ids))?
    ])?;
    let (emb_name, emb_value) = sole_output(embeds_run)?;
    if emb_name == "" {
        return Err(anyhow!("embed_tokens output had no name"));
    }
    let emb = as_f32(&emb_value)?;
    let emb_dim = emb.len() / t_len;
    println!("embed_tokens -> {emb_dim}-dim");

    // i == 0 only: inputs_embeds = cat([cond_emb, inputs_embeds], dim=1)
    let cond = as_f32(cond_emb)?;
    let cond_len = cond.len() / emb_dim;
    println!("cond_emb -> {cond_len} frames ({} floats)", cond.len());
    let mut inputs_embeds: Vec<f32> = Vec::with_capacity((cond_len + t_len) * emb_dim);
    inputs_embeds.extend_from_slice(&cond);
    inputs_embeds.extend_from_slice(&emb);

    // ── KV cache: read the real names and dtypes off the session ─────────────
    let kv_inputs: Vec<(String, bool)> = language_model
        .inputs()
        .iter()
        .filter(|i| i.name().contains("past_key_values"))
        .map(|i| (i.name().to_string(), is_f16(i.dtype())))
        .collect();
    let kv_f16 = kv_inputs.first().map(|k| k.1).unwrap_or(false);
    println!("kv inputs: {} (f16 = {kv_f16}), lm outputs: {}", kv_inputs.len(), language_model.outputs().len());
    if kv_inputs.len() != 48 {
        println!("WARNING: expected 48 kv inputs (24 layers x 2), got {}", kv_inputs.len());
    }
    let mut past: Vec<DynValue> = kv_inputs
        .iter()
        .map(|(_, f16)| zeros_typed(vec![1, NUM_KV_HEADS as i64, 0, HEAD_DIM as i64], *f16))
        .collect::<Result<_>>()?;

    let mut seq_len = cond_len + t_len;
    let mut attention_mask: Vec<i64> = vec![1; seq_len];
    let mut position_ids: Vec<i64> = (0..seq_len as i64).collect();
    let mut generated: Vec<i64> = vec![START_SPEECH_TOKEN];
    let mut speech_tokens: Vec<i64> = Vec::new();

    // ── 5-8. generation loop ──────────────────────────────────────────────────
    let mut embed_again_s = 0f64;
    let t_loop = Instant::now();
    let mut step = 0usize;
    while step < MAX_NEW_TOKENS {
        // The three inputs do NOT share a sequence length. On the prefill: embeds are
        // cond+text frames, mask is the same length, positions are 0..n. After that the
        // embeds are ONE frame, the mask keeps growing, and position_ids is
        // `position_ids[:, -1:] + 1` — a single position.
        let emb_frames = position_ids.len();
        let mask_len = seq_len as i64;
        let embeds_in = Tensor::from_array((
            vec![1i64, emb_frames as i64, emb_dim as i64],
            std::mem::take(&mut inputs_embeds),
        ))?;

        let mut named: Vec<(Cow<'_, str>, SessionInputValue<'_>)> = Vec::with_capacity(3 + kv_inputs.len());
        named.push((Cow::Borrowed("inputs_embeds"), embeds_in.into()));
        named.push((
            Cow::Borrowed("attention_mask"),
            Tensor::from_array((vec![1i64, mask_len], attention_mask.clone()))?.into(),
        ));
        named.push((
            Cow::Borrowed("position_ids"),
            Tensor::from_array((vec![1i64, position_ids.len() as i64], position_ids.clone()))?.into(),
        ));
        for ((name, _), value) in kv_inputs.iter().zip(past.drain(..)) {
            named.push((Cow::Borrowed(name.as_str()), value.into()));
        }

        let outputs = language_model.run(named)?;
        let mut values: Vec<DynValue> = outputs.into_iter().map(|(_, v)| v).collect();
        if values.len() != kv_inputs.len() + 1 {
            return Err(anyhow!("language_model gave {} outputs for {} kv inputs", values.len(), kv_inputs.len()));
        }
        // Declared graph order: logits first, then the present-KV tensors, positionally
        // matching the past_key_values.* inputs (this is what `logits, *present = run()`
        // does in the reference script).
        let present = values.split_off(1);
        let logits = values.pop().unwrap();
        past = present;

        // repetition penalty over every token generated so far, then argmax.
        // The graph returns logits for EVERY position, not just the last one:
        // `logits[:, -1, :]` in the reference script.
        let shape = logits.dtype().tensor_shape().map(|s| s.to_vec()).unwrap_or_default();
        let all = as_f32(&logits)?;
        let vocab = *shape.last().unwrap_or(&(all.len() as i64)) as usize;
        if all.len() < vocab {
            return Err(anyhow!("logits hold {} floats for a {vocab}-token vocab", all.len()));
        }
        let mut row = all[all.len() - vocab..].to_vec();
        for &g in &generated {
            let i = g as usize;
            if i < vocab {
                row[i] = if row[i] < 0.0 { row[i] * REPETITION_PENALTY } else { row[i] / REPETITION_PENALTY };
            }
        }
        let mut best = 0usize;
        for (i, v) in row.iter().enumerate() {
            if *v > row[best] {
                best = i;
            }
        }
        let next = best as i64;
        generated.push(next);
        if next == STOP_SPEECH_TOKEN {
            break;
        }
        speech_tokens.push(next);

        // feed the single new token back through embed_tokens
        let t = Instant::now();
        let mut e = embed_tokens.run(ort::inputs![
            "input_ids" => Tensor::from_array((vec![1i64, 1i64], vec![next]))?
        ])?;
        let (_, v) = sole_output(e)?;
        inputs_embeds = as_f32(&v)?;
        embed_again_s += t.elapsed().as_secs_f64();

        seq_len += 1;
        attention_mask.push(1);
        // `position_ids[:, -1:] + 1`: from here on positions are a single value.
        let last = position_ids[position_ids.len() - 1];
        position_ids = vec![last + 1];
        step += 1;
    }
    let loop_s = t_loop.elapsed().as_secs_f64();
    println!(
        "loop: {step} steps, {} speech tokens, {loop_s:.3}s ({:.1} ms/step); {embed_again_s:.3}s of that is re-embedding",
        speech_tokens.len(),
        loop_s * 1000.0 / step.max(1) as f64
    );
    if step >= MAX_NEW_TOKENS {
        println!("NOTE: stopped at max_new_tokens={MAX_NEW_TOKENS} without a stop token");
    }

    // ── 9. decoder input ──────────────────────────────────────────────────────
    let mut decode_ids = as_i64(prompt_token)?;
    println!("prompt_token: {} ids, first few {:?}", decode_ids.len(), &decode_ids[..decode_ids.len().min(8)]);
    decode_ids.extend_from_slice(&speech_tokens);
    decode_ids.extend_from_slice(&[SILENCE_TOKEN; 3]); // reference adds 3 trailing silence tokens
    let decode_len = decode_ids.len();

    // ── 10. conditional decoder ───────────────────────────────────────────────
    let t = Instant::now();
    let mut wav = cond_decoder.run(ort::inputs![
        "speech_tokens" => Tensor::from_array((vec![1i64, decode_len as i64], decode_ids))?,
        "speaker_embeddings" => speaker_embeddings.view(),
        "speaker_features" => speaker_features.view(),
    ])?;
    let (_, wav_value) = sole_output(wav)?;
    let samples = as_f32(&wav_value)?;
    let decoder_s = t.elapsed().as_secs_f64();

    let audio_s = samples.len() as f64 / SAMPLE_RATE as f64;
    let peak = samples.iter().fold(0f32, |a, b| a.max(b.abs()));
    let rms = (samples.iter().map(|s| (*s as f64) * (*s as f64)).sum::<f64>() / samples.len().max(1) as f64).sqrt();
    let nonfinite = samples.iter().filter(|s| !s.is_finite()).count();
    let bytes = write_wav(&out_path, &samples)?;

    println!("--- timings ---");
    println!("load          {load_s:.3}s");
    println!("encoder       {encoder_s:.3}s");
    println!("lm loop       {loop_s:.3}s");
    println!("decoder       {decoder_s:.3}s");
    println!("--- audio ---");
    println!("samples       {}", samples.len());
    println!("duration      {audio_s:.3}s @ {SAMPLE_RATE} Hz mono");
    println!("peak          {peak:.6}");
    println!("rms           {rms:.6}");
    println!("non-finite    {nonfinite}");
    println!("wrote         {out_path:?} ({bytes} bytes)");
    Ok(())
}