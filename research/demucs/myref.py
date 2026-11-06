# Independent Demucs reference: whole-clip chunking with 25% linear crossfades,
# written from the model card's spec (input "mix" (1,2,343980), output "stems"
# (1,4,2,343980) as drums, bass, other, vocals).
import numpy as np, onnxruntime as ort
N = 343980; OV = N // 4; ST = N - OV
x = np.fromfile('/home/jaspermeller/slipmat/research/summer.f32', np.float32).reshape(-1, 2).T.copy()
so = ort.SessionOptions(); so.graph_optimization_level = ort.GraphOptimizationLevel.ORT_DISABLE_ALL
so.enable_cpu_mem_arena = False; so.enable_mem_pattern = False; so.intra_op_num_threads = 4
sess = ort.InferenceSession('/home/jaspermeller/.cache/slipmat/models/htdemucs_fp16weights.onnx', so, providers=['CPUExecutionProvider'])
w = np.ones(N, np.float32); fade = np.linspace(0, 1, OV, dtype=np.float32); w[:OV] = fade; w[-OV:] = fade[::-1]
T = x.shape[1]; out = np.zeros((4, 2, T), np.float32); wt = np.zeros(T, np.float32)
for i in range(max(1, -(-T // ST))):
    a = i * ST; b = min(a + N, T)
    if a >= T: break
    seg = np.zeros((2, N), np.float32); seg[:, :b - a] = x[:, a:b]
    y = sess.run(['stems'], {'mix': seg[None]})[0][0]
    out[:, :, a:b] += y[:, :, :b - a] * w[:b - a]; wt[a:b] += w[:b - a]
    if b >= T: break  # the piece that reaches the end is the last (as in Slipmat)
out /= np.maximum(wt, 1e-8)
for k, s in [(3, 'vocals'), (0, 'drums'), (1, 'bass')]:
    out[k].T.astype(np.float32).tofile(f'ref_{s}.f32')
err = np.sqrt(((out.sum(0) - x) ** 2).mean()) / np.sqrt((x ** 2).mean())
print('stems summed vs mix (dB):', round(float(20 * np.log10(err)), 1))
