# Reference Spleeter 4-stem separation (whole clip at once), per the model card.
import time, resource, numpy as np, onnxruntime as ort
M = '/home/jaspermeller/.cache/slipmat/models/'
R = '/home/jaspermeller/slipmat/research/'
x = np.fromfile(R + 'summer.f32', dtype=np.float32).reshape(-1, 2).T.astype(np.float64)
N, H, T, F, BINS = 4096, 1024, 512, 1536, 2049
PAD = N - H
W = np.hanning(N + 1)[:-1]
def stft(v):
    n = int(np.ceil((PAD + len(v)) / H)); p = np.zeros((n - 1) * H + N); p[PAD:PAD + len(v)] = v
    idx = np.arange(N)[None, :] + H * np.arange(n)[:, None]
    return np.fft.rfft(p[idx] * W, axis=-1)
def istft(spec, length):
    frames = np.fft.irfft(spec, n=N, axis=-1); total = (len(spec) - 1) * H + N
    out, ws = np.zeros(total), np.zeros(total)
    for i in range(len(spec)):
        out[i*H:i*H+N] += frames[i] * W; ws[i*H:i*H+N] += W ** 2
    out = np.divide(out, ws, out=np.zeros_like(out), where=ws > 1e-8)
    return out[PAD:PAD + length]
t0 = time.time()
n = x.shape[1]
spec = np.stack([stft(x[c]) for c in range(2)]); frames = spec.shape[1]; splits = int(np.ceil(frames / T))
mag = np.zeros((2, splits * T, F), np.float32); mag[:, :frames] = np.abs(spec[:, :, :F]); net = mag.reshape(2, splits, T, F)
est = {}
for s in ('vocals', 'drums', 'bass', 'other'):
    sess = ort.InferenceSession(M + f'spleeter-{s}.wide.fp16.onnx', providers=['CPUExecutionProvider'])
    est[s] = sess.run(['y'], {'x': net})[0].reshape(2, -1, F)[:, :frames].astype(np.float64)
total = sum(e ** 2 for e in est.values()) + 1e-10
out = {}
for s, e in est.items():
    m = (e ** 2 + 1e-10 / 4) / total
    ext = np.repeat(m.mean(axis=2, keepdims=True), BINS - F, axis=2)
    m = np.concatenate([m, ext], axis=2)
    out[s] = np.stack([istft(spec[c] * m[c], n) for c in range(2)])
print(f'separated {n/44100:.1f}s in {time.time()-t0:.2f}s, peak RSS {resource.getrusage(resource.RUSAGE_SELF).ru_maxrss//1024} MB')
recon = sum(out.values())
print('reconstruction error dB', 20*np.log10(np.abs(recon - x).max() / np.abs(x).max()))
for s, v in out.items():
    print(f'{s:7s} rms {20*np.log10(np.sqrt((v**2).mean())+1e-12):6.1f} dBFS')
    v.T.astype(np.float32).tofile(R + f'ref_{s}.f32')
