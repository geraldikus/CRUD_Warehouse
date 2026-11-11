"""Local speech service.

`GET /health` answers only after the models are loaded. `WS /stream` takes
16 kHz mono PCM16 and emits phrase events. `POST /tts` returns a WAV.
"""

from __future__ import annotations

import logging
import os
from contextlib import asynccontextmanager
from pathlib import Path

from fastapi import FastAPI, HTTPException, WebSocket, WebSocketDisconnect
from fastapi.concurrency import run_in_threadpool
from fastapi.responses import JSONResponse, Response
from pydantic import BaseModel

from app.segmenter import Segmenter, SpeechStart, Utterance
from app.silero import SileroVad
from app.stt import Transcriber
from app.tts import Speaker

log = logging.getLogger("voice")

MAX_TTS_CHARS = 2000


def models_dir() -> Path:
    return Path(os.environ.get("MODELS_DIR", "/models"))


@asynccontextmanager
async def lifespan(app: FastAPI):
    root = models_dir()
    vosk_path = root / "vosk"
    piper_path = root / "piper" / "en_US-lessac-low.onnx"
    vad_path = root / "vad" / "silero_vad.onnx"
    for path in (vosk_path, piper_path, vad_path):
        if not path.exists():
            raise RuntimeError(f"missing model file {path}; bootstrap did not finish")
    log.info("loading vosk from %s", vosk_path)
    app.state.transcriber = Transcriber(vosk_path)
    log.info("loading piper from %s", piper_path)
    app.state.speaker = Speaker(piper_path)
    app.state.vad_path = vad_path
    app.state.ready = True
    log.info("voice models ready")
    yield


app = FastAPI(lifespan=lifespan)


@app.get("/health")
def health():
    if not getattr(app.state, "ready", False):
        return JSONResponse({"status": "loading"}, status_code=503)
    return {"status": "ok"}


class TtsRequest(BaseModel):
    text: str


@app.post("/tts")
async def tts(body: TtsRequest):
    text = body.text.strip()
    if not text:
        raise HTTPException(status_code=400, detail="text must not be empty")
    if len(text) > MAX_TTS_CHARS:
        raise HTTPException(status_code=400, detail="text is too long")
    wav = await run_in_threadpool(app.state.speaker.synthesize, text)
    return Response(content=wav, media_type="audio/wav")


@app.websocket("/stream")
async def stream(websocket: WebSocket) -> None:
    await websocket.accept()
    if not getattr(app.state, "ready", False):
        await websocket.send_json({"type": "error", "message": "voice is not ready"})
        await websocket.close()
        return

    try:
        segmenter = Segmenter(SileroVad(app.state.vad_path))
    except Exception:
        log.exception("failed to open the vad")
        await websocket.send_json({"type": "error", "message": "voice is not ready"})
        await websocket.close()
        return

    try:
        while True:
            message = await websocket.receive()
            if message["type"] == "websocket.disconnect":
                break
            data = message.get("bytes")
            if not data:
                continue
            for event in segmenter.push(bytes(data)):
                if isinstance(event, SpeechStart):
                    await websocket.send_json({"type": "speech_start"})
                elif isinstance(event, Utterance):
                    await _transcribe(websocket, event.pcm16)
    except WebSocketDisconnect:
        return


async def _transcribe(websocket: WebSocket, pcm16: bytes) -> None:
    try:
        text = await run_in_threadpool(app.state.transcriber.transcribe, pcm16)
    except Exception:
        log.exception("transcription failed")
        await websocket.send_json({"type": "error", "message": "transcription failed"})
        return
    if not text:
        return
    log.info("transcript chars=%s", len(text))
    await websocket.send_json({"type": "transcript", "text": text})
