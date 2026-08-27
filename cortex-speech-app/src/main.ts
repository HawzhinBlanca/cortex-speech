import { mount } from 'svelte';
import App from './App.svelte';
import './app.css';
import { installGlobalErrorTrap } from './lib/globalErrorTrap';
import type { SpeechSegment } from './lib/types';

type DemoSpeechSegment = SpeechSegment & { confidence: number; snrDb: number };

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
  // These are deliberate preview fixtures, not regex catch-alls. A newly introduced command must
  // choose and implement an explicit preview contract or fail loudly at the final branch below.
  const emptyListCommands = new Set([
    'get_active_learning_queue',
    'get_escalation_queue',
    'get_escalation_rate_trend',
    'list_agent_import_reports',
    'list_agent_stage_events',
    'list_db_snapshots',
    'list_eval_runs',
    'list_model_versions',
  ]);
  const emptyObjectCommands = new Set([
    'app_health',
    'check_agentic_readiness',
    'db_info',
    'get_settings',
    'import_status',
    'models_status',
  ]);

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
  let demoSegments: DemoSpeechSegment[] = SAMPLE.map(([text, conf, spk, ver, dur], i) => ({
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
  const demoReviewRevisions = new Map(demoSegments.map((segment) => [segment.id, 0]));
  const demoReviewDrafts = new Map<
    string,
    { segmentId: string; baseRevision: number; text: string; updatedAt: string }
  >();
  const demoReviewOperations = new Map<
    string,
    {
      segmentId: string;
      committedRevision: number;
      authoritativeTranscript: string;
      decisionId: string;
    }
  >();
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
    if (cmd === 'get_review_page_v1') {
      const scope = args?.scope as { kind?: string; query?: string; focusId?: string } | undefined;
      const query =
        scope?.kind === 'search'
          ? String(scope.query ?? '')
              .trim()
              .toLowerCase()
          : '';
      const items = sampleSegments()
        .filter((segment) => {
          if (scope?.kind === 'pending' && segment.verified) return false;
          if (scope?.kind === 'escalation' && !segment.escalated) return false;
          return (
            !query ||
            segment.rawTranscript.toLowerCase().includes(query) ||
            segment.audioPath.toLowerCase().includes(query) ||
            (segment.speakerId ?? '').toLowerCase().includes(query)
          );
        })
        .map((segment) => ({
          segment,
          baseRevision: demoReviewRevisions.get(segment.id) ?? 0,
          eligible: segment.rawTranscript.trim().length > 0,
          disabledReason: segment.rawTranscript.trim().length > 0 ? null : 'TRANSCRIPT_NOT_READY',
        }));
      return {
        items,
        total: items.length,
        nextCursor: null,
        scopeLabel: scope?.kind ?? 'pending',
        focusNarrowed: scope?.kind === 'voiceFocus',
      };
    }
    if (cmd === 'get_review_draft_v1') {
      return demoReviewDrafts.get(String(args?.segmentId ?? '')) ?? null;
    }
    if (cmd === 'save_review_draft_v1') {
      const segmentId = String(args?.segmentId ?? '');
      const baseRevision = Number(args?.baseRevision);
      if (!demoReviewRevisions.has(segmentId)) {
        throw new Error(`Unknown preview segment: ${segmentId}`);
      }
      if (demoReviewRevisions.get(segmentId) !== baseRevision) {
        throw {
          schema: 1,
          code: 'STALE_REVIEW_DRAFT',
          message: 'The preview segment changed. Reload it before saving this draft.',
          retryable: false,
          suggestedAction: 'reloadClip',
          operationId: null,
        };
      }
      const draft = {
        segmentId,
        baseRevision,
        text: String(args?.text ?? ''),
        updatedAt: new Date().toISOString(),
      };
      demoReviewDrafts.set(segmentId, draft);
      return draft;
    }
    if (cmd === 'delete_review_draft_v1') {
      const segmentId = String(args?.segmentId ?? '');
      const baseRevision = Number(args?.baseRevision);
      const draft = demoReviewDrafts.get(segmentId);
      if (!draft || draft.baseRevision !== baseRevision) return false;
      demoReviewDrafts.delete(segmentId);
      return true;
    }
    if (cmd === 'commit_review_v1') {
      const request = args?.request as
        | {
            operationId?: string;
            segmentId?: string;
            baseRevision?: number;
            decision?: string;
            transcript?: string | null;
          }
        | undefined;
      const operationId = String(request?.operationId ?? '');
      const replay = demoReviewOperations.get(operationId);
      if (replay) return replay;
      const segmentId = String(request?.segmentId ?? '');
      const index = demoSegments.findIndex((segment) => segment.id === segmentId);
      const currentRevision = demoReviewRevisions.get(segmentId);
      if (index < 0 || currentRevision === undefined) {
        throw new Error(`Unknown preview segment: ${segmentId}`);
      }
      if (request?.baseRevision !== currentRevision) {
        throw {
          schema: 1,
          code: 'STALE_REVIEW_REVISION',
          message: 'The preview segment changed. Reload it before committing.',
          retryable: false,
          suggestedAction: 'reloadClip',
          operationId,
        };
      }
      const authoritativeTranscript =
        request.decision === 'edit'
          ? String(request.transcript ?? '')
          : demoSegments[index].rawTranscript;
      const committedRevision = currentRevision + 1;
      demoSegments[index] = {
        ...demoSegments[index],
        rawTranscript: authoritativeTranscript,
        annotatedTranscript:
          request.decision === 'accept' || request.decision === 'edit'
            ? authoritativeTranscript
            : null,
        verified: request.decision === 'accept' || request.decision === 'edit',
      };
      demoReviewRevisions.set(segmentId, committedRevision);
      const draft = demoReviewDrafts.get(segmentId);
      if (draft?.baseRevision === currentRevision) demoReviewDrafts.delete(segmentId);
      const committed = {
        segmentId,
        committedRevision,
        authoritativeTranscript,
        decisionId: `preview-${operationId}`,
      };
      demoReviewOperations.set(operationId, committed);
      return committed;
    }
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
    if (cmd === 'update_segment_metadata_v1') {
      const request = args?.request as
        | {
            segmentId?: string;
            changes?: Array<{
              field?: 'speakerId' | 'alignmentJson';
              expected?: string | null;
              value?: string | null;
            }>;
          }
        | undefined;
      const id = String(request?.segmentId ?? '');
      const index = demoSegments.findIndex((segment) => segment.id === id);
      if (index < 0) {
        throw {
          schema: 1,
          code: 'SEGMENT_NOT_FOUND',
          message: 'The selected preview segment no longer exists.',
          retryable: false,
          suggestedAction: 'reloadClip',
        };
      }
      const changes = request?.changes ?? [];
      if (changes.length === 0) throw new Error('metadata changes cannot be empty');
      const current = demoSegments[index];
      for (const change of changes) {
        const currentValue =
          change.field === 'speakerId' ? current.speakerId : current.alignmentJson;
        if (currentValue !== change.expected && currentValue !== change.value) {
          throw {
            schema: 1,
            code: 'STALE_SEGMENT_METADATA',
            message:
              'This preview metadata changed. Reload it before choosing which value to keep.',
            retryable: false,
            suggestedAction: 'reloadClip',
          };
        }
      }
      let next = current;
      for (const change of changes) {
        next =
          change.field === 'speakerId'
            ? { ...next, speakerId: change.value ?? null }
            : { ...next, alignmentJson: change.value ?? null };
      }
      demoSegments[index] = next;
      return {
        segmentId: id,
        speakerId: next.speakerId,
        alignmentJson: next.alignmentJson,
        changed:
          next.speakerId !== current.speakerId || next.alignmentJson !== current.alignmentJson,
      };
    }
    if (cmd === 'delete_segments_v1') {
      const ids = (args?.request as { ids?: string[] } | undefined)?.ids ?? [];
      if (ids.length === 0 || new Set(ids).size !== ids.length) {
        throw {
          schema: 1,
          code: 'INVALID_DELETE_REQUEST',
          message: 'The preview deletion request is invalid.',
          retryable: false,
        };
      }
      const requested = new Set(ids);
      const previousCount = demoSegments.length;
      demoSegments = demoSegments.filter((segment) => !requested.has(segment.id));
      return {
        requestedCount: ids.length,
        deletedCount: previousCount - demoSegments.length,
      };
    }
    if (cmd === 'get_speaker_inventory_v1') {
      const inventory = new Map<string | null, { segmentCount: number; durationMs: number }>();
      for (const segment of demoSegments) {
        const current = inventory.get(segment.speakerId) ?? { segmentCount: 0, durationMs: 0 };
        current.segmentCount += 1;
        current.durationMs += segment.durationMs;
        inventory.set(segment.speakerId, current);
      }
      return Array.from(inventory, ([speakerId, totals]) => ({
        speakerId,
        segmentCount: totals.segmentCount,
        totalDurationSeconds: totals.durationMs / 1000,
      })).sort((left, right) => right.segmentCount - left.segmentCount);
    }
    if (cmd === 'rename_speaker_v1') {
      const request = args?.request as
        | {
            sourceSpeakerId?: string | null;
            targetSpeakerId?: string;
            expectedSourceCount?: number;
            expectedTargetCount?: number;
          }
        | undefined;
      const sourceSpeakerId = request?.sourceSpeakerId ?? null;
      const targetSpeakerId = String(request?.targetSpeakerId ?? '');
      const sourceCount = demoSegments.filter(
        (segment) => segment.speakerId === sourceSpeakerId,
      ).length;
      const targetCount = demoSegments.filter(
        (segment) => segment.speakerId === targetSpeakerId,
      ).length;
      if (
        !targetSpeakerId ||
        sourceSpeakerId === targetSpeakerId ||
        sourceCount !== request?.expectedSourceCount ||
        targetCount !== request?.expectedTargetCount
      ) {
        throw {
          schema: 1,
          code: 'STALE_SPEAKER_INVENTORY',
          message: 'The preview speaker inventory changed.',
          retryable: false,
        };
      }
      demoSegments = demoSegments.map((segment) =>
        segment.speakerId === sourceSpeakerId
          ? { ...segment, speakerId: targetSpeakerId }
          : segment,
      );
      return {
        sourceSpeakerId,
        targetSpeakerId,
        renamedCount: sourceCount,
        targetCount: sourceCount + targetCount,
        merged: targetCount > 0,
      };
    }
    if (cmd === 'assign_speakers_v1') {
      const request = args?.request as
        { ids?: string[]; targetSpeakerId?: string | null } | undefined;
      const ids = request?.ids ?? [];
      if (ids.length === 0 || new Set(ids).size !== ids.length) {
        throw {
          schema: 1,
          code: 'INVALID_SPEAKER_ASSIGNMENT',
          message: 'The preview speaker assignment is invalid.',
          retryable: false,
        };
      }
      const requested = new Set(ids);
      let changedCount = 0;
      demoSegments = demoSegments.map((segment) => {
        if (
          !requested.has(segment.id) ||
          segment.speakerId === (request?.targetSpeakerId ?? null)
        ) {
          return segment;
        }
        changedCount += 1;
        return { ...segment, speakerId: request?.targetSpeakerId ?? null };
      });
      return {
        requestedCount: ids.length,
        changedCount,
        unchangedCount: ids.length - changedCount,
      };
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
    if (cmd === 'get_audio_health') {
      return { totalFiles: sampleSegments().length, missingFiles: 0, missingPaths: [] };
    }
    if (cmd === 'register_media_asset' || cmd === 'register_review_media_asset') {
      const audioPath = String(args?.audioPath ?? '');
      return {
        id: `preview-${audioPath}`,
        expiresAt: new Date(Date.now() + 60_000).toISOString(),
      };
    }
    if (cmd === 'get_media_asset_url') {
      return 'data:audio/wav;base64,UklGRiQAAABXQVZFZm10IBAAAAABAAEARKwAAIhYAQACABAAZGF0YQAAAAA=';
    }
    if (cmd === 'begin_desktop_playback_session_v1') {
      const segmentId = String(args?.segmentId ?? '');
      const expectedRevision = Number(args?.expectedRevision ?? 0);
      return {
        playbackReceiptId: crypto.randomUUID(),
        segmentId,
        segmentRevision: expectedRevision,
        clipDurationMs: 6200,
        expiresAtMs: Date.now() + 30 * 60_000,
      };
    }
    if (cmd === 'finalize_desktop_playback_session_v1') {
      const intervals = Array.isArray(args?.intervals)
        ? (args.intervals as Array<{ startMs?: number; endMs?: number }>)
        : [];
      const uniquePlayedMs = intervals.reduce(
        (total, interval) =>
          total + Math.max(0, Number(interval.endMs ?? 0) - Number(interval.startMs ?? 0)),
        0,
      );
      return {
        playbackReceiptId: String(args?.playbackReceiptId ?? ''),
        segmentId: 'dev-preview-segment',
        segmentRevision: 0,
        uniquePlayedMs,
        clipDurationMs: 6200,
        coverageRatio: Math.min(1, uniquePlayedMs / 6200),
      };
    }
    if (cmd === 'get_stats' || cmd === 'compute_stats' || cmd === 'get_dataset_stats')
      return sampleStats();
    if (cmd === 'get_dataset_quality') {
      const segs = sampleSegments();
      return {
        totalSegments: segs.length,
        emptyTranscriptCount: segs.filter((segment) => !segment.rawTranscript.trim()).length,
        lowConfidenceCount: segs.filter((segment) => segment.confidence < 0.5).length,
        duplicateTranscriptGroups: 0,
        duplicateTranscriptSegments: 0,
        durationOutlierCount: 0,
        medianDurationMs: 5600,
        q1DurationMs: 4200,
        q3DurationMs: 7200,
        duplicateGroups: [],
        durationOutliers: [],
        annotatedSegmentCount: segs.filter((segment) => segment.annotatedTranscript).length,
        meanWer: 0,
        meanCer: 0,
        segmentsAboveWerThreshold: 0,
        segmentsAboveCerThreshold: 0,
        qualityGatePassed: true,
        werOutliers: [],
      };
    }
    if (cmd === 'get_label_quality_lift') return null;
    if (cmd === 'get_dataset_certificate') {
      return {
        targetError: 0.05,
        confidenceLevel: 0.95,
        threshold: 0.35,
        totalCertified: 0,
        certifiedSegmentIds: [],
        expectedErrorBound: 0.05,
        isCalibrated: false,
      };
    }
    if (cmd === 'restore_session' || cmd === 'take_last_crash') return null;
    if (cmd === 'save_session' || cmd === 'update_settings') return null;
    if (cmd === 'get_history_status_v1') return { undoAction: null, redoAction: null };
    if (cmd === 'undo' || cmd === 'redo') {
      return { action: null, status: { undoAction: null, redoAction: null } };
    }
    if (cmd === 'get_configured_providers') return [];
    if (cmd === 'couch_review_status') return { running: false, reviewers: [] };
    if (cmd === 'spot_check_report' || cmd === 'reviewer_throughput') return [];
    if (cmd === 'count_segments' || cmd === 'get_segment_count') return SAMPLE.length;
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
    if (emptyListCommands.has(cmd)) return [];
    if (emptyObjectCommands.has(cmd)) return {};
    throw new Error(`Unknown development mock command: ${cmd}`);
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
