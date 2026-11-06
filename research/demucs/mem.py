import sys, time, numpy as np, onnxruntime as ort
M = '/home/jaspermeller/.cache/slipmat/models/htdemucs_fp16weights.onnx'
lvl = {'off': ort.GraphOptimizationLevel.ORT_DISABLE_ALL, 'basic': ort.GraphOptimizationLevel.ORT_ENABLE_BASIC, 'all': ort.GraphOptimizationLevel.ORT_ENABLE_ALL}[sys.argv[1]]
so = ort.SessionOptions(); so.graph_optimization_level = lvl; so.enable_cpu_mem_arena = False; so.enable_mem_pattern = False; so.intra_op_num_threads = 4
t = time.time(); s = ort.InferenceSession(M, so, providers=['CPUExecutionProvider']); print(f'load {time.time()-t:.1f}s', flush=True)
x = np.fromfile('/home/jaspermeller/slipmat/research/summer.f32', np.float32).reshape(-1, 2).T[:, :343980][None].copy()
t = time.time(); y = s.run(['stems'], {'mix': x})[0]; print(f'segment {time.time()-t:.1f}s', y.shape, flush=True)
