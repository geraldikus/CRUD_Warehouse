"""Cut a PCM stream into phrases.

The score function returns a speech probability for one 512-sample frame
at 16 kHz. Silero supplies that in production. Tests pass a fake so they
do not need the model file.

A phrase starts when the probability crosses 0.5, ends after about 600 ms
of silence, and never grows past 15 seconds. The 300 ms before the start
is kept so the first consonant is not clipped.
"""

from __future__ import annotations

import array
import sys
from collections import deque
from collections.abc import Callable
from dataclasses import dataclass

SAMPLE_RATE = 16_000
FRAME_SAMPLES = 512
THRESHOLD = 0.5
END_SILENCE_SAMPLES = int(0.6 * SAMPLE_RATE)
PREROLL_SAMPLES = int(0.3 * SAMPLE_RATE)
MAX_SAMPLES = 15 * SAMPLE_RATE
FRAME_BYTES = FRAME_SAMPLES * 2


@dataclass(frozen=True)
class SpeechStart:
    """The probability just crossed the threshold."""


@dataclass(frozen=True)
class Utterance:
    """One finished phrase, 16 kHz mono PCM16 little-endian."""

    pcm16: bytes


Event = SpeechStart | Utterance


class Segmenter:
    def __init__(self, score: Callable[[list[float]], float]) -> None:
        self._score = score
        self._raw = bytearray()
        self._offset = 0
        self._preroll: deque[float] = deque(maxlen=PREROLL_SAMPLES)
        self._utterance: list[float] = []
        self._in_speech = False
        self._silence_samples = 0

    def push(self, pcm16: bytes) -> list[Event]:
        self._raw.extend(pcm16)
        events: list[Event] = []
        while len(self._raw) - self._offset >= FRAME_BYTES:
            raw = bytes(self._raw[self._offset : self._offset + FRAME_BYTES])
            self._offset += FRAME_BYTES
            events.extend(self._feed(pcm16_to_floats(raw)))
        # Drop consumed bytes in one slice. Deleting the front of a
        # bytearray on every frame copies the rest, which is quadratic
        # for a long phrase sent in one chunk.
        if self._offset >= 65536:
            del self._raw[: self._offset]
            self._offset = 0
        return events

    def _feed(self, frame: list[float]) -> list[Event]:
        probability = float(self._score(frame))
        if not self._in_speech:
            self._preroll.extend(frame)
            if probability <= THRESHOLD:
                return []
            self._in_speech = True
            self._silence_samples = 0
            self._utterance = list(self._preroll)
            return [SpeechStart()]

        self._utterance.extend(frame)
        if probability > THRESHOLD:
            self._silence_samples = 0
        else:
            self._silence_samples += len(frame)

        if (
            len(self._utterance) < MAX_SAMPLES
            and self._silence_samples < END_SILENCE_SAMPLES
        ):
            return []

        utterance = Utterance(pcm16=floats_to_pcm16(self._utterance))
        self._in_speech = False
        self._utterance = []
        self._silence_samples = 0
        self._preroll.clear()
        return [utterance]


def pcm16_to_floats(raw: bytes) -> list[float]:
    count = len(raw) // 2
    data = array.array("h")
    data.frombytes(raw[: count * 2])
    if sys.byteorder != "little":
        data.byteswap()
    return [sample / 32768.0 for sample in data]


def floats_to_pcm16(samples: list[float]) -> bytes:
    data = array.array("h")
    for sample in samples:
        clamped = max(-1.0, min(1.0, sample))
        data.append(int(round(clamped * 32767.0)))
    if sys.byteorder != "little":
        data.byteswap()
    return data.tobytes()
