---
language: en
license: mit
library_name: onnxruntime
pipeline_tag: audio-to-audio
tags:
  - onnx
  - onnxruntime
  - stem-separation
  - source-separation
  - vocal-remover
  - karaoke
  - acapella
  - demucs
  - htdemucs
  - music
  - audio-to-audio
  - mobile
  - ios
  - android
  - coreml
  - directml
  - production-ready
datasets:
  - StemSplitio/stem-separation-benchmark-2026
inference: false
---

# HT-Demucs (single-file 4-stem) — ONNX

The **first ONNX export of the standard `htdemucs` (non-FT) model** on
the Hugging Face Hub. Runs in `onnxruntime` on CPU out of the box, and
on CoreML / CUDA / DirectML with a one-line provider change.
**No PyTorch required at inference.**

This repo is the single-file companion to
[`StemSplitio/htdemucs-ft-onnx`](https://huggingface.co/StemSplitio/htdemucs-ft-onnx).
You get all 4 stems out of one 316 MB `.onnx` file (`htdemucs.onnx`),
or 166 MB if you grab the fp16weights variant. The FT bag is higher
quality; this single model is ~30% faster and uses 1 session instead of 4.

---

## TL;DR

```bash
# 316 MB fp32 model:
pip install onnxruntime numpy soundfile
python infer.py your-song.mp3 ./out/ --write-all-stems
# writes ./out/{drums,bass,other,vocals}.wav at 44.1 kHz stereo

# 166 MB fp16weights variant (same runtime cost):
python infer.py your-song.mp3 ./out/ --small --write-all-stems
```

The repo contains:

- `htdemucs.onnx` — 316 MB, opset 17, parity-verified vs PyTorch fp32.
- `htdemucs_fp16weights.onnx` — 166 MB, fp16-stored weights, same runtime memory / latency.
- `infer.py` — pure-numpy reference inference (~200 lines, no torch).
- `requirements.txt` — three small packages, no PyTorch.

---

## Quality

The official `htdemucs` model is the precursor to `htdemucs_ft` — same
architecture, single set of weights instead of 4 specialist sub-models.
On MUSDB18-HQ:

| Metric | `htdemucs` (this) | `htdemucs_ft` (4-bag) |
|---|---:|---:|
| Median vocals SDR | ~8.8 dB | **9.19 dB** |
| Median drums SDR  | ~9.5 dB | **10.11 dB** |
| Total model size  | **316 MB** | 1.26 GB |
| Sessions to load  | **1**     | 4 |
| Speed vs the bag  | **~1.4× faster** | baseline |

Parity vs PyTorch fp32 (random input, 7.8 s segment):

- `htdemucs.onnx` max abs diff: **6.62 × 10⁻⁴**
- `htdemucs_fp16weights.onnx` max abs diff (vs fp32 weights): **4.6 × 10⁻⁵**

Both well within the 1e-3 publish threshold.

---

## Performance

Single 7.8 s segment, Apple M4 Pro CPU:

| Variant | RAM | Latency | RTF |
|---|---:|---:|---:|
| `htdemucs.onnx` (fp32) | ~1.1 GB | ~1.6 s | 0.20 |
| `htdemucs_fp16weights.onnx` | ~1.1 GB | ~1.6 s | 0.20 |
| For comparison: `htdemucs_ft` (4-session bag) | ~4.0 GB | ~6.4 s | 0.49 |

CUDA / DirectML / CoreML EPs are typically ≥ 5× faster on real GPUs.

---

## Quick start

### Python

```python
import soundfile as sf
import infer

audio, sr = sf.read("your-song.mp3", dtype="float32", always_2d=True)
stems = infer.separate(audio.T, sr,
                       model_path=infer.DEFAULT_MODEL,
                       providers=["CPUExecutionProvider"])
for stem, arr in stems.items():
    sf.write(f"{stem}.wav", arr.T, sr)
```

### CLI

```bash
python infer.py your-song.mp3 ./out/ --write-all-stems
python infer.py your-song.mp3 ./out/ --providers coreml   # macOS arm64
python infer.py your-song.mp3 ./out/ --providers cuda     # Linux + NVIDIA
python infer.py your-song.mp3 ./out/ --providers dml      # Windows + DX12
python infer.py your-song.mp3 ./out/ --small              # 166 MB variant
```

### Mobile / Web (after pip install `onnxruntime-mobile` or `onnxruntime-web`)

```swift
// iOS / Swift
import onnxruntime_objc
let opts = try ORTSessionOptions()
try opts.appendCoreMLExecutionProvider(with: ORTCoreMLExecutionProviderOptions())
let session = try ORTSession(env: env,
    modelPath: Bundle.main.path(forResource: "htdemucs", ofType: "onnx")!,
    sessionOptions: opts)
```

```js
// Browser / web
import * as ort from "onnxruntime-web";
const sess = await ort.InferenceSession.create("htdemucs_fp16weights.onnx", {
  executionProviders: ["wasm"],
});
const t = new ort.Tensor("float32", audioBuffer, [1, 2, 343980]);
const out = await sess.run({ mix: t });   // out.stems is (1, 4, 2, 343980)
```

For a turnkey browser demo with file-picker + chunked overlap-add, see
[`demucs-onnx browser-demo`](https://github.com/StemSplit/demucs-onnx#browser-demos).

---

## Input / output spec

| Tensor | Name | Shape | Dtype | Notes |
|---|---|---|---|---|
| Input | `mix` | `(1, 2, 343980)` | float32 | Stereo, 44.1 kHz, 7.8 s segment. Values in [-1, 1]. |
| Output | `stems` | `(1, 4, 2, 343980)` | float32 | Stems in order `[drums, bass, other, vocals]`. All 4 are real predictions (unlike the FT specialists). |

For longer audio, chunk with overlap-add — see `infer.py::separate` for
a working 60-line implementation.

---

## Tooling — `demucs-onnx` Python package

This model can be run (and re-exported from PyTorch) via the open-source
[`demucs-onnx`](https://github.com/StemSplit/demucs-onnx) Python package
on PyPI. It auto-downloads from this repo on first use, so you don't
have to clone or wrangle file paths.

```bash
pip install demucs-onnx

# Single-file 4-stem flavor (this repo):
demucs-onnx separate song.mp3 stems/ --model htdemucs

# Python API:
python -c "from demucs_onnx import separate; \
  print(separate('song.mp3', model='htdemucs').keys())"
```

To re-export your own fine-tune:

```bash
pip install 'demucs-onnx[export]'
demucs-onnx export htdemucs out/htdemucs.onnx
```

---

## How it was built

The export pipeline lives in the open-source
[`demucs-onnx`](https://github.com/StemSplit/demucs-onnx) package at
[`demucs_onnx/export/`](https://github.com/StemSplit/demucs-onnx/tree/main/src/demucs_onnx/export).
It applies four patches to make `torch.onnx.export` work on htdemucs:

1. Complex-typed `torch.stft` outputs → `Conv1d` with sin/cos kernels.
2. `model.segment` `fractions.Fraction` → plain `float`.
3. `random.randrange` in transformer pos-embedding → hardcoded `shift=0`.
4. `aten::_native_multi_head_attention` (no ONNX symbolic) → drop-in
   `nn.MultiheadAttention.forward` built from `Linear`/`bmm`/`softmax`.

These are the four blockers every previous community attempt at "demucs
onnx" stalled on. See the [README of the demucs-onnx package](https://github.com/StemSplit/demucs-onnx#the-4-blockers-explained)
for the full write-up with code references.

---

## Related work

Sibling ONNX repos from the same export pipeline:

| Repo | Format | Stems | Use when |
|---|---|---|---|
| `htdemucs-onnx` *(this)* | Single file | 4 | Faster startup, fewer sessions, ~30% lower latency than the FT bag. |
| [`htdemucs-ft-onnx`](https://huggingface.co/StemSplitio/htdemucs-ft-onnx) | Bag of 4 files | 4 | Best SDR, especially on vocals. The default in StemSplit production. |
| [`htdemucs-6s-onnx`](https://huggingface.co/StemSplitio/htdemucs-6s-onnx) | Single file | 6 | Need guitar + piano stems on top of the standard 4. |
| [`htdemucs-ft-{drums,bass,other,vocals}-onnx`](https://huggingface.co/StemSplitio) | Single specialist | 1 | Fastest single-stem inference; 4× faster than the bag. |

Full benchmark across every popular open-source separator:
[StemSplitio/stem-separation-benchmark-2026](https://huggingface.co/datasets/StemSplitio/stem-separation-benchmark-2026).

---

## Skip the infrastructure — use the StemSplit API

Don't want to bundle a 316 MB model in your app, manage a GPU pool, or
write overlap-add chunking? Use the **[StemSplit API](https://stemsplit.io/developers)**
instead — same model under the hood, hosted for you, with credits and a
dashboard.

- 🌐 [stemsplit.io](https://stemsplit.io)
- 📘 [Developer docs](https://stemsplit.io/developers/docs)
- 🔌 [API reference](https://stemsplit.io/developers/reference)

Or use the no-code tools that ship the same model family:

- 🎤 [Vocal Remover](https://stemsplit.io/vocal-remover)
- 🎶 [Karaoke Maker](https://stemsplit.io/karaoke-maker)
- 🎙️ [Acapella Maker](https://stemsplit.io/acapella-maker)
- 📺 [YouTube Stem Splitter](https://stemsplit.io/youtube-stem-splitter)

---

## License & attribution

This repo is **MIT-licensed**, matching the original HT-Demucs.

```bibtex
@inproceedings{rouard2023hybrid,
  title     = {Hybrid Transformers for Music Source Separation},
  author    = {Rouard, Simon and Massa, Francisco and D{\'e}fossez, Alexandre},
  booktitle = {ICASSP},
  year      = {2023}
}
```

- Original PyTorch model: [`facebookresearch/demucs`](https://github.com/facebookresearch/demucs)
- ONNX export, parity verification, and packaging by [StemSplit](https://stemsplit.io)
- Search keywords: **htdemucs onnx**, **demucs onnx single file**, **demucs ios**,
  **demucs android**, **music source separation onnx**, **stem separation mobile**.
