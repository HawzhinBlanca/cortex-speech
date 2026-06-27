#!/usr/bin/env python3
"""build_review_page.py -- honest, self-contained Sorani review page with play buttons.

Reads a run manifest (JSONL or CSV) of speech segments and emits a SINGLE standalone HTML
file. Each segment row renders as: a bounded audio play button, the raw draft transcript
(RTL), an editable correction box, and a per-segment approve toggle, plus approve-all and
export-approved (downloads corrected JSON + CSV entirely in the browser, offline).

Honesty contract:
- Every draft is labelled RAW MACHINE OUTPUT from the named engine. The page never implies
  a transcript was human-verified; "approved" means the human reviewing THIS page approved it.
- Missing audio is shown as "audio not found", never hidden.
"""
from __future__ import annotations

import argparse
import base64
import csv
import html
import json
import sys
from pathlib import Path
from urllib.parse import quote


def file_url(p: Path) -> str:
    """A percent-encoded file:// URL. Without encoding, a path with a space (the project's own default
    Halwest dirs contain one), '#', '?', '%', or a non-ASCII Kurdish filename produces an invalid URL,
    so the WebView2/Chromium <audio> fails to load and the play button is silently dead."""
    return "file:///" + quote(p.as_posix().lstrip("/"), safe="/:")

AUDIO_KEYS = ["audio", "audio_filepath", "audio_file", "audio_path", "review_audio_file"]
TEXT_KEYS = ["text", "draft_text", "raw_transcript", "transcript", "normalized_transcript"]
ID_KEYS = ["id", "segment_id"]
ENGINE_KEYS = ["engine", "model_id", "source"]
SPEAKER_KEYS = ["speaker_id", "speaker"]

MIME = {
    ".wav": "audio/wav", ".mp3": "audio/mpeg", ".flac": "audio/flac", ".m4a": "audio/mp4",
    ".mp4": "audio/mp4", ".ogg": "audio/ogg", ".opus": "audio/ogg", ".aac": "audio/aac",
    ".webm": "audio/webm", ".wma": "audio/x-ms-wma",
}


def first(row: dict, keys: list[str], default: str = "") -> str:
    for k in keys:
        if k in row and str(row[k]).strip():
            return str(row[k]).strip()
    return default


def load_manifest(path: Path) -> list[dict]:
    if not path.is_file():
        raise FileNotFoundError(f"Manifest not found: {path}")
    suffix = path.suffix.lower()
    if suffix in (".jsonl", ".ndjson"):
        return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]
    if suffix == ".json":
        data = json.loads(path.read_text(encoding="utf-8"))
        if isinstance(data, dict):
            data = data.get("segments") or data.get("rows") or []
        if not isinstance(data, list):
            raise ValueError("JSON manifest must be a list, or have a 'segments'/'rows' list")
        return data
    if suffix in (".csv", ".tsv"):
        delim = "\t" if suffix == ".tsv" else ","
        with path.open("r", encoding="utf-8-sig", newline="") as f:
            return list(csv.DictReader(f, delimiter=delim))
    raise ValueError(f"Unsupported manifest type: {suffix} (use .jsonl, .json, .csv, or .tsv)")


def duration_seconds(row: dict) -> str:
    for k in ("duration", "duration_sec"):
        if str(row.get(k, "")).strip():
            try:
                return f"{float(row[k]):.2f}"
            except ValueError:
                return str(row[k])
    if str(row.get("duration_ms", "")).strip():
        try:
            return f"{float(row['duration_ms']) / 1000.0:.2f}"
        except ValueError:
            return ""
    return ""


def resolve_audio(raw: str, audio_root):
    if not raw:
        return None
    p = Path(raw)
    if not p.is_absolute() and audio_root is not None:
        p = audio_root / raw
    return p


