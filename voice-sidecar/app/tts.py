"""Piper voice. One ONNX session, serialized because the session is not re-entrant."""

from __future__ import annotations

import io
import threading
import wave
from pathlib import Path

from piper import PiperVoice


class Speaker:
    def __init__(self, model_path: Path) -> None:
        self._voice = PiperVoice.load(model_path)
        self._lock = threading.Lock()

    def synthesize(self, text: str) -> bytes:
        buffer = io.BytesIO()
        with self._lock:
            with wave.open(buffer, "wb") as wav_file:
                self._voice.synthesize_wav(text, wav_file)
        return buffer.getvalue()
