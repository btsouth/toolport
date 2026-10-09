#!/usr/bin/env python3
"""Quantize a pinned public Model2Vec download. No labels or task-specific training.

Requires numpy and safetensors. Pass the upstream folder and output folder.
The shipped tokenizer is preserved byte for byte; the MIT notice is embedded
in the quantized data so every gateway binary retains it.
"""
import hashlib
import pathlib
import shutil
import struct
import sys

import numpy as np
from safetensors.numpy import load_file

source, output = map(pathlib.Path, sys.argv[1:])
output.mkdir(parents=True, exist_ok=True)
weights = load_file(str(source / "model.safetensors"))["embeddings"]
assert weights.shape == (29528, 256)
scales = np.max(np.abs(weights), axis=1) / 127
scales[scales == 0] = 1
quantized = np.round(weights / scales[:, None]).clip(-127, 127).astype(np.int8)
license_path = pathlib.Path(__file__).resolve().parents[1] / "src-tauri/assets/search/LICENSE"
payload = (
    b"TPSEMQ01"
    + struct.pack("<III", *weights.shape, 8)
    + scales.astype("<f4").tobytes()
    + quantized.tobytes()
    + license_path.read_bytes()
)
(output / "model-q8.bin").write_bytes(payload)
shutil.copyfile(source / "tokenizer.json", output / "tokenizer.json")
for path in [source / "model.safetensors", output / "model-q8.bin", output / "tokenizer.json"]:
    print(hashlib.sha256(path.read_bytes()).hexdigest(), path.name)
