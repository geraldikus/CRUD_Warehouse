import hashlib
import io
import json
import threading
import zipfile
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

import pytest

from app.bootstrap import BootstrapError, ensure


class _Server:
    def __init__(self, files: dict[str, bytes]) -> None:
        self.hits = 0
        files_box = files
        owner = self

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):  # noqa: N802 - stdlib name
                owner.hits += 1
                body = files_box.get(self.path)
                if body is None:
                    self.send_response(404)
                    self.end_headers()
                    return
                self.send_response(200)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, _format, *_args):
                return

        self._httpd = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.port = self._httpd.server_address[1]
        thread = threading.Thread(target=self._httpd.serve_forever, daemon=True)
        thread.start()

    def close(self) -> None:
        self._httpd.shutdown()


def _zip_bytes() -> bytes:
    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, "w") as archive:
        archive.writestr("vosk-model-small-en-us-0.15/conf/model.conf", "x")
        archive.writestr("vosk-model-small-en-us-0.15/am/final.mdl", "m")
    return buffer.getvalue()


def _manifest(base: str, zip_blob: bytes, raw: bytes, *, zip_sha: str | None = None) -> dict:
    return {
        "models": [
            {
                "name": "vosk",
                "files": [
                    {
                        "url": f"{base}/vosk.zip",
                        "sha256": zip_sha or hashlib.sha256(zip_blob).hexdigest(),
                        "archive": "zip",
                    }
                ],
            },
            {
                "name": "vad",
                "files": [
                    {
                        "url": f"{base}/vad.onnx",
                        "sha256": hashlib.sha256(raw).hexdigest(),
                        "filename": "silero_vad.onnx",
                    }
                ],
            },
        ]
    }


def test_second_run_does_not_download_again(tmp_path: Path):
    zip_blob = _zip_bytes()
    raw = b"onnx-bytes"
    server = _Server({"/vosk.zip": zip_blob, "/vad.onnx": raw})
    try:
        manifest = tmp_path / "models.json"
        manifest.write_text(
            json.dumps(_manifest(f"http://127.0.0.1:{server.port}", zip_blob, raw)),
            encoding="utf-8",
        )
        root = tmp_path / "models"
        ensure(manifest, root)
        hits_after_first = server.hits
        ensure(manifest, root)
        assert server.hits == hits_after_first
        assert (root / "vosk" / ".ready").is_file()
        assert (root / "vosk" / "conf" / "model.conf").read_text(encoding="utf-8") == "x"
        assert not (root / "vosk" / "vosk-model-small-en-us-0.15").exists()
        assert (root / "vad" / "silero_vad.onnx").read_bytes() == raw
    finally:
        server.close()


def test_bad_sha256_leaves_no_ready_marker(tmp_path: Path):
    zip_blob = _zip_bytes()
    raw = b"onnx-bytes"
    server = _Server({"/vosk.zip": zip_blob, "/vad.onnx": raw})
    try:
        manifest = tmp_path / "models.json"
        manifest.write_text(
            json.dumps(
                _manifest(
                    f"http://127.0.0.1:{server.port}",
                    zip_blob,
                    raw,
                    zip_sha="0" * 64,
                )
            ),
            encoding="utf-8",
        )
        root = tmp_path / "models"
        with pytest.raises(BootstrapError, match="sha256"):
            ensure(manifest, root)
        assert not (root / "vosk" / ".ready").exists()
        assert not (root / ".partial-vosk").exists()
    finally:
        server.close()


def test_zip_slip_is_rejected(tmp_path: Path):
    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, "w") as archive:
        archive.writestr("../evil.txt", "nope")
    blob = buffer.getvalue()
    server = _Server({"/vosk.zip": blob})
    try:
        manifest = tmp_path / "models.json"
        manifest.write_text(
            json.dumps(
                {
                    "models": [
                        {
                            "name": "vosk",
                            "files": [
                                {
                                    "url": f"http://127.0.0.1:{server.port}/vosk.zip",
                                    "sha256": hashlib.sha256(blob).hexdigest(),
                                    "archive": "zip",
                                }
                            ],
                        }
                    ]
                }
            ),
            encoding="utf-8",
        )
        with pytest.raises(BootstrapError, match="unsafe zip path"):
            ensure(manifest, tmp_path / "models")
    finally:
        server.close()
