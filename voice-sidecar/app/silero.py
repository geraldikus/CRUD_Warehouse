"""Silero VAD through onnxruntime. No PyTorch.

The v6 model expects 512 new samples plus 64 samples of context, and a
state tensor of shape (2, 1, 128). Each call updates both.
"""

from __future__ import annotations

from pathlib import Path

CONTEXT_SAMPLES = 64
FRAME_SAMPLES = 512


class SileroVad:
    def __init__(self, model_path: Path) -> None:
        import numpy as np
        import onnxruntime as ort

        options = ort.SessionOptions()
        options.inter_op_num_threads = 1
        options.intra_op_num_threads = 1
        self._np = np
        self._session = ort.InferenceSession(
            str(model_path),
            sess_options=options,
            providers=["CPUExecutionProvider"],
        )
        self._state = np.zeros((2, 1, 128), dtype=np.float32)
        self._context = np.zeros((1, CONTEXT_SAMPLES), dtype=np.float32)

    def __call__(self, frame: list[float]) -> float:
        np = self._np
        if len(frame) != FRAME_SAMPLES:
            raise ValueError(f"silero expects {FRAME_SAMPLES} samples, got {len(frame)}")
        audio = np.asarray(frame, dtype=np.float32).reshape(1, FRAME_SAMPLES)
        batched = np.concatenate([self._context, audio], axis=1)
        out, state = self._session.run(
            None,
            {
                "input": batched,
                "state": self._state,
                "sr": np.array(16000, dtype=np.int64),
            },
        )
        self._state = state
        self._context = batched[:, -CONTEXT_SAMPLES:]
        return float(np.reshape(out, -1)[0])
