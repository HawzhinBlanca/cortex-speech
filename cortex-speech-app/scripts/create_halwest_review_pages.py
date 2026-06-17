import csv
import html
import json
from pathlib import Path
from urllib.parse import quote

from build_halwest_dataset import OUT_DIR, resolve_for_safety


def review_site() -> Path:
    return OUT_DIR / "review_pages"


def checked_audio_path(path: str | Path) -> Path:
    if not str(path).strip():
        raise ValueError("Missing audio path in review row")
    candidate = Path(path)
    if not candidate.is_absolute():
        candidate = OUT_DIR / candidate
    resolved = resolve_for_safety(candidate)
    root = resolve_for_safety(OUT_DIR)
    try:
        resolved.relative_to(root)
    except ValueError as exc:
        raise ValueError(f"Review audio path is outside dataset output: {resolved}") from exc
    if not resolved.is_file():
        raise FileNotFoundError(f"Review audio file not found: {resolved}")
    return resolved


def file_uri(path: str | Path) -> str:
    p = checked_audio_path(path)
    return "file:///" + quote(str(p).replace("\\", "/"), safe="/:")


def read_jsonl(path: Path) -> list[dict]:
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]


def read_csv(path: Path) -> list[dict]:
    with path.open("r", encoding="utf-8-sig", newline="") as f:
        return list(csv.DictReader(f))


def page_template(title: str, body: str) -> str:
    return f"""<!doctype html>
<html lang="ckb" dir="rtl">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>{html.escape(title)}</title>
  <style>
    :root {{
      color-scheme: light;
      --bg: #f7f7f4;
      --ink: #191917;
      --muted: #62625b;
      --line: #d8d7cf;
      --panel: #ffffff;
      --accent: #155f5b;
      --warn: #8a4f08;
    }}
    * {{ box-sizing: border-box; }}
    body {{
      margin: 0;
      font-family: Tahoma, Arial, sans-serif;
      background: var(--bg);
      color: var(--ink);
      line-height: 1.65;
    }}
    header {{
      direction: ltr;
      padding: 24px;
      border-bottom: 1px solid var(--line);
      background: var(--panel);
      position: sticky;
      top: 0;
      z-index: 2;
    }}
    h1 {{
      margin: 0 0 6px;
      font-size: 22px;
      letter-spacing: 0;
    }}
    .meta {{
      color: var(--muted);
      font-size: 14px;
    }}
    main {{
      max-width: 1180px;
      margin: 0 auto;
      padding: 18px;
    }}
    .row {{
      background: var(--panel);
      border: 1px solid var(--line);
      border-radius: 8px;
      padding: 14px;
      margin: 12px 0;
    }}
    .row-head {{
      direction: ltr;
      display: flex;
      gap: 12px;
      justify-content: space-between;
      align-items: center;
      color: var(--muted);
      font-size: 13px;
      border-bottom: 1px solid var(--line);
      padding-bottom: 8px;
      margin-bottom: 10px;
    }}
    audio {{
      width: 100%;
      margin: 4px 0 10px;
    }}
    .text {{
      font-size: 18px;
      white-space: pre-wrap;
    }}
    .draft {{
      color: var(--warn);
    }}
    textarea {{
      direction: rtl;
      width: 100%;
      min-height: 88px;
      font: 17px/1.6 Tahoma, Arial, sans-serif;
      border: 1px solid var(--line);
      border-radius: 6px;
      padding: 10px;
      resize: vertical;
    }}
    .copy {{
      direction: ltr;
      font-family: Consolas, monospace;
      color: var(--accent);
      font-size: 12px;
      overflow-wrap: anywhere;
    }}
  </style>
</head>
<body>
  <header>
    <h1>{html.escape(title)}</h1>
    <div class="meta">Generated from {html.escape(str(OUT_DIR))}</div>
  </header>
  <main>
{body}
  </main>
</body>
</html>
"""


def render_rows(title: str, rows: list[dict], text_key: str, audio_key: str, extra_key: str | None = None) -> str:
    parts: list[str] = []
    for idx, row in enumerate(rows, start=1):
        text = row.get(text_key, "")
        audio = row.get(audio_key, "")
        extra = row.get(extra_key, "") if extra_key else ""
        row_id = row.get("id", f"row_{idx}")
        duration = row.get("duration") or row.get("duration_sec", "")
        source = Path(row.get("source_audio", "")).name if row.get("source_audio") else ""
        parts.append(
            f"""    <section class="row">
      <div class="row-head">
        <span>{idx:04d} | {html.escape(row_id)} | {html.escape(source)} | {html.escape(str(duration))}s</span>
        <span>{html.escape(extra)}</span>
      </div>
      <audio controls preload="none" src="{file_uri(audio)}"></audio>
      <div class="text">{html.escape(text)}</div>
      <div class="copy">{html.escape(audio)}</div>
    </section>
"""
        )
    return page_template(title, "".join(parts))