def audio_src(p, embed: bool, per_clip_limit: int, budget: list):
    if p is None:
        return "", "no audio path in manifest"
    if not p.is_file():
        return "", "audio not found"
    if not embed:
        return file_url(p), ""
    size = p.stat().st_size
    if size > per_clip_limit:
        return file_url(p), f"clip too large to embed ({size // 1024} KB); linked by file path"
    if budget[0] - size < 0:
        return file_url(p), "embed budget exhausted; linked by file path"
    budget[0] -= size
    mime = MIME.get(p.suffix.lower(), "application/octet-stream")
    b64 = base64.b64encode(p.read_bytes()).decode("ascii")
    return f"data:{mime};base64,{b64}", ""


def render(rows, engine_label, source_name, embed, audio_root, per_clip_limit, total_limit):
    budget = [total_limit]
    sections = []
    export_meta = []
    embedded = 0
    missing = 0
    for idx, row in enumerate(rows, start=1):
        sid = f"seg{idx}"
        rid = first(row, ID_KEYS, default=f"row_{idx}")
        draft = first(row, TEXT_KEYS, default="")
        engine = first(row, ENGINE_KEYS, default=engine_label)
        speaker = first(row, SPEAKER_KEYS, default="")
        dur = duration_seconds(row)
        start = first(row, ["start", "start_time"], "0")
        end = first(row, ["end", "end_time"], "")
        ap = resolve_audio(first(row, AUDIO_KEYS, ""), audio_root)
        src, note = audio_src(ap, embed, per_clip_limit, budget)
        if src.startswith("data:"):
            embedded += 1
        if note in ("audio not found", "no audio path in manifest"):
            missing += 1
        meta_bits = [f"{idx:04d}", html.escape(rid)]
        if speaker:
            meta_bits.append("spk " + html.escape(speaker))
        if dur:
            meta_bits.append(html.escape(dur) + "s")
        meta_bits.append(html.escape(engine))
        head = " &middot; ".join(meta_bits)
        if src:
            player = (
                f'<button type="button" class="play" data-target="{sid}" aria-label="Play clip">&#9654; Play</button>'
                f'<audio id="{sid}" preload="none" src="{html.escape(src)}" '
                f'data-start="{html.escape(start)}" data-end="{html.escape(end)}"></audio>'
            )
        else:
            player = f'<span class="missing">[{html.escape(note)}]</span>'
        if note and src:
            player += f'<span class="note">{html.escape(note)}</span>'
        draft_html = html.escape(draft) if draft else '<span class="empty">[engine returned empty]</span>'
        sections.append(
            f"""    <section class="row" data-sid="{sid}">
      <div class="head">
        <span class="meta">{head}</span>
        <label class="approve"><input type="checkbox" class="ok" id="{sid}-ok"> approved</label>
      </div>
      <div class="player">{player}</div>
      <div class="label">raw machine draft ({html.escape(engine)}) -- not human-verified:</div>
      <div class="draft" dir="rtl">{draft_html}</div>
      <div class="label">your corrected Sorani transcript:</div>
      <textarea id="{sid}-fix" dir="rtl" spellcheck="false" placeholder="Correct {html.escape(rid)}">{html.escape(draft)}</textarea>
    </section>
"""
        )
        export_meta.append({"sid": sid, "id": rid, "engine": engine, "duration_sec": dur,
                            "audio": first(row, AUDIO_KEYS, ""), "draft": draft})
    seg_json = (
        json.dumps(export_meta, ensure_ascii=False)
        .replace("<", "\\u003c")
        .replace(">", "\\u003e")
        .replace("&", "\\u0026")
    )
    banner = (
        f"Source: {html.escape(source_name)} &middot; {len(rows)} segments &middot; "
        f"{embedded} audio embedded &middot; {missing} missing audio &middot; "
        f"drafts are RAW {html.escape(engine_label)} output (cloud engines off by default)."
    )
    return (PAGE.replace("__BANNER__", banner)
                .replace("__BODY__", "".join(sections))
                .replace("__SEGJSON__", seg_json))


