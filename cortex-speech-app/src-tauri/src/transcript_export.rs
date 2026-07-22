//! Plain transcript + subtitle export (TXT / SRT / VTT) — human-facing, distinct from the ML
//! dataset export in `export.rs`. Lets the owner use their transcripts as documents or subtitles.
//!
//! Subtitles (SRT/VTT) are a SINGLE-media format: cue timing comes from each segment's source
//! window. When the library spans several source files the cues are grouped and timed per file
//! (best-effort, monotonic within each file); TXT is the format for a whole multi-file library.

use crate::chunking::SegmentSourceMeta;
use crate::db::{Database, SpeechSegment};
use crate::error::AppResult;
use crate::{export, quality};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptFormat {
    Txt,
    Srt,
    Vtt,
}

impl TranscriptFormat {
    pub fn from_str_lossy(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "srt" => TranscriptFormat::Srt,
            "vtt" => TranscriptFormat::Vtt,
            _ => TranscriptFormat::Txt,
        }
    }
}

/// One timed line: source-file window [start_ms, end_ms] + optional speaker + text.
struct Cue {
    audio_path: String,
    start_ms: i64,
    end_ms: i64,
    speaker: Option<String>,
    text: String,
}

fn source_meta(seg: &SpeechSegment) -> Option<SegmentSourceMeta> {
    seg.alignment_json.as_deref().and_then(SegmentSourceMeta::from_alignment_json)
}

/// Ordered cues from the exportable segments. Timing comes from each segment's source window; a
/// segment without one (e.g. a whole-file import) falls back to a running cumulative offset PER
/// source file, so cue times stay monotonic and non-overlapping.
fn build_cues(segments: &[SpeechSegment]) -> Vec<Cue> {
    let mut ordered: Vec<&SpeechSegment> = segments.iter().collect();
    ordered.sort_by(|a, b| {
        let a_start = source_meta(a).map(|m| m.source_start_ms).unwrap_or(0);
        let b_start = source_meta(b).map(|m| m.source_start_ms).unwrap_or(0);
        a.audio_path
            .cmp(&b.audio_path)
            .then(a_start.cmp(&b_start))
            .then(a.created_at.cmp(&b.created_at))
            .then(a.id.cmp(&b.id))
    });

    let mut cues = Vec::new();
    let mut cursor_path = String::new();
    let mut cursor_ms: i64 = 0;
    for seg in ordered {
        let text = quality::effective_transcript(seg).trim().to_string();
        if text.is_empty() {
            continue;
        }
        if seg.audio_path != cursor_path {
            cursor_path = seg.audio_path.clone();
            cursor_ms = 0;
        }
        let (start_ms, end_ms) = match source_meta(seg) {
            // Honor the source position whenever it exists; a degenerate window (end <= start) keeps
            // its start and lets the formatter clamp the end. Only a segment with NO source meta at
            // all (e.g. a whole-file import) falls back to a per-file cumulative offset.
            Some(m) => {
                let start = m.source_start_ms.max(0);
                (start, m.source_end_ms.max(start))
            }
            None => {
                let start = cursor_ms;
                (start, start + seg.duration_ms.max(0))
            }
        };
        cursor_ms = end_ms;
        cues.push(Cue {
            audio_path: seg.audio_path.clone(),
            start_ms,
            end_ms,
            speaker: seg.speaker_id.clone().filter(|s| !s.trim().is_empty()),
            text,
        });
    }
    cues
}

/// "HH:MM:SS<sep>mmm" — `,` for SRT, `.` for VTT.
fn fmt_ts(ms: i64, sep: char) -> String {
    let ms = ms.max(0);
    let hours = ms / 3_600_000;
    let minutes = (ms % 3_600_000) / 60_000;
    let seconds = (ms % 60_000) / 1000;
    let millis = ms % 1000;
    format!("{hours:02}:{minutes:02}:{seconds:02}{sep}{millis:03}")
}

