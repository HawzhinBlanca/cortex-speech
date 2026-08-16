import { mount } from 'svelte';
import App from './App.svelte';
import './app.css';
import { installGlobalErrorTrap } from './lib/globalErrorTrap';

// P2.2 (audit F3): surface un-awaited promise rejections as a toast instead of letting them vanish.
// Installed BEFORE mount so it covers the whole app lifetime. Idempotent.
installGlobalErrorTrap();

// ---------------------------------------------------------------------------
// Dev-only Tauri IPC mock.
// Lets the Svelte UI render in a plain browser (no Rust backend) so the
// frontend can be previewed and iterated. Completely inert under real Tauri,
// where window.__TAURI_INTERNALS__ already exists.
// ---------------------------------------------------------------------------
if (import.meta.env.DEV && !('__TAURI_INTERNALS__' in window)) {
  // `_trend$` added 2026-08-05: `get_escalation_rate_trend` matched NOTHING here, so it fell through
  // to the final `null` and RefineryPanel's `trend.length === 0` threw "Cannot read properties of null
  // (reading 'length')" on every Insights load in dev preview. An ErrorBoundary caught it, which is
  // why it never surfaced as a page error and was easy to miss — the panel just rendered a retry
  // button. Commands that return a Vec must default to [], not null.
  const listKinds =
    /(^get_segments$|^list_|_reports$|_events$|_runs$|_history$|_trend$|^search_|_queue$|^get_speakers$)/;
  const objKinds =
    /(^get_settings$|^app_health$|^db_info$|^import_status$|readiness$|^models_status$|_info$|^get_stats$|^compute_stats$)/;

  // Sample Sorani dataset so the populated curate UI renders without a backend.
  const SAMPLE: Array<[string, number, string | null, boolean, number]> = [
    ['ئەمڕۆ هەوا زۆر خۆشە و خۆر هەڵهاتووە', 0.96, 'SPEAKER_00', true, 6200],
    ['من لە شاری هەولێر دەژیم و خوێندکارم', 0.88, 'SPEAKER_00', true, 4800],
    ['زمانی کوردی زمانێکی دەوڵەمەند و کۆنە', 0.74, 'SPEAKER_01', false, 5100],
    ['ئەو کتێبەی دوێنێ خوێندمەوە زۆر سەرنجڕاکێش بوو', 0.61, 'SPEAKER_01', false, 8300],
    ['سبەینێ بەیانی دەچینە سەر چیاکان بۆ گەشت', 0.93, 'SPEAKER_02', true, 5600],
    ['خواردنەکەی ئێوارە تامێکی تایبەتی هەبوو', 0.45, 'SPEAKER_02', false, 3900],
    ['ئەو گۆرانییەی گوێم لێبوو دەنگێکی خۆشی هەبوو', 0.81, null, false, 7200],
    ['پرۆژەکە سەرکەوتووانە تەواو بوو سوپاس بۆ هەمووان', 0.99, 'SPEAKER_00', true, 4200],
  ];
  // Dev-only: synthesize per-word timestamps + confidence from a clip's text so the
  // Review listen-strip (click-to-seek, karaoke, confidence heatmap) renders without a
  // backend aligner. A few words are pushed low/mid to exercise the colour bins.
  const makeAlignment = (text: string, durMs: number, segConf: number): string => {
    const ws = text.split(/\s+/).filter(Boolean);
    const dur = durMs / 1000;
    const per = ws.length ? dur / ws.length : dur;
    const words = ws.map((word, i) => {
      const jitter = ((i * 37) % 100) / 100; // deterministic pseudo-random in [0,1)
      let c: number;
      if (jitter < 0.18)
        c = Math.min(segConf, 0.5); // a few low → red
      else if (jitter < 0.45)
        c = Math.min(0.82, segConf); // some mid → amber
      else c = Math.min(0.99, segConf + 0.1); // most high → neutral
      return {
        word,
        start: +(i * per).toFixed(3),
        end: +((i + 1) * per).toFixed(3),
        confidence: +c.toFixed(2),
      };
    });
    return JSON.stringify({ words });
  };
  let demoSegments = SAMPLE.map(([text, conf, spk, ver, dur], i) => ({
    id: `seg_${String(i + 1).padStart(3, '0')}`,
    createdAt: `2026-06-1${i % 9}T0${i % 8}:12:00Z`,
    audioPath: `fixtures/clip_${i + 1}.wav`,
    rawTranscript: text,
    normalizedTranscript: text,
    annotatedTranscript: ver ? text : null,
    alignmentJson: makeAlignment(text, dur, conf),
    durationMs: dur,
    speakerId: spk,
    verified: ver,
    confidence: conf,
    ctcScore: conf - 0.05,
    clippingRatio: i === 5 ? 0.08 : 0.0,
    rmsDb: -18 - i,
    snrDb: i === 5 ? 6.2 : 22 - i,
    split: i % 4 === 0 ? 'test' : 'train',
    signalAnomalyScore: null,
    verdict: conf < 0.7 ? 'escalate' : 'accept',
    escalated: conf < 0.7,
    isGold: i === 0,
    alignmentQuality: ver ? 'ctc_forced' : null,
  }));
  // Return detached rows just like IPC serialization does. The backing collection remains mutable so
  // review decisions survive subsequent reads during a browser-preview session.
  const sampleSegments = () => demoSegments.map((segment) => ({ ...segment }));
  const sampleStats = () => {
    const segs = sampleSegments();
    const totalSec = segs.reduce((a, s) => a + s.durationMs / 1000, 0);
    const verified = segs.filter((s) => s.verified).length;
    return {
      totalSegments: segs.length,
      totalDurationSeconds: totalSec,
      avgDurationSeconds: totalSec / segs.length,
      verifiedCount: verified,
      pendingCount: segs.length - verified,
      verificationRate: (verified / segs.length) * 100,
      uniqueSpeakers: 3,
      totalChars: segs.reduce((a, s) => a + s.rawTranscript.length, 0),
      avgCharsPerSegment: 38,
      durationHistogram: { under5s: 3, under10s: 4, under15s: 1, under30s: 0, over30s: 0 },
      topSpeakers: [
        { speakerId: 'SPEAKER_00', segmentCount: 3, totalDurationSeconds: 15.2 },
        { speakerId: 'SPEAKER_01', segmentCount: 2, totalDurationSeconds: 13.4 },
        { speakerId: 'SPEAKER_02', segmentCount: 2, totalDurationSeconds: 9.5 },
      ],
    };
  };
  const sampleWaveform = (n = 400) =>
    Array.from({ length: n }, (_, i) => {
      const env = Math.sin((i / n) * Math.PI);
      const osc = 0.5 + 0.5 * Math.sin(i / 3.0) * Math.sin(i / 11.0);
      return Math.max(0.02, Math.min(1, env * osc));
    });

  const mockInvoke = async (cmd: string, args?: Record<string, unknown>): Promise<unknown> => {
    // Tauri's event API expects the listener id back and calls the plugin-internal unregister hook on
    // teardown. Mirroring that tiny lifecycle contract keeps Vite HMR/reloads from leaking callbacks or
    // throwing while the app is already handling another update.
    if (cmd === 'plugin:event|listen') return args?.handler ?? 0;
    if (cmd.startsWith('plugin:')) return null;
    if (cmd === 'get_segments_page') {
      const q = String(args?.query ?? '')
        .trim()
        .toLowerCase();
      const verified = typeof args?.verified === 'boolean' ? args.verified : null;
      const items = sampleSegments().filter(
        (s) =>
          (verified === null || s.verified === verified) &&
          (!q ||
            s.rawTranscript.toLowerCase().includes(q) ||
            s.audioPath.toLowerCase().includes(q) ||
            (s.speakerId ?? '').toLowerCase().includes(q)),
      );
      return { items, total: items.length, nextCursor: null };
    }
    if (cmd === 'get_segment') {
      const id = String(args?.segmentId ?? '');
      const segment = sampleSegments().find((item) => item.id === id);
      if (!segment) throw new Error(`Segment '${id}' no longer exists`);
      return segment;
    }
    if (cmd === 'update_segment_fields') {
      const id = String(args?.segmentId ?? '');
      const fields = args?.fields;
      if (!fields || typeof fields !== 'object' || Array.isArray(fields)) {
        throw new Error('update_segment_fields requires an object fields payload');
      }
      const index = demoSegments.findIndex((segment) => segment.id === id);
      if (index < 0) return false;
      demoSegments[index] = { ...demoSegments[index], ...fields };
      return true;
    }
    // Same class: the readiness verdict and the accuracy card call these, and `{}` / `null` from the
    // catch-alls crashed the Insights panel on `.summary`. Mock the real shapes so dev preview shows
    // the decision layer rather than an error.
    if (cmd === 'get_training_grade_breakdown') {
      const items = sampleSegments();
      const ready = items.filter((s) => s.verified).length;
      return {
        summary: {
          totalSegments: items.length,
          trainingReadySegments: ready,
          goldSegments: ready,
          silverSegments: 0,
          reviewSegments: items.length - ready,
          rejectedSegments: 0,
        },
        reasonCounts: {
          human_verified: ready,
          not_human_or_high_confidence_agent_verified: items.length - ready,
        },
      };
    }
    if (cmd === 'get_waveform') return sampleWaveform();
    if (cmd === 'get_audio_duration') return 6.2;
    if (cmd === 'get_stats' || cmd === 'compute_stats' || cmd === 'get_dataset_stats')
      return sampleStats();
    if (cmd === 'count_segments' || cmd === 'get_segment_count') return SAMPLE.length;
    if (cmd === 'get_speakers') return ['SPEAKER_00', 'SPEAKER_01', 'SPEAKER_02'];
    if (cmd === 'validate_dataset_cmd') {
      const segs = sampleSegments();
      const warnings = segs
        .filter((s) => s.snrDb < 10)
        .map((s) => ({
          severity: 'Warning',
          category: 'audio_quality',
          segmentId: s.id,
          field: 'snrDb',
          message: `Low SNR: ${s.snrDb} dB`,
          details: s.audioPath,
        }));
      const errors = segs
        .filter((s) => s.confidence < 0.5)
        .map((s) => ({
          severity: 'Error',
          category: 'transcript',
          segmentId: s.id,
          field: 'confidence',
          message: `Very low confidence: ${Math.round(s.confidence * 100)}%`,
          details: null,
        }));
      return {
        totalSegments: segs.length,
        totalAudioFiles: segs.length,
        passed: segs.length - errors.length,
        warnings,
        errors,
        summary: `${segs.length} segments checked · ${errors.length} error(s) · ${warnings.length} warning(s)`,
      };
    }
    if (listKinds.test(cmd)) return [];
    if (objKinds.test(cmd)) return {};
    return null;
  };
  (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {
    invoke: (cmd: string, args?: Record<string, unknown>) => mockInvoke(cmd, args),
    transformCallback: (cb: unknown) => {
      const id = Math.floor(Math.random() * 1e9);
      const w = window as unknown as Record<string, Record<number, unknown>>;
      w.__TAURI_CB__ = w.__TAURI_CB__ || {};
      w.__TAURI_CB__[id] = cb;
      return id;
    },
    metadata: {
      currentWindow: { label: 'main' },
      currentWebview: { windowLabel: 'main', label: 'main' },
    },
  };
  (
    window as unknown as Record<
      string,
      { unregisterListener?: (_event: string, id: number) => void }
    >
  ).__TAURI_EVENT_PLUGIN_INTERNALS__ = {
    unregisterListener: (_event, id) => {
      const callbacks = (window as unknown as { __TAURI_CB__?: Record<number, unknown> })
        .__TAURI_CB__;
      if (callbacks) delete callbacks[id];
    },
  };
  // eslint-disable-next-line no-console
  console.info('[cortex] dev Tauri mock installed — UI preview mode (no backend)');
}

const app = mount(App, { target: document.getElementById('app')! });

export default app;
