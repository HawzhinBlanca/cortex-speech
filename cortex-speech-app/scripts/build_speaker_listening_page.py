#!/usr/bin/env python3
"""Build a self-contained listening page so a human ear can settle what CAM++ cannot.

WHY. `src-tauri/src/bin/speaker_change_probe.rs` measures whether a clip's two halves sound like the
same person, and its own controls say the measurement is INCONCLUSIVE: on the owner's material the
same-speaker and different-speaker distributions sit 0.009 apart. The probe uses CAM++'s own labels as
ground truth for CAM++, so it is circular by construction and cannot escape that on its own. Fifteen
minutes of listening breaks the circle.

The clips are presented in SHUFFLED order and the page shows no similarity score, no CAM++ label and no
band until after the answers are exported. That is deliberate: knowing a clip was picked as
"most likely to hold two speakers" is exactly the kind of hint that turns a measurement into a
confirmation of what it was told to expect.

PRIVACY. Audio is embedded as base64 into a LOCAL file. This is biometric data under GDPR Art. 9 and
must not be uploaded to any hosting service — that is why this is a file on disk and not a hosted page.

Usage:
  python scripts/build_speaker_listening_page.py <listening-set-dir> [-o out.html]
"""

from __future__ import annotations

import argparse
import base64
import json
import random
import sys
from pathlib import Path


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("dir", type=Path, help="directory written by speaker_change_probe --export")
    ap.add_argument("-o", "--out", type=Path, default=None)
    args = ap.parse_args()

    manifest_path = args.dir / "manifest.json"
    if not manifest_path.exists():
        print(f"no manifest at {manifest_path}", file=sys.stderr)
        return 1
    items = json.loads(manifest_path.read_text(encoding="utf-8"))

    # Deterministic shuffle: the same set always presents in the same order, so a second pass can be
    # compared against the first, but the order carries no information about similarity.
    random.Random(20260731).shuffle(items)

    for it in items:
        wav = args.dir / it["file"]
        if not wav.exists():
            print(f"missing clip: {wav}", file=sys.stderr)
            return 1
        it["b64"] = base64.b64encode(wav.read_bytes()).decode("ascii")

    out = args.out or (args.dir / "listen.html")
    out.write_text(PAGE.replace("__DATA__", json.dumps(items)), encoding="utf-8")
    total_mb = out.stat().st_size / 1e6
    print(f"wrote {out}  ({len(items)} clips, {total_mb:.1f} MB)")
    return 0


PAGE = r"""<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8" />
<meta name="viewport" content="width=device-width, initial-scale=1" />
<title>How many speakers?</title>
<style>
  :root { color-scheme: light dark; --bg:#f8fafc; --panel:#fff; --line:#cbd5e1; --ink:#0f172a;
          --dim:#475569; --accent:#0369a1; --ok:#15803d; --warn:#a16207; }
  @media (prefers-color-scheme: dark) {
    :root { --bg:#0b1220; --panel:#111a2e; --line:#334155; --ink:#e2e8f0; --dim:#94a3b8; }
  }
  * { box-sizing: border-box; margin: 0; }
  body { font-family: system-ui, sans-serif; background: var(--bg); color: var(--ink);
         padding: 20px; max-width: 760px; margin: 0 auto; }
  h1 { font-size: 20px; margin-bottom: 6px; }
  p.lead { color: var(--dim); font-size: 14px; line-height: 1.6; margin-bottom: 18px; }
  .card { background: var(--panel); border: 1px solid var(--line); border-radius: 12px;
          padding: 14px; margin-bottom: 12px; }
  .card.answered { border-color: var(--ok); }
  .n { font-size: 12px; color: var(--dim); font-weight: 600; margin-bottom: 8px; }
  audio { width: 100%; margin-bottom: 10px; }
  .row { display: flex; gap: 8px; flex-wrap: wrap; }
  button { flex: 1; min-width: 110px; padding: 12px 8px; border-radius: 10px; border: 1px solid var(--line);
           background: transparent; color: var(--ink); font-size: 14px; font-weight: 600; cursor: pointer; }
  button[aria-pressed="true"] { background: var(--accent); border-color: var(--accent); color: #f1f5f9; }
  #done { position: sticky; bottom: 0; background: var(--panel); border: 1px solid var(--line);
          border-radius: 12px; padding: 14px; margin-top: 18px; }
  #result { width: 100%; min-height: 90px; margin-top: 10px; font-family: ui-monospace, monospace;
            font-size: 12px; padding: 10px; border-radius: 8px; border: 1px solid var(--line);
            background: var(--bg); color: var(--ink); }
  .count { font-size: 13px; color: var(--dim); }
</style>
</head>
<body>
<h1>How many speakers do you hear?</h1>
<p class="lead">
  Fifteen clips from your own library, in random order. For each one: is it <strong>one person
  speaking</strong>, or <strong>more than one</strong>? If two or more voices ever overlap — talking at
  the same time — use the third button, because that is a different problem from simple turn-taking.
  <br /><br />
  You are not being shown the similarity score, the CAM++ label, or which group a clip came from. That
  is on purpose: knowing a clip was flagged is exactly the hint that would turn this measurement into a
  confirmation of what it expected. Answer by ear only.
</p>
<div id="list"></div>
<div id="done">
  <div class="count" id="count"></div>
  <textarea id="result" readonly placeholder="Answer every clip, then this fills in — copy it back to Claude."></textarea>
</div>
<script>
  const ITEMS = __DATA__;
  const answers = {};
  const CHOICES = [
    ['one', 'One speaker'],
    ['multi', 'More than one'],
    ['overlap', 'They overlap'],
  ];
  const list = document.getElementById('list');
  ITEMS.forEach((it, idx) => {
    const card = document.createElement('div');
    card.className = 'card';
    const n = document.createElement('div');
    n.className = 'n';
    n.textContent = `Clip ${idx + 1} of ${ITEMS.length} · ${(it.durationMs / 1000).toFixed(1)}s`;
    const audio = document.createElement('audio');
    audio.controls = true;
    audio.preload = 'none';
    audio.src = 'data:audio/wav;base64,' + it.b64;
    const row = document.createElement('div');
    row.className = 'row';
    CHOICES.forEach(([value, label]) => {
      const b = document.createElement('button');
      b.type = 'button';
      b.textContent = label;
      b.setAttribute('aria-pressed', 'false');
      b.onclick = () => {
        answers[it.id] = value;
        [...row.children].forEach((o) => o.setAttribute('aria-pressed', String(o === b)));
        card.classList.add('answered');
        render();
      };
      row.appendChild(b);
    });
    card.append(n, audio, row);
    list.appendChild(card);
  });

  function render() {
    const n = Object.keys(answers).length;
    document.getElementById('count').textContent = `${n} of ${ITEMS.length} answered`;
    if (n < ITEMS.length) { document.getElementById('result').value = ''; return; }
    // Compact, and it carries the hidden metadata back so the correlation can be computed without the
    // listener ever having seen it.
    const rows = ITEMS.map((it) => [it.id.slice(0, 8), answers[it.id], it.similarity.toFixed(3), it.band, it.camppLabel].join(','));
    document.getElementById('result').value = 'id8,answer,similarity,band,camppLabel\n' + rows.join('\n');
    document.getElementById('result').select();
  }
  render();
</script>
</body>
</html>
"""


if __name__ == "__main__":
    raise SystemExit(main())
