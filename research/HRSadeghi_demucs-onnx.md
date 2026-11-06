---
license: mit
library_name: onnx
tags:
  - audio
  - audio-to-audio
  - source-separation
  - music
  - demucs
  - onnx
  - onnxruntime
pipeline_tag: audio-to-audio
---

# demucs-onnx — exported graphs

[Demucs](https://github.com/adefossez/demucs) music source separation, exported to
ONNX so it can run without PyTorch. These are the weights for
[demucs-onnx](https://github.com/HRSadeghi/demucs-onnx), which runs them from
Python, C++, Rust, Java, C# and JavaScript.

Try them in the browser: [the demucs-onnx Space](https://huggingface.co/spaces/HRSadeghi/demucs-onnx).

## What is in here

Each model is a pair — `<name>.onnx`, the graph, and `<name>.json`, a sidecar naming
its stems, sample rate and STFT parameters. A *bag* (`htdemucs_ft`) is a manifest
listing four member graphs and the per-stem weights they are blended with.

| file | stems | notes |
|---|---|---|
| `htdemucs` | drums, bass, other, vocals | the v4 default |
| `htdemucs_fp16` | same | half-precision weights |
| `htdemucs_int8` | same | quantised; much faster on a CPU |
| `htdemucs_6s` | + guitar, piano | six-stem model |
| `htdemucs_ft` | drums, bass, other, vocals | bag of four fine-tuned networks, best quality |
| `hdemucs_mmi` | drums, bass, other, vocals | the v3 hybrid model |

## Using them

```bash
pip install "git+https://github.com/HRSadeghi/demucs-onnx.git#subdirectory=python"
hf download HRSadeghi/demucs-onnx htdemucs.onnx htdemucs.json --local-dir models
demucs-onnx song.wav -m models/htdemucs -o separated
```

The graph is cut at the complex boundary: the STFT, the ISTFT and the overlap-add
happen in host code, and only the conv encoder / transformer / conv decoder is in
the ONNX graph. The project README explains why, and every language binding in the
repository reads exactly these two files.

## Licence

The runtime code is MIT. The **weights are Meta's**, and carry the terms of the
[Demucs licence](https://github.com/adefossez/demucs/blob/main/LICENSE) —
non-commercial (CC BY-NC 4.0) for the v3/v4 checkpoints. Check that before you put
this in a product.
