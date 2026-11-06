import sys, time, resource, numpy as np, onnxruntime as ort
M = '/home/jaspermeller/.cache/slipmat/models/'
x = np.fromfile('/home/jaspermeller/slipmat/research/summer.f32', dtype=np.float32).reshape(-1, 2).T.copy()  # (2, n)
which = sys.argv[1]
so = ort.SessionOptions(); so.intra_op_num_threads = int(sys.argv[2]) if len(sys.argv) > 2 else 0
if which == 'spleeter':
    N, H, T, F = 4096, 1024, 512, 1024
    W = np.hanning(N + 1)[:-1].astype(np.float32)
    pad = N - H
    def stft(v):
        n = int(np.ceil((pad + len(v)) / H)); p = np.zeros((n - 1) * H + N, np.float32); p[pad:pad + len(v)] = v
        idx = np.arange(N)[None, :] + H * np.arange(n)[:, None]
        return np.fft.rfft(p[idx] * W, axis=-1)
    t0 = time.time()
    spec = np.stack([stft(x[c]) for c in range(2)]); frames = spec.shape[1]; splits = int(np.ceil(frames / T))
    mag = np.zeros((2, splits * T, F), np.float32); mag[:, :frames] = np.abs(spec[:, :, :F]); net = mag.reshape(2, splits, T, F)
    t1 = time.time()
    for s in ('vocals', 'drums', 'bass', 'other'):
        sess = ort.InferenceSession(M + f'spleeter-{s}.fp16.onnx', so, providers=['CPUExecutionProvider'])
        ts = time.time(); sess.run(None, {'x': net}); print(s, f'{time.time()-ts:.2f}s')
    print(f'spleeter: stft {t1-t0:.2f}s, total {time.time()-t0:.2f}s for {x.shape[1]/44100:.0f}s audio')
else:
    sess = ort.InferenceSession(M + 'htdemucs_fp16weights.onnx', so, providers=['CPUExecutionProvider'])
    print([ (i.name, i.shape) for i in sess.get_inputs()], [(o.name, o.shape) for o in sess.get_outputs()])
    seg = x[:, :343980][None].astype(np.float32)
    for _ in range(2):
        t0 = time.time(); out = sess.run(None, {sess.get_inputs()[0].name: seg})[0]; print(f'htdemucs 7.8 s segment: {time.time()-t0:.2f}s', out.shape)
print('peak RSS MB', resource.getrusage(resource.RUSAGE_SELF).ru_maxrss // 1024)
