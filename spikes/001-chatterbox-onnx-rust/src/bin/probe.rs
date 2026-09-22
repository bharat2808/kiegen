//! Isolation probe: exercise each graph on its own with controlled inputs so a
//! failure can be attributed to one graph instead of the whole chain.

use anyhow::{anyhow, Context, Result};
use ort::session::{Session, SessionInputValue};
use ort::value::{DynValue, Tensor};

fn graph(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
    dir.join("onnx").join(format!("{name}_q4f16.onnx"))
}

fn main() -> Result<()> {
    let dir = std::env::var("CHATTERBOX_MODEL_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("/Users/home/.hermes/cache/scratch/chatterbox_onnx"));

    // ── embed_tokens ─────────────────────────────────────────────────────────
    let mut emb = Session::builder()?.commit_from_file(graph(&dir, "embed_tokens"))?;
    for ids in [vec![464i64, 2068, 7586, 21831, 18045, 625, 262, 16931, 3290, 13], vec![464], vec![6561], vec![6562]] {
        let l = ids.len() as i64;
        match emb.run(ort::inputs!["input_ids" => Tensor::from_array((vec![1i64, l], ids.clone()))?]) {
            Ok(o) => {
                let (name, v) = {
                    let n = o.keys().next().unwrap().to_string();
                    let mut o = o;
                    let v = o.remove(&n).unwrap();
                    (n, v)
                };
                println!("embed_tokens {ids:?} -> {name} {:?} {:?}", v.dtype().tensor_type(), v.dtype().tensor_shape().map(|s| s.to_vec()).unwrap_or_default());
            }
            Err(e) => println!("embed_tokens {ids:?} -> ERROR {e}"),
        }
    }

    // ── language_model, zeros embeds, empty kv ───────────────────────────────
    let mut lm = Session::builder()?.commit_from_file(graph(&dir, "language_model"))?;
    let kv: Vec<(String, bool)> = lm
        .inputs()
        .iter()
        .filter(|i| i.name().contains("past_key_values"))
        .map(|i| (i.name().to_string(), matches!(i.dtype().tensor_type(), Some(ort::value::TensorElementType::Float16))))
        .collect();

    for (label, seq, n_past) in [("zeros/prefill seq=161", 161i64, 0i64), ("zeros/prefill seq=4", 4, 0)] {
        let embeds = Tensor::from_array((vec![1i64, seq, 1024i64], vec![0f32; (seq * 1024) as usize]))?;
        let mask = Tensor::from_array((vec![1i64, seq], vec![1i64; seq as usize]))?;
        let pos = Tensor::from_array((vec![1i64, seq], (0..seq).collect::<Vec<i64>>()))?;
        let mut named: Vec<(std::borrow::Cow<'_, str>, SessionInputValue<'_>)> = vec![
            ("inputs_embeds".into(), embeds.into()),
            ("attention_mask".into(), mask.into()),
            ("position_ids".into(), pos.into()),
        ];
        for (name, f16) in &kv {
            let n = (n_past * 16 * 64) as usize;
            let v: DynValue = if *f16 {
                Tensor::from_array((vec![1i64, 16, n_past, 64], vec![half::f16::ZERO; n]))?.upcast().into_dyn()
            } else {
                Tensor::from_array((vec![1i64, 16, n_past, 64], vec![0f32; n]))?.upcast().into_dyn()
            };
            named.push((std::borrow::Cow::Borrowed(name.as_str()), v.into()));
        }
        match lm.run(named) {
            Ok(o) => {
                let keys: Vec<String> = o.keys().map(str::to_string).take(3).collect();
                println!("language_model {label} -> OK, first outputs {keys:?}, total {}", o.len());
            }
            Err(e) => println!("language_model {label} -> ERROR {e}"),
        }
    }

    // ── language_model: one decode step with real cached kv ──────────────────
    // Prefill with zeros, then decode one step using the returned present tensors,
    // exactly as the generation loop does.
    {
        let seq = 4i64;
        let embeds = Tensor::from_array((vec![1i64, seq, 1024i64], vec![0f32; (seq * 1024) as usize]))?;
        let mut named: Vec<(std::borrow::Cow<'_, str>, SessionInputValue<'_>)> = vec![
            ("inputs_embeds".into(), embeds.into()),
            ("attention_mask".into(), Tensor::from_array((vec![1i64, seq], vec![1i64; seq as usize]))?.into()),
            ("position_ids".into(), Tensor::from_array((vec![1i64, seq], (0..seq).collect::<Vec<i64>>()))?.into()),
        ];
        for (name, f16) in &kv {
            let v: DynValue = if *f16 {
                Tensor::from_array((vec![1i64, 16, 0i64, 64], vec![half::f16::ZERO; 0]))?.upcast().into_dyn()
            } else {
                Tensor::from_array((vec![1i64, 16, 0i64, 64], vec![0f32; 0]))?.upcast().into_dyn()
            };
            named.push((std::borrow::Cow::Borrowed(name.as_str()), v.into()));
        }
        let outs = lm.run(named).context("prefill")?;
        let mut values: Vec<DynValue> = outs.into_iter().map(|(_, v)| v).collect();
        let present = values.split_off(1);
        let logits = values.pop().unwrap();
        println!("prefill logits shape {:?}", logits.dtype().tensor_shape().map(|s| s.to_vec()).unwrap_or_default());
        println!("present[0] shape {:?}", present[0].dtype().tensor_shape().map(|s| s.to_vec()).unwrap_or_default());

        // next step: one new embed, kv from the prefill
        let e = Tensor::from_array((vec![1i64, 1i64, 1024i64], vec![0f32; 1024]))?;
        let mut named: Vec<(std::borrow::Cow<'_, str>, SessionInputValue<'_>)> = vec![
            ("inputs_embeds".into(), e.into()),
            ("attention_mask".into(), Tensor::from_array((vec![1i64, seq + 1], vec![1i64; seq as usize + 1]))?.into()),
            ("position_ids".into(), Tensor::from_array((vec![1i64, 1i64], vec![seq]))?.into()),
        ];
        for ((name, _), value) in kv.iter().zip(present.into_iter()) {
            named.push((std::borrow::Cow::Borrowed(name.as_str()), value.into()));
        }
        match lm.run(named) {
            Ok(o) => println!("decode step 2 -> OK ({} outputs)", o.len()),
            Err(e) => println!("decode step 2 -> ERROR {e}"),
        }
    }

    // ── speech_encoder + conditional_decoder end to end ──────────────────────
    {
        let mut se = Session::builder()?.commit_from_file(graph(&dir, "speech_encoder"))?;
        let wav = dir.join("default_voice.wav");
        let mut reader = hound::WavReader::open(&wav)?;
        let spec = reader.spec();
        let audio: Vec<f32> = reader.samples::<f32>().collect::<Result<_, _>>()?;
        let n = audio.len() as i64;
        println!("voice {n} samples, {spec:?}");
        let names: Vec<String> = se.outputs().iter().map(|o| o.name().to_string()).collect();
        let mut run = match se.run(ort::inputs![
            "audio_values" => Tensor::from_array((vec![1i64, n], audio))?
        ]) {
            Ok(o) => o,
            Err(e) => return Err(anyhow!("speech_encoder ERROR {e}")),
        };
        let mut vals = Vec::new();
        for nm in &names {
            vals.push(run.remove(nm).unwrap());
        }
        println!("speech_encoder OK: {names:?}");
        let (_, prompt_token) = (0, &vals[1]);
        let (_, prompt) = prompt_token.try_extract_tensor::<i64>()?;
        println!("prompt_token len {}, min {}, max {}", prompt.len(), prompt.iter().min().unwrap(), prompt.iter().max().unwrap());

        let mut cd = Session::builder()?.commit_from_file(graph(&dir, "conditional_decoder"))?;
        // Try the decoder with a token range that stays inside the vocab.
        for (label, ids) in [
            ("real prompt_token", prompt.to_vec()),
            ("arange 0..200", (0..200i64).collect()),
        ] {
            let l = ids.len() as i64;
            let res = cd.run(ort::inputs![
                "speech_tokens" => Tensor::from_array((vec![1i64, l], ids))?,
                "speaker_embeddings" => vals[2].view(),
                "speaker_features" => vals[3].view(),
            ]);
            match res {
                Ok(o) => {
                    let mut o = o;
                    let v = o.remove("waveform").unwrap();
                    let (_, d) = v.try_extract_tensor::<f32>()?;
                    let peak = d.iter().fold(0f32, |a, b| a.max(b.abs()));
                    println!("conditional_decoder {label} -> OK {} samples, peak {peak}", d.len());
                }
                Err(e) => println!("conditional_decoder {label} -> ERROR {e}"),
            }
        }
    }

    Ok(())
}