from app.segmenter import (
    END_SILENCE_SAMPLES,
    FRAME_SAMPLES,
    MAX_SAMPLES,
    SpeechStart,
    Utterance,
    Segmenter,
    floats_to_pcm16,
    pcm16_to_floats,
)


def _frame(first: float) -> bytes:
    return floats_to_pcm16([first] + [0.0] * (FRAME_SAMPLES - 1))


def test_silence_produces_no_phrases():
    segmenter = Segmenter(lambda _frame: 0.0)
    two_seconds = b"\x00\x00" * (2 * 16000)
    assert segmenter.push(two_seconds) == []


def test_speech_then_silence_emits_one_phrase_with_preroll():
    seen = {"n": 0}

    def score(_frame):
        seen["n"] += 1
        return 0.9 if seen["n"] == 4 else 0.0

    segmenter = Segmenter(score)
    events = []
    events.extend(segmenter.push(_frame(0.25)))
    events.extend(segmenter.push(_frame(0.0)))
    events.extend(segmenter.push(_frame(0.0)))
    events.extend(segmenter.push(_frame(0.5)))
    assert any(isinstance(event, SpeechStart) for event in events)

    events.extend(segmenter.push(b"\x00\x00" * (END_SILENCE_SAMPLES + FRAME_SAMPLES)))
    utterances = [event for event in events if isinstance(event, Utterance)]
    starts = [event for event in events if isinstance(event, SpeechStart)]
    assert len(starts) == 1
    assert len(utterances) == 1
    first = pcm16_to_floats(utterances[0].pcm16[:2])[0]
    assert abs(first - 0.25) < 0.01


def test_long_speech_is_cut_at_fifteen_seconds():
    segmenter = Segmenter(lambda _frame: 0.9)
    events = segmenter.push(b"\x00\x10" * (MAX_SAMPLES + FRAME_SAMPLES))
    utterances = [event for event in events if isinstance(event, Utterance)]
    assert len(utterances) == 1
    samples = len(utterances[0].pcm16) // 2
    assert MAX_SAMPLES <= samples <= MAX_SAMPLES + FRAME_SAMPLES
