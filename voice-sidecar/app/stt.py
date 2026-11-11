"""One Vosk model for the process, one recognizer per phrase."""

from __future__ import annotations

import json
import threading
from pathlib import Path

from vosk import KaldiRecognizer, Model


class Transcriber:
    def __init__(self, model_path: Path) -> None:
        self._model = Model(str(model_path))
        self._lock = threading.Lock()

    def transcribe(self, pcm16: bytes) -> str:
        if not pcm16:
            return ""
        with self._lock:
            recognizer = KaldiRecognizer(self._model, 16000)
            recognizer.AcceptWaveform(pcm16)
            payload = json.loads(recognizer.FinalResult())
        return str(payload.get("text", "")).strip()