fn labelled(speaker: &Option<String>, text: &str) -> String {
    match speaker {
        Some(sp) => format!("{sp}: {text}"),
        None => text.to_string(),
    }
}

/// Collapse a cue body into subtitle-safe lines. SRT and VTT delimit cues with a BLANK line, so an
/// interior blank line in a transcript (a human paragraph break — `\n\n`, or Windows `\r\n\r\n`) would
/// terminate the cue early: a parser then reads the remaining text as the NEXT cue's numeric index,
/// silently dropping transcript from the exported subtitle (and throwing off every following cue). Drop
/// blank / whitespace-only lines and trim each surviving one, rejoined with a single `\n` — a multi-line
/// cue is valid, a blank line is not. `.lines()` also strips the `\r` of a CRLF, so no stray carriage
/// return survives either. Applied only to SRT/VTT; TXT is freeform and keeps the text as-is.
fn subtitle_safe_text(text: &str) -> String {
    text.lines().map(str::trim).filter(|l| !l.is_empty()).collect::<Vec<_>>().join("\n")
}

fn to_srt(cues: &[Cue]) -> String {
    let mut out = String::new();
    for (i, c) in cues.iter().enumerate() {
        // SRT requires end > start; clamp a zero/negative window to a minimal 1 ms so players accept it.
        let end = c.end_ms.max(c.start_ms + 1);
        out.push_str(&format!("{}\n", i + 1));
        out.push_str(&format!("{} --> {}\n", fmt_ts(c.start_ms, ','), fmt_ts(end, ',')));
        out.push_str(&labelled(&c.speaker, &subtitle_safe_text(&c.text)));
        out.push_str("\n\n");
    }
    out
}

fn to_vtt(cues: &[Cue]) -> String {
    let mut out = String::from("WEBVTT\n\n");
    for c in cues {
        let end = c.end_ms.max(c.start_ms + 1);
        out.push_str(&format!("{} --> {}\n", fmt_ts(c.start_ms, '.'), fmt_ts(end, '.')));
        // VTT voice span carries the speaker without polluting the caption text.
        let safe = subtitle_safe_text(&c.text);
        let line = match &c.speaker {
            Some(sp) => format!("<v {sp}>{safe}"),
            None => safe,
        };
        out.push_str(&line);
        out.push_str("\n\n");
    }
    out
}

fn to_txt(cues: &[Cue]) -> String {
    let distinct_sources: std::collections::BTreeSet<&str> = cues.iter().map(|c| c.audio_path.as_str()).collect();
    let multi = distinct_sources.len() > 1;
    let mut out = String::new();
    let mut current = String::new();
    for c in cues {
        if multi && c.audio_path != current {
            current = c.audio_path.clone();
            let name =
                std::path::Path::new(&c.audio_path).file_name().and_then(|n| n.to_str()).unwrap_or(&c.audio_path);
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&format!("# {name}\n\n"));
        }
        out.push_str(&labelled(&c.speaker, &c.text));
        out.push('\n');
    }
    out
}

/// Refuse a single-file subtitle export whose cues span MORE THAN ONE source media file. Each source's
/// cues are timed from that source's own window, so concatenating several into one SRT/VTT produces
/// timestamps that RESET to zero at every source boundary — a non-monotonic, broken subtitle track
/// (audit P1: "never concatenate multiple source timelines into one subtitle file"). TXT is the
/// whole-library format (it labels each source with a `# name` header); subtitles are per-source. Pure
/// so it is unit-testable without a database.
fn ensure_single_source_for_subtitles(cues: &[Cue], format: TranscriptFormat) -> AppResult<()> {
    if !matches!(format, TranscriptFormat::Srt | TranscriptFormat::Vtt) {
        return Ok(());
    }
    let sources: std::collections::BTreeSet<&str> = cues.iter().map(|c| c.audio_path.as_str()).collect();
    if sources.len() > 1 {
        return Err(crate::error::AppError::Validation(format!(
            "Subtitles (SRT/VTT) are per-source: this library spans {} source media files, whose \
             timelines would collide (timestamps resetting to zero) in one file. Export TXT instead for a \
             whole multi-source library (it labels each source).",
            sources.len()
        )));
    }
    Ok(())
}

