# Spike 001 — Chatterbox as pure-Rust ONNX (`ort`), no Python

**Status: throwaway. Delete once the port lands.**

## The question

kiegen's Chatterbox engine currently needs a ~3 GB Python/MLX sidecar. Can the same model
run as ONNX graphs chained in Rust with the `ort` crate — which is already in the dependency
tree for Kokoro — with comparable audio and acceptable speed?

## What was built

A standalone crate (not wired into the app) that chains four ONNX graphs from
`ResembleAI/chatterbox-turbo-ONNX` at q4f16 (558 MB total weights, MIT):

```
speech_encoder(audio_values) -> cond_emb, prompt_token, speaker_embeddings, speaker_features
embed_tokens(input_ids)     -> inputs_embeds
language_model(embeds, mask, position_ids, past_kv...) -> logits, present_kv...   (30 layers, 16 KV heads, dim 64)
  loop: repetition penalty 1.2 -> argmax -> stop at token 6562, kv cache carried forward
conditional_decoder(speech_tokens, speaker_embeddings, speaker_features) -> 24 kHz wav
```

The reference voice is the repo's own `default_voice.wav`, loaded at 24 kHz.

## Results (uncontended machine, dev profile with `opt-level = 3`)

| | value |
|---|---|
| Model load | 5.93 s (8.08 s on the first-ever run, cold page cache) |
| Speech tokens generated | 78 steps for 3.240 s of audio |
| Autoregressive loop | 1.79–1.88 s (**23–24 ms/step**) |
| Encode + prefill + decode + write | 1.95–2.08 s |
| **Total synthesis** | **~3.85 s** |
| **RTF** | **~1.19** (about 0.84x realtime) |
| Audio | 3.240 s, 24 kHz mono, peak 1.05, RMS 0.11–0.13, 94.8% non-zero samples |
| Weights on disk | **558 MB** |

Head-to-head against the MLX/Python path on the same sentence and the same reference voice:

| Dimension | Rust `ort` (this spike) | MLX Python (incumbent) |
|---|---|---|
| Weights | **558 MB** | 3,069 MB (2.574 GB + 495 MB) |
| Runtime | `ort` + `tokenizers` — already in the tree | Python 3.12 + mlx + mlx-metal + mlx-audio |
| Load | 5.93 s | 4.21 s |
| Synthesis | ~3.85 s | 11.59 s *(contended)* |
| Audio length | 3.240 s | 3.160 s |
| **RTF** | **~1.19** | 3.67 *(contended)*; 2.81 on its first call |
| Peak / RMS | 1.05 / 0.11 | 0.99 / 0.12 |
| Non-zero samples | 94.8% | 90.8% |

The MLX numbers were all measured while this machine was concurrently downloading and
compiling, and its own repeat swung to RTF 7.53 — so 3.67 is a ceiling, not a floor, and the
speed comparison should be re-run on a quiet machine before anyone quotes it. The 558 MB vs
3,069 MB weight difference does not depend on that re-measurement.

## What worked

- All four graphs load and run under `ort` on CPU, including the quantized (MatMulNBits) LM.
- The kv-cache generation loop drives cleanly: **f16** cache tensors, 30 layers x 2, fed back
  as `present_*` after `logits` in declared input order.
- Output audio is real speech at the right length and level, closely matching the MLX
  baseline's duration and RMS. Both WAVs were listened to back to back.
- Speed: 23.8 ms per frame, 78 frames, comfortably ahead of the Python path.

## What didn't

- **The end-to-end RTF is 1.19, not the 0.57 the loop timing suggests.** Roughly half the
  synthesis time is spent *outside* the generation loop — the `speech_encoder` and
  `conditional_decoder` passes — which is the direct cost of those two graphs existing only
  as fp32 (592 MB and 534 MB) while only the LM is quantized. Quantized encoder/decoder
  variants are the obvious next lever, and they do not exist upstream for the multilingual
  export.
- **Two real bugs, both found by running it rather than reading it:**
  1. `language_model` returns logits shaped `[1, seq, vocab]`, not `[1, 1, vocab]`; slicing
     the wrong axis produced an out-of-range token (800497) instead of a crash.
  2. After prefill, `inputs_embeds` is a single frame `[1, 1, 1024]` while the attention mask
     keeps growing; tagging it with the full sequence length emits garbage.
- No README/verdict was written by the first attempt — that process was killed mid-report. The
  code, the built binary and the WAV survived; the numbers above were re-measured afterwards.

## Surprises

- The `speech_encoder` output names differ from the community card's prose: the real outputs
  are `audio_features`, `audio_tokens`, `speaker_embeddings`, `speaker_features`.
- `default_voice.wav` from the *English* community repo works as the reference voice for this
  pipeline — the graphs are not picky about which repo the prompt came from.
- The 1-step decoder in Turbo is genuinely cheap; that is what makes the fp32 encoder stand out
  as the bottleneck.

## Verdict: VALIDATED

Chatterbox runs as pure Rust ONNX with no Python, no MLX and no sidecar, at ~1.2x realtime on
CPU, with audio that matches the MLX path in length and level and needs only 558 MB of weights
instead of 3,069 MB. The architecture is proven: four `ort` sessions, a hand-written kv-cache
loop, `tokenizers` for the text side.

### Recommendation for the real build

1. Port it into `src-tauri` as the Chatterbox engine, replacing the MLX sidecar. `ort` and the
   pattern are already there from Kokoro; the new parts are the generation loop, the kv-cache
   plumbing and the reference-audio encode.
2. **Do not promise RTF < 1.** Budget ~1.2x realtime single-stream on CPU, and treat the
   encoder/decoder passes as the optimisation target: re-export them quantized, or run the
   decoder on the Neural Engine/GPU.
3. Expect the multilingual model to be slower than this: 500M parameters instead of 350M, and
   a multi-step decoder instead of Turbo's 1-step. The measured 1.19 RTF is a *floor* for the
   multilingual port, not an estimate of it.
4. Beware the `.onnx` / `.onnx_data` pair: the graph file is useless without its external
   weights sidecar, and `Entry::fetch`'s `with_extension("part")` temp naming collides between
   `X.onnx` and `X.onnx_data` if fetches are ever parallelised.
5. Keep the PerTh watermark question open — the Python path applies it, this spike does not.
