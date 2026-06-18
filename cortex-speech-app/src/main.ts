import { mount } from 'svelte';
import App from './App.svelte';
import './app.css';

// ---------------------------------------------------------------------------
// Dev-only Tauri IPC mock.
// Lets the Svelte UI render in a plain browser (no Rust backend) so the
// frontend can be previewed and iterated. Completely inert under real Tauri,
// where window.__TAURI_INTERNALS__ already exists.
// ---------------------------------------------------------------------------
if (import.meta.env.DEV && !('__TAURI_INTERNALS__' in window)) {
  const listKinds =
    /(^get_segments$|^list_|_reports$|_events$|_runs$|_history$|^search_|_queue$|^get_speakers$)/;
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
  const sampleSegments = () =>
    SAMPLE.map(([text, conf, spk, ver, dur], i) => ({
      id: `seg_${String(i + 1).padStart(3, '0')}`,
      createdAt: `2026-06-1${i % 9}T0${i % 8}:12:00Z`,
      audioPath: `fixtures/clip_${i + 1}.wav`,
      rawTranscript: text,
      normalizedTranscript: text,
      annotatedTranscript: ver ? text : null,
      alignmentJson: null,
      durationMs: dur,
      speakerId: spk,
      verified: ver,
      confidence: conf,
      ctcScore: conf - 0.05,
      clippingRatio: i === 5 ? 0.08 : 0.0,
      rmsDb: -18 - i,
      snrDb: i === 5 ? 6.2 : 22 - i,
      split: i % 4 === 0 ? 'test' : 'train',
      oodScore: null,
      verdict: conf < 0.7 ? 'escalate' : 'accept',
      escalated: conf < 0.7,
      isGold: i === 0,
      alignmentQuality: ver ? 'ctc_forced' : null,
    }));
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
      verificationRate: verified / segs.length,
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

  const mockInvoke = async (cmd: string): Promise<unknown> => {
    if (cmd.startsWith('plugin:')) return null;
    if (cmd === 'get_segments') return sampleSegments();
    if (cmd === 'get_waveform') return sampleWaveform();
    if (cmd === 'get_audio_duration') return 6.2;
    if (cmd === 'get_stats' || cmd === 'compute_stats') return sampleStats();
    if (cmd === 'count_segments' || cmd === 'get_segment_count') return SAMPLE.length;
    if (cmd === 'get_speakers') return ['SPEAKER_00', 'SPEAKER_01', 'SPEAKER_02'];
    if (listKinds.test(cmd)) return [];
    if (objKinds.test(cmd)) return {};
    return null;
  };
  (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {
    invoke: (cmd: string) => mockInvoke(cmd),
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
  // eslint-disable-next-line no-console
  console.info('[cortex] dev Tauri mock installed — UI preview mode (no backend)');
}

const app = mount(App, { target: document.getElementById('app')! });

export default app;