PAGE = """<!doctype html>
<html lang="ckb" dir="rtl">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Cortex Speech -- segment review</title>
<style>
  :root { color-scheme: light dark; --bg:#0f1115; --panel:#171a21; --ink:#e8e8e3; --muted:#9aa0aa;
          --line:#2a2f3a; --accent:#3aa0a0; --warn:#d08a3a; --ok:#3a8f5a; }
  * { box-sizing: border-box; }
  body { margin:0; font-family: Tahoma, Arial, sans-serif; background:var(--bg); color:var(--ink); line-height:1.6; }
  header { direction:ltr; position:sticky; top:0; z-index:5; background:var(--panel); border-bottom:1px solid var(--line); padding:14px 18px; }
  header h1 { margin:0 0 4px; font-size:18px; }
  .banner { color:var(--muted); font-size:12.5px; }
  .bar { direction:ltr; display:flex; gap:10px; align-items:center; margin-top:10px; flex-wrap:wrap; }
  button { font:inherit; cursor:pointer; border:1px solid var(--line); background:#222734; color:var(--ink); border-radius:8px; padding:7px 12px; }
  button.primary { background:var(--accent); border-color:var(--accent); color:#04110f; font-weight:700; }
  .count { color:var(--muted); font-size:13px; }
  main { max-width:1100px; margin:0 auto; padding:16px; }
  .row { background:var(--panel); border:1px solid var(--line); border-radius:10px; padding:14px; margin:12px 0; }
  .head { direction:ltr; display:flex; justify-content:space-between; align-items:center; gap:12px; border-bottom:1px solid var(--line); padding-bottom:8px; margin-bottom:10px; }
  .meta { color:var(--muted); font-size:12.5px; font-family:Consolas,monospace; }
  .approve { color:var(--muted); font-size:13px; user-select:none; }
  .player { direction:ltr; display:flex; gap:10px; align-items:center; margin-bottom:8px; }
  button.play { background:var(--accent); border-color:var(--accent); color:#04110f; font-weight:700; }
  .note { color:var(--warn); font-size:12px; }
  .missing { color:var(--warn); font-size:13px; }
  .label { color:var(--muted); font-size:11.5px; direction:ltr; margin:6px 0 2px; }
  .draft { font-size:18px; white-space:pre-wrap; padding:6px 8px; border:1px dashed var(--line); border-radius:6px; }
  .empty { color:var(--warn); font-size:14px; }
  textarea { width:100%; min-height:78px; font:17px/1.6 Tahoma, Arial, sans-serif; background:#0c0e12; color:var(--ink); border:1px solid var(--line); border-radius:6px; padding:9px; resize:vertical; }
  .row.approved { border-color:var(--ok); }
  footer { color:var(--muted); font-size:12px; text-align:center; padding:18px; direction:ltr; }
</style>
</head>
<body>
<header>
  <h1>Cortex Speech -- segment review</h1>
  <div class="banner">__BANNER__</div>
  <div class="bar">
    <button type="button" class="primary" id="approve-all">Approve all</button>
    <button type="button" id="clear-all">Clear all</button>
    <button type="button" id="export-json">Export approved (JSON)</button>
    <button type="button" id="export-csv">Export approved (CSV)</button>
    <span class="count" id="count">0 approved</span>
  </div>
</header>
<main>
__BODY__
</main>
<footer>Review happens entirely in your browser -- nothing is uploaded. "Approved" means YOU approved it here.</footer>
<script>
const SEGMENTS = __SEGJSON__;
document.querySelectorAll('button.play').forEach(function (btn) {
  const audio = document.getElementById(btn.dataset.target);
  const start = parseFloat(audio.dataset.start || '0') || 0;
  const end = parseFloat(audio.dataset.end || '') || 0;
  btn.addEventListener('click', function () {
    if (audio.paused) {
      if (audio.currentTime < start || (end > 0 && audio.currentTime >= end)) { audio.currentTime = start; }
      audio.play().then(function () { btn.innerHTML = '&#10073;&#10073; Pause'; }).catch(function () { btn.innerHTML = 'play blocked'; });
    } else { audio.pause(); }
  });
  audio.addEventListener('timeupdate', function () { if (end > 0 && audio.currentTime >= end) { audio.pause(); } });
  audio.addEventListener('pause', function () { btn.innerHTML = '&#9654; Play'; });
  audio.addEventListener('play', function () { btn.innerHTML = '&#10073;&#10073; Pause'; });
});
function refresh() {
  let n = 0;
  SEGMENTS.forEach(function (s) {
    const ok = document.getElementById(s.sid + '-ok').checked;
    document.querySelector('[data-sid="' + s.sid + '"]').classList.toggle('approved', ok);
    if (ok) n++;
  });
  document.getElementById('count').textContent = n + ' / ' + SEGMENTS.length + ' approved';
}
document.querySelectorAll('input.ok').forEach(function (c) { c.addEventListener('change', refresh); });
document.getElementById('approve-all').addEventListener('click', function () { document.querySelectorAll('input.ok').forEach(function (c) { c.checked = true; }); refresh(); });
document.getElementById('clear-all').addEventListener('click', function () { document.querySelectorAll('input.ok').forEach(function (c) { c.checked = false; }); refresh(); });
function approvedRows() {
  return SEGMENTS.filter(function (s) { return document.getElementById(s.sid + '-ok').checked; }).map(function (s) {
    return { id: s.id, engine: s.engine, duration_sec: s.duration_sec, audio: s.audio, draft: s.draft, corrected: document.getElementById(s.sid + '-fix').value };
  });
}
function download(name, text, type) {
  const blob = new Blob([text], { type: type });
  const a = document.createElement('a');
  a.href = URL.createObjectURL(blob); a.download = name; a.click();
  setTimeout(function () { URL.revokeObjectURL(a.href); }, 1000);
}
document.getElementById('export-json').addEventListener('click', function () {
  const rows = approvedRows();
  if (!rows.length) { alert('No segments approved yet.'); return; }
  download('approved_corrections.json', JSON.stringify(rows, null, 2), 'application/json');
});
document.getElementById('export-csv').addEventListener('click', function () {
  const rows = approvedRows();
  if (!rows.length) { alert('No segments approved yet.'); return; }
  const esc = function (v) {
    var s = String(v == null ? '' : v);
    // Neutralize spreadsheet formula injection: a cell beginning with = + - @ (or a tab/CR) is
    // executed as a formula by Excel/LibreOffice. Prefix it with a single quote, matching the Rust
    // csv_safe_cell guard used by every other CSV writer in this project.
    if (/^[=+\\-@\\t\\r]/.test(s)) { s = "'" + s; }
    return '"' + s.replace(/"/g, '""') + '"';
  };
  const head = ['id', 'engine', 'duration_sec', 'audio', 'draft', 'corrected'];
  const lines = [head.join(',')].concat(rows.map(function (r) { return head.map(function (k) { return esc(r[k]); }).join(','); }));
  download('approved_corrections.csv', '\\ufeff' + lines.join('\\r\\n'), 'text/csv');
});
refresh();
</script>
</body>
</html>
"""


def main() -> int:
    ap = argparse.ArgumentParser(description="Build a self-contained Sorani segment review page.")
    ap.add_argument("--manifest", required=True, type=Path)
    ap.add_argument("--out", required=True, type=Path)
    ap.add_argument("--audio-root", type=Path, default=None)
    ap.add_argument("--embed-audio", action="store_true")
    ap.add_argument("--engine", default="OmniASR CTC (local)")
    ap.add_argument("--max-embed-mb", type=float, default=8.0)
    ap.add_argument("--max-total-mb", type=float, default=200.0)
    args = ap.parse_args()
    rows = load_manifest(args.manifest)
    if not rows:
        print("Manifest has no rows; nothing to review.", file=sys.stderr)
        return 2
    out = render(rows, args.engine, args.manifest.name, args.embed_audio, args.audio_root,
                 int(args.max_embed_mb * 1024 * 1024), int(args.max_total_mb * 1024 * 1024))
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(out, encoding="utf-8")
    print(f"Wrote {args.out} ({len(rows)} segments, embed={args.embed_audio})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