def render_correction_page(rows: list[dict]) -> str:
    parts: list[str] = []
    for idx, row in enumerate(rows, start=1):
        row_id = row["id"]
        audio = row["review_audio_file"]
        draft = row.get("draft_text", "")
        corrected = row.get("corrected_text", "")
        parts.append(
            f"""    <section class="row">
      <div class="row-head">
        <span>{idx:04d} | {html.escape(row_id)} | {html.escape(row.get('duration_sec', ''))}s</span>
        <span>{html.escape(row.get('review_status', ''))}</span>
      </div>
      <audio controls preload="none" src="{file_uri(audio)}"></audio>
      <div class="draft">{html.escape(draft)}</div>
      <textarea spellcheck="false" placeholder="Corrected Sorani transcript for {html.escape(row_id)}">{html.escape(corrected)}</textarea>
      <div class="copy">{html.escape(audio)}</div>
    </section>
"""
        )
    intro = """    <section class="row" dir="ltr">
      <strong>Correction workflow</strong><br>
      Listen to each clip, type the corrected Sorani text in the box, then copy it back into
      <code>asr_draft/B7876RX/B7876RX_correction_template.csv</code> under <code>corrected_text</code>
      and set <code>review_status</code> to <code>approved</code>.
    </section>
"""
    return page_template("B7876RX Draft Correction Review", intro + "".join(parts))


def main() -> None:
    site = review_site()
    site.mkdir(parents=True, exist_ok=True)

    trusted = read_jsonl(OUT_DIR / "tts_voice_cloning" / "manifest.jsonl")
    trusted.sort(key=lambda r: r["id"])
    (site / "trusted_tts_review.html").write_text(
        render_rows("Trusted TTS Transcript Review", trusted, "text", "audio_filepath"),
        encoding="utf-8",
    )

    gold_manifest = OUT_DIR / "tts_voice_cloning_gold" / "manifest.jsonl"
    has_gold = gold_manifest.exists()
    if has_gold:
        gold = read_jsonl(gold_manifest)
        gold.sort(key=lambda r: r["id"])
        (site / "gold_tts_review.html").write_text(
            render_rows("Gold Exact-Transcript TTS Review", gold, "text", "audio_filepath", "trust_tier"),
            encoding="utf-8",
        )

    review_rows = [r for r in read_csv(OUT_DIR / "review" / "segment_review.csv") if r.get("status") == "review"]
    review_rows.sort(key=lambda r: r["id"])
    (site / "needs_review_segments.html").write_text(
        render_rows("Segments Needing Manual Review", review_rows, "text", "audio_file", "issues"),
        encoding="utf-8",
    )

    correction_path = OUT_DIR / "asr_draft" / "B7876RX" / "B7876RX_correction_template.csv"
    has_b7876_correction = correction_path.exists()
    if has_b7876_correction:
        correction = read_csv(correction_path)
        (site / "b7876_correction_review.html").write_text(
            render_correction_page(correction),
            encoding="utf-8",
        )

    gold_link = '  <li><a href="gold_tts_review.html">Gold Exact-Transcript TTS Review</a></li>' if has_gold else ""
    b7876_link = (
        '  <li><a href="b7876_correction_review.html">B7876 Draft Correction Review</a></li>'
        if has_b7876_correction
        else ""
    )
    index = f"""<!doctype html>
<html lang="en">
<head><meta charset="utf-8"><title>Halwest Review Pages</title></head>
<body>
<h1>Halwest Review Pages</h1>
<ul>
{gold_link}
  <li><a href="trusted_tts_review.html">Trusted TTS Transcript Review</a></li>
  <li><a href="needs_review_segments.html">Segments Needing Manual Review</a></li>
{b7876_link}
</ul>
<p>Dataset root: {html.escape(str(OUT_DIR))}</p>
</body>
</html>
"""
    (site / "index.html").write_text(index, encoding="utf-8")
    print(str(site / "index.html"))


if __name__ == "__main__":
    main()
