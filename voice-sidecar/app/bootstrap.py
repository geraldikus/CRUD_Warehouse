"""Download speech models on first start, then run the HTTP server.

Models are not in git. A marker file `.ready` inside each directory means
that download finished. A crash mid-download leaves no marker, so the next
start fetches that model again. The sha256 is checked before anything is
renamed into place.
"""

from __future__ import annotations

import hashlib
import io
import json
import os
import shutil
import sys
import urllib.request
import zipfile
from pathlib import Path

MANIFEST = Path(__file__).with_name("models.json")


class BootstrapError(Exception):
    """A model could not be downloaded or did not match its checksum."""


def ensure(manifest_path: Path, root: Path) -> None:
    """Make every model in the manifest available under `root`."""
    root.mkdir(parents=True, exist_ok=True)
    spec = json.loads(manifest_path.read_text(encoding="utf-8"))
    for model in spec["models"]:
        name = model["name"]
        dest = root / name
        if (dest / ".ready").is_file():
            print(f"{name}: already downloaded", flush=True)
            continue
        if dest.exists():
            shutil.rmtree(dest)
        tmp = root / f".partial-{name}"
        if tmp.exists():
            shutil.rmtree(tmp)
        tmp.mkdir()
        try:
            for entry in model["files"]:
                print(f"{name}: downloading {entry['url']}", flush=True)
                blob = download(entry["url"])
                digest = hashlib.sha256(blob).hexdigest()
                expected = entry["sha256"].lower()
                if digest != expected:
                    raise BootstrapError(
                        f"{name}: sha256 mismatch (got {digest}, expected {expected})"
                    )
                if entry.get("archive") == "zip":
                    extract_zip(blob, tmp)
                else:
                    target = tmp / entry["filename"]
                    target.parent.mkdir(parents=True, exist_ok=True)
                    target.write_bytes(blob)
            (tmp / ".ready").write_text("ok\n", encoding="utf-8")
            os.rename(tmp, dest)
            print(f"{name}: ready", flush=True)
        except Exception:
            shutil.rmtree(tmp, ignore_errors=True)
            raise


def download(url: str) -> bytes:
    request = urllib.request.Request(
        url, headers={"User-Agent": "warehouse-voice-sidecar"}
    )
    with urllib.request.urlopen(request, timeout=60) as response:
        total = int(response.headers.get("Content-Length") or 0)
        chunks: list[bytes] = []
        got = 0
        next_report = 8 * 1024 * 1024
        while True:
            chunk = response.read(256 * 1024)
            if not chunk:
                break
            chunks.append(chunk)
            got += len(chunk)
            if total and got >= next_report:
                print(f"  {_size(got)} / {_size(total)}", flush=True)
                next_report += 8 * 1024 * 1024
        print(f"  downloaded {_size(got)}", flush=True)
        return b"".join(chunks)


def extract_zip(blob: bytes, dest: Path) -> None:
    """Unpack a zip, rejecting paths that escape `dest`, then flatten one wrapper dir."""
    with zipfile.ZipFile(io.BytesIO(blob)) as archive:
        for info in archive.infolist():
            if info.is_dir():
                continue
            mode = (info.external_attr >> 16) & 0o170000
            if mode == 0o120000:
                raise BootstrapError(f"refusing symlink in zip: {info.filename}")
            relative = Path(info.filename)
            if relative.is_absolute() or ".." in relative.parts:
                raise BootstrapError(f"unsafe zip path: {info.filename}")
            target = dest / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            with archive.open(info) as src, target.open("wb") as out:
                shutil.copyfileobj(src, out)
    _flatten_single_directory(dest)


def _flatten_single_directory(dest: Path) -> None:
    """Vosk zips wrap the model in one folder. Lift those files up."""
    children = [path for path in dest.iterdir() if path.name != ".ready"]
    if len(children) != 1 or not children[0].is_dir():
        return
    inner = children[0]
    for child in inner.iterdir():
        os.replace(child, dest / child.name)
    inner.rmdir()


def _size(nbytes: int) -> str:
    if nbytes >= 1024 * 1024:
        return f"{nbytes / (1024 * 1024):.1f} MiB"
    if nbytes >= 1024:
        return f"{nbytes / 1024:.1f} KiB"
    return f"{nbytes} B"


def main() -> None:
    root = Path(os.environ.get("MODELS_DIR", "/models"))
    try:
        ensure(MANIFEST, root)
    except Exception as err:
        print(f"model download failed: {err}", file=sys.stderr, flush=True)
        sys.exit(1)
    os.execvp(
        "uvicorn",
        ["uvicorn", "app.main:app", "--host", "0.0.0.0", "--port", "8000"],
    )


if __name__ == "__main__":
    main()
