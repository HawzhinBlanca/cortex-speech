#!/usr/bin/env python3
"""The champion 7B must accept every container the rest of the app accepts.

MEASURED 2026-08-10: a 25-clip podcast source in an .mp4 container failed EVERY 7B request with
"Error opening ...: Format not recognised" — libsndfile, torchaudio's soundfile backend, handles no
MPEG-4/AAC. The app's own import path decodes mp4 fine, so those clips imported, chunked and
transcribed locally, then silently could not be re-transcribed by the champion. The review queue ended
up 462 clips at 7B quality and 25 at the weaker engine, with nothing on screen saying so — mixed
provenance inside one dataset is exactly the kind of thing that quietly corrupts a measurement.

Two checks, because either alone would be hollow:
  * the fallback is WIRED (source pin) — and fails loudly rather than returning an empty clip, since
    the caller treats "" as a transient "server under load" and would leave the old transcript.
  * the fallback WORKS (functional) — the exact ffmpeg argv is run against a real generated .mp4 and
    must yield the sample count and dtype the server then hands to torch. A pinned command line that
    nobody executes is a guess.

The functional half needs ffmpeg (already a hard dependency of the import path) and SKIPs honestly
without it. It needs no torch, no model and no GPU, so it runs on the ordinary policy runner.
"""

from __future__ import annotations

import array
import shutil
import subprocess
import tempfile
from pathlib import Path

SERVER = Path(__file__).resolve().parent / "cortex_7b_server.py"


def test_the_server_falls_back_to_ffmpeg_and_fails_loudly() -> None:
    src = SERVER.read_text(encoding="utf-8")

    assert "def load_any(" in src, "the 7B server must route audio loading through a fallback loader"
    assert "torchaudio.load(audio_path)" in src, "torchaudio must still be the fast path"

    start = src.index("def load_any(")
    end = src.index("def transcribe(", start)
    body = src[start:end]

    assert "ffmpeg" in body, "no ffmpeg fallback: a container torchaudio cannot open would still fail"
    assert "raise RuntimeError" in body, (
        "the fallback must RAISE when ffmpeg fails. Returning an empty tensor transcribes to \"\", "
        "which the caller reads as a transient server-under-load result and leaves the stale "
        "transcript in place — a silent wrong-engine row."
    )
    assert "transcribe(audio_path" not in body.replace("def load_any(", ""), "loader must not transcribe"

    transcribe_body = src[end:end + 800]
    # 2026-08-20 (external review blocker #4): the loader now takes the requested WINDOW so the
    # server decodes only the clip, never the whole source. transcribe() must pass it through.
    assert "load_any(audio_path, start_ms, end_ms)" in transcribe_body, (
        "transcribe() must decode via load_any WITH the clip window — a bare-path call decodes the whole source"
    )
    assert '"-ss"' in body and '"-t"' in body, (
        "the ffmpeg call must SEEK to the requested window; decoding a 77-minute episode for a 9s clip "
        "scales work with the source instead of the clip"
    )
    # 2026-08-20 (review): the seek must start EARLY and trim the excess. A decoder started mid-stream
    # has no history — MEASURED on a 320 kbps MP3 master at a 42-minute offset, a bare seek made the
    # first 10 ms of every clip as wrong as the signal was loud (0.0 dB) and 10-50 ms -5.4 dB, i.e. the
    # word ONSET was garbage while everything from 50 ms on was bit-identical. With the pad the onset
    # measures -381 dB against the exact whole-file slice: silence, as it should be.
    assert "SEEK_PAD_MS" in src, "the padded-seek constant is gone — clip onsets go back to being decoder garbage"
    assert "samples = samples[drop:]" in body, (
        "the padded lead-in must be TRIMMED off; leaving it shifts every clip earlier by the pad"
    )


def test_the_ffmpeg_argv_actually_decodes_an_mp4() -> None:
    """Run the server's exact flags against a real .mp4 and check what comes back."""
    if not shutil.which("ffmpeg"):
        print("SKIP-ENV: ffmpeg not on PATH (it is a hard dependency of the import path)")
        return

    src = SERVER.read_text(encoding="utf-8")
    for flag in ('"-f", "f32le"', '"-ac", "1"', '"-ar", "16000"'):
        assert flag in src, f"the server's ffmpeg call no longer passes {flag}; this test would drift"

    with tempfile.TemporaryDirectory() as tmp:
        mp4 = Path(tmp) / "tone.mp4"
        made = subprocess.run(
            ["ffmpeg", "-nostdin", "-loglevel", "error", "-f", "lavfi",
             "-i", "sine=frequency=440:duration=1", "-c:a", "aac", str(mp4)],
            capture_output=True,
        )
        if made.returncode != 0 or not mp4.is_file():
            print("SKIP-ENV: this ffmpeg cannot mux AAC/mp4, so the case cannot be reproduced here")
            return

        # The exact argv from cortex_7b_server.load_any.
        out = subprocess.run(
            ["ffmpeg", "-nostdin", "-loglevel", "error", "-i", str(mp4),
             "-f", "f32le", "-acodec", "pcm_f32le", "-ac", "1", "-ar", "16000", "-"],
            capture_output=True,
        )
        assert out.returncode == 0, f"ffmpeg failed on a plain mp4: {out.stderr.decode(errors='replace')[:200]}"
        assert out.stdout, "ffmpeg produced no PCM for a one-second tone"

        samples = array.array("f")
        samples.frombytes(out.stdout[: len(out.stdout) // 4 * 4])
        # 1 s at 16 kHz mono; encoder padding makes this approximate, not exact.
        assert 15000 <= len(samples) <= 18000, f"expected ~16000 mono samples at 16 kHz, got {len(samples)}"
        assert any(abs(value) > 0.01 for value in samples[:8000]), "decoded audio is silent — a 440 Hz tone is not"


def main() -> None:
    test_the_server_falls_back_to_ffmpeg_and_fails_loudly()
    test_the_ffmpeg_argv_actually_decodes_an_mp4()
    print("7B audio decode policy passed")


if __name__ == "__main__":
    main()
