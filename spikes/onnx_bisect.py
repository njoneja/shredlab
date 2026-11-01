"""Export RoFormer sub-modules one at a time and compare ONNX vs torch."""

import io
import sys

import numpy as np
import onnxruntime as ort
import torch

sys.path.insert(0, str(__import__("pathlib").Path(__file__).parent))
from onnx_roformer import RoformerCore, load_torch_model, patch_for_export  # noqa: E402


def check(name, module, *inputs):
    buf = io.BytesIO()
    torch.onnx.export(module, inputs, buf, opset_version=17, do_constant_folding=True)
    sess = ort.InferenceSession(buf.getvalue(), providers=["CPUExecutionProvider"])
    feeds = {i.name: x.numpy() for i, x in zip(sess.get_inputs(), inputs)}
    got = sess.run(None, feeds)[0]
    with torch.no_grad():
        ref = module(*inputs).numpy()
    rel = np.abs(got - ref).max() / (np.abs(ref).max() + 1e-12)
    print(f"{name:28s} rel err {rel:.2e}  shape {ref.shape}")


def main():
    patch_for_export()
    model, _ = load_torch_model()
    core = RoformerCore(model).eval()  # applies band split / mask / GLU patches
    torch.manual_seed(0)
    t, bands, d = 64, len(model.band_split.dim_inputs), 256

    x_in = torch.randn(1, t, sum(model.band_split.dim_inputs))
    feats = torch.randn(1, t, bands, d)
    seq = torch.randn(bands, t, d)

    time_t = model.layers[0][-2]
    check("band_split", model.band_split, x_in)
    check("time transformer", time_t, seq)
    check("time attention only", time_t.layers[0][0], seq)
    check("time feedforward only", time_t.layers[0][1], seq)
    check("final_norm", model.final_norm, feats)
    check("mask_estimator[0]", model.mask_estimators[0], feats)
    check("full core (small t)", core, torch.randn(1, 2, 1025, t, 2))


if __name__ == "__main__":
    main()