/// Read the exportable segments and write the transcript/subtitle file atomically.
pub fn export_transcript(db: &Database, path: &std::path::Path, format: TranscriptFormat) -> AppResult<()> {
    // Include gold/holdout (the owner wants THEIR transcripts — this is not a training artifact), but
    // drop human-rejected clips and not-yet-transcribed placeholders: the same "not real output"
    // filter the dataset export uses, minus the training-leakage holdout exclusion.
    let segments: Vec<SpeechSegment> = db
        .get_segments(None)?
        .into_iter()
        .filter(|s| !quality::is_human_rejected(s) && !quality::is_effective_placeholder(s))
        .collect();
    let cues = build_cues(&segments);
    // Fail closed BEFORE writing: a multi-source subtitle file would ship broken (resetting) timings.
    ensure_single_source_for_subtitles(&cues, format)?;
    let body = match format {
        TranscriptFormat::Txt => to_txt(&cues),
        TranscriptFormat::Srt => to_srt(&cues),
        TranscriptFormat::Vtt => to_vtt(&cues),
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    export::write_text_atomic(path, &body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(id: &str, audio: &str, text: &str, start_ms: i64, end_ms: i64) -> SpeechSegment {
        SpeechSegment {
            id: id.to_string(),
            audio_path: audio.to_string(),
            annotated_transcript: Some(text.to_string()),
            verified: true,
            duration_ms: end_ms - start_ms,
            alignment_json: Some(
                SegmentSourceMeta { source_start_ms: start_ms, source_end_ms: end_ms, chunk_index: 0, chunk_count: 1 }
                    .to_alignment_json(),
            ),
            ..Default::default()
        }
    }

    #[test]
    fn subtitles_refuse_a_multi_source_library_but_txt_and_single_source_are_allowed() {
        // Two different source files -> a single SRT/VTT would reset timestamps at the boundary (broken).
        let multi = build_cues(&[seg("a", "one.wav", "سڵاو", 0, 1000), seg("b", "two.wav", "بەخێربێن", 0, 1000)]);
        let srt_err = ensure_single_source_for_subtitles(&multi, TranscriptFormat::Srt).unwrap_err();
        assert!(format!("{srt_err}").contains("per-source"), "SRT multi-source must be refused: {srt_err}");
        assert!(ensure_single_source_for_subtitles(&multi, TranscriptFormat::Vtt).is_err(), "VTT too");
        // TXT is the whole-library format — it labels each source, so it is allowed.
        assert!(ensure_single_source_for_subtitles(&multi, TranscriptFormat::Txt).is_ok());
        // A single source (multiple segments, same file) exports subtitles fine.
        let one = build_cues(&[seg("a", "one.wav", "سڵاو", 0, 1000), seg("c", "one.wav", "دوو", 1000, 2000)]);
        assert!(ensure_single_source_for_subtitles(&one, TranscriptFormat::Srt).is_ok());
        // Degenerate: no cues at all is not multi-source, so it does not error.
        assert!(ensure_single_source_for_subtitles(&[], TranscriptFormat::Srt).is_ok());
    }

    #[test]
    fn srt_formats_cues_with_comma_timestamps_and_1_based_index() {
        let cues = build_cues(&[seg("a", "f.wav", "سڵاو", 1000, 2500)]);
        let srt = to_srt(&cues);
        assert_eq!(srt, "1\n00:00:01,000 --> 00:00:02,500\nسڵاو\n\n");
    }

    #[test]
    fn vtt_has_header_and_dot_timestamps() {
        let cues = build_cues(&[seg("a", "f.wav", "دنیا", 65_000, 66_000)]);
        let vtt = to_vtt(&cues);
        assert!(vtt.starts_with("WEBVTT\n\n"));
        assert!(vtt.contains("00:01:05.000 --> 00:01:06.000\nدنیا\n\n"));
    }

    #[test]
    fn ordering_is_by_source_then_start_time() {
        // Deliberately out of order; export must sort by source start time.
        let cues = build_cues(&[seg("b", "f.wav", "دوو", 2000, 3000), seg("a", "f.wav", "یەک", 0, 1000)]);
        assert_eq!(cues[0].text, "یەک");
        assert_eq!(cues[1].text, "دوو");
    }

    #[test]
    fn missing_source_window_falls_back_to_cumulative_per_file() {
        let mut s =
            SpeechSegment { id: "x".into(), audio_path: "f.wav".into(), duration_ms: 1500, ..Default::default() };
        s.annotated_transcript = Some("A".into());
        let mut s2 = s.clone();
        s2.id = "y".into();
        s2.annotated_transcript = Some("B".into());
        let cues = build_cues(&[s, s2]);
        assert_eq!(cues[0].start_ms, 0);
        assert_eq!(cues[0].end_ms, 1500);
        assert_eq!(cues[1].start_ms, 1500); // second cue starts where the first ended
        assert_eq!(cues[1].end_ms, 3000);
    }

    #[test]
    fn txt_labels_speakers_and_headers_multiple_sources() {
        let mut a = seg("a", "one.wav", "hello", 0, 1000);
        a.speaker_id = Some("SPK1".into());
        let b = seg("b", "two.wav", "world", 0, 1000);
        let txt = to_txt(&build_cues(&[a, b]));
        assert!(txt.contains("# one.wav\n\nSPK1: hello\n"));
        assert!(txt.contains("# two.wav\n\nworld\n"));
    }

    #[test]
    fn empty_window_is_clamped_so_srt_end_exceeds_start() {
        let cues = build_cues(&[seg("a", "f.wav", "x", 5000, 5000)]); // end == start
        let srt = to_srt(&cues);
        assert!(srt.contains("00:00:05,000 --> 00:00:05,001"));
    }

    #[test]
    fn subtitle_cue_body_drops_interior_blank_lines_that_would_split_the_cue() {
        // A human paragraph break in the transcript ("سڵاو\n\nدنیا") must NOT terminate the cue early — a
        // blank line is the SRT/VTT cue delimiter, so leaving it in makes a parser read "دنیا" as the next
        // cue's numeric index and silently drop it. The blank line is removed; a plain multi-line cue is kept.
        let cues = build_cues(&[seg("a", "f.wav", "سڵاو\n\nدنیا", 0, 1000)]);
        let srt = to_srt(&cues);
        assert!(
            srt.contains("00:00:00,000 --> 00:00:01,000\nسڵاو\nدنیا\n\n"),
            "srt cue body has no blank line: {srt:?}"
        );
        assert!(!srt.contains("سڵاو\n\nدنیا"), "no interior blank line may survive inside a cue");
        // A Windows CRLF paragraph break is handled too: no stray \r and no blank line.
        let cues_crlf = build_cues(&[seg("b", "f.wav", "one\r\n\r\ntwo", 0, 1000)]);
        let vtt = to_vtt(&cues_crlf);
        assert!(vtt.contains("one\ntwo\n\n"), "vtt cue body collapses the CRLF blank line: {vtt:?}");
        assert!(!vtt.contains('\r'), "no stray carriage return survives in a cue");
    }

    #[test]
    fn empty_transcript_segments_are_skipped() {
        let cues = build_cues(&[seg("a", "f.wav", "   ", 0, 1000), seg("b", "f.wav", "real", 1000, 2000)]);
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].text, "real");
    }
}
