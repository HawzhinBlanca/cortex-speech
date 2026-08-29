import type { Page } from '@playwright/test';
import { createHash, randomUUID } from 'node:crypto';
import { readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';

const DURABLE_REVIEW_STORY_FLAG = '__cortex_e2e_durable_review_restart__';
const DURABLE_REVIEW_AUDIO_FIXTURE = fileURLToPath(
  new URL('../../src-tauri/tests/fixtures/fleurs_ckb_sample.wav', import.meta.url),
);

type DurableReviewDecision = 'accept' | 'edit' | 'reject';

interface DurableReviewSegment {
  id: string;
  audioPath: string;
  rawTranscript: string;
  normalizedTranscript: string | null;
  annotatedTranscript: string | null;
  alignmentJson: string | null;
  durationMs: number;
  speakerId: string | null;
  verified: boolean;
  humanDecision: string | null;
  verdict: string | null;
  verdictTranscript: string | null;
  correctedAt: string | null;
  escalated?: boolean;
}

interface DurableReviewRow {
  revision: number;
  segment: DurableReviewSegment;
}

type DurableReviewUndoTarget =
  | {
      kind: 'decision';
      effectEventId: number;
      segmentId: string;
      decision: DurableReviewDecision;
      sourceOperationId: string;
      sourcePayloadHash: string;
      databaseGeneration: number;
    }
  | {
      kind: 'flag';
      effectEventId: number;
      segmentId: string;
      sourceOperationId: string;
      sourcePayloadHash: string;
      priorRevision: number;
      flagRevision: number;
      flagKind: { kind: 'generic' };
      databaseGeneration: number;
    };

type DurableReviewUndoAvailability =
  | { status: 'available'; target: DurableReviewUndoTarget }
  | { status: 'none' }
  | { status: 'blocked'; reason: 'latestDecisionUndone' | 'latestFlagUndone' };

export interface DurableReviewBackendSnapshot {
  segments: Array<DurableReviewRow>;
  undoAvailability: DurableReviewUndoAvailability;
  latestCommit: {
    operationId: string;
    effectEventId: number;
    segmentId: string;
    decision: DurableReviewDecision;
    payloadHash: string;
  } | null;
  latestFlag: {
    operationId: string;
    effectEventId: number;
    segmentId: string;
    payloadHash: string;
    priorRevision: number;
    flagRevision: number;
  } | null;
  latestUndo: {
    operationId: string;
    target: DurableReviewUndoTarget;
  } | null;
  availabilityReads: number;
  committedOperationCount: number;
  flagOperationCount: number;
  undoOperationCount: number;
}

const UUID_PATTERN = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;

function cloneDurableSegment(segment: DurableReviewSegment): DurableReviewSegment {
  return { ...segment };
}

/**
 * A process-side fake for the native review store. Unlike values declared in addInitScript, this
 * closure survives page.reload(), so a restart story cannot pass by retaining Svelte/module state.
 */
function createDurableReviewBackend() {
  const rows = new Map<string, DurableReviewRow>([
    [
      'e2e-segment-1',
      {
        revision: 0,
        segment: {
          id: 'e2e-segment-1',
          audioPath: 'fleurs_ckb_sample.wav',
          rawTranscript: 'hello world',
          normalizedTranscript: 'hello world',
          annotatedTranscript: 'hello world',
          alignmentJson: null,
          durationMs: 8_220,
          speakerId: 'SPEAKER_00',
          verified: false,
          humanDecision: null,
          verdict: null,
          verdictTranscript: null,
          correctedAt: null,
        },
      },
    ],
    [
      'e2e-segment-2',
      {
        revision: 0,
        segment: {
          id: 'e2e-segment-2',
          audioPath: 'fleurs_ckb_sample.wav',
          rawTranscript: 'second durable clip',
          normalizedTranscript: 'second durable clip',
          annotatedTranscript: 'second durable clip',
          alignmentJson: null,
          durationMs: 8_220,
          speakerId: 'SPEAKER_00',
          verified: false,
          humanDecision: null,
          verdict: null,
          verdictTranscript: null,
          correctedAt: null,
        },
      },
    ],
  ]);
  const committedOperations = new Map<
    string,
    { payload: string; response: Record<string, unknown> }
  >();
  const flagOperations = new Map<string, { payload: string; response: Record<string, unknown> }>();
  const undoOperations = new Map<
    string,
    {
      payload: string;
      effectEventId: number;
      restoredRevision: number;
      effectKind: 'decision' | 'flag';
    }
  >();
  const playbackSessions = new Map<
    string,
    { segmentId: string; revision: number; mediaGrantId: string; finalized: boolean }
  >();
  const drafts = new Map<
    string,
    { segmentId: string; baseRevision: number; text: string; updatedAt: string }
  >();
  const priorByEffect = new Map<number, DurableReviewRow>();
  let nextEffectEventId = 10_001;
  let undoAvailability: DurableReviewUndoAvailability = { status: 'none' };
  let latestCommit: DurableReviewBackendSnapshot['latestCommit'] = null;
  let latestFlag: DurableReviewBackendSnapshot['latestFlag'] = null;
  let latestUndo: DurableReviewBackendSnapshot['latestUndo'] = null;
  let availabilityReads = 0;
  let audioDataUrl: Promise<string> | null = null;

  const segmentRows = () =>
    [...rows.values()].map((row) => ({
      revision: row.revision,
      segment: cloneDurableSegment(row.segment),
    }));
  const pendingRows = () =>
    segmentRows().filter((row) => !row.segment.verified && !row.segment.escalated);

  function snapshot(): DurableReviewBackendSnapshot {
    return {
      segments: segmentRows(),
      undoAvailability:
        undoAvailability.status === 'available'
          ? { status: 'available', target: { ...undoAvailability.target } }
          : { ...undoAvailability },
      latestCommit: latestCommit ? { ...latestCommit } : null,
      latestFlag: latestFlag ? { ...latestFlag } : null,
      latestUndo: latestUndo
        ? { operationId: latestUndo.operationId, target: { ...latestUndo.target } }
        : null,
      availabilityReads,
      committedOperationCount: committedOperations.size,
      flagOperationCount: flagOperations.size,
      undoOperationCount: undoOperations.size,
    };
  }

  async function invoke(action: string, rawPayload?: unknown): Promise<unknown> {
    const payload = (rawPayload ?? {}) as Record<string, unknown>;
    switch (action) {
      case 'segmentsPage': {
        const items = segmentRows().map((row) => row.segment);
        return {
          items,
          total: items.length,
          nextCursor: null,
          revisions: Object.fromEntries(segmentRows().map((row) => [row.segment.id, row.revision])),
          focusNarrowed: false,
        };
      }
      case 'reviewPage': {
        const pending = pendingRows();
        return {
          items: pending.map((row) => ({
            segment: row.segment,
            baseRevision: row.revision,
            eligible: true,
            disabledReason: null,
          })),
          total: pending.length,
          nextCursor: null,
          scopeLabel: 'pending',
          focusNarrowed: false,
        };
      }
      case 'segment': {
        const row = rows.get(String(payload.segmentId ?? ''));
        if (!row) throw new Error('Segment no longer exists');
        return cloneDurableSegment(row.segment);
      }
      case 'segments':
        return segmentRows().map((row) => row.segment);
      case 'segmentIds':
        return segmentRows().map((row) => row.segment.id);
      case 'stats': {
        const all = segmentRows();
        const verifiedCount = all.filter((row) => row.segment.verified).length;
        return {
          totalSegments: all.length,
          totalDurationSeconds: 16.44,
          avgDurationSeconds: 8.22,
          verifiedCount,
          pendingCount: all.length - verifiedCount,
          verificationRate: verifiedCount / all.length,
          uniqueSpeakers: 1,
          totalChars: all.reduce((total, row) => total + row.segment.rawTranscript.length, 0),
          avgCharsPerSegment: 17,
          durationHistogram: {
            under5s: 0,
            under10s: 2,
            under15s: 0,
            under30s: 0,
            over30s: 0,
          },
          topSpeakers: [{ speakerId: 'SPEAKER_00', segmentCount: 2, totalDurationSeconds: 16.44 }],
          reviewTiming: {
            decisionsLogged: committedOperations.size,
            medianSeconds: null,
            samples: 0,
          },
          dbSizeBytes: 4096,
        };
      }
      case 'getDraft':
        return drafts.get(String(payload.segmentId ?? '')) ?? null;
      case 'reserveDraft': {
        const operationId = String(payload.operationId ?? '');
        if (!UUID_PATTERN.test(operationId))
          throw new Error('Malformed draft reservation identity');
        return null;
      }
      case 'saveDraft': {
        const segmentId = String(payload.segmentId ?? '');
        const row = rows.get(segmentId);
        const baseRevision = Number(payload.baseRevision);
        const text = String(payload.text ?? '');
        if (!row || row.revision !== baseRevision || !text.trim()) {
          throw new Error('Malformed revision-bound review draft');
        }
        const draft = {
          segmentId,
          baseRevision,
          text,
          updatedAt: '2026-08-28T00:00:00.000Z',
        };
        drafts.set(segmentId, draft);
        return { ...draft };
      }
      case 'deleteDraft': {
        const segmentId = String(payload.segmentId ?? '');
        const baseRevision = Number(payload.baseRevision);
        const draft = drafts.get(segmentId);
        if (draft && draft.baseRevision !== baseRevision) {
          throw new Error('Stale revision-bound review draft delete');
        }
        return drafts.delete(segmentId);
      }
      case 'beginPlayback': {
        const segmentId = String(payload.segmentId ?? '');
        const revision = Number(payload.expectedRevision);
        const mediaGrantId = String(payload.mediaGrantId ?? '');
        const clientAttemptId = String(payload.clientAttemptId ?? '');
        const row = rows.get(segmentId);
        if (
          !row ||
          row.revision !== revision ||
          mediaGrantId !== 'e2e-audio-grant' ||
          !UUID_PATTERN.test(clientAttemptId)
        ) {
          throw new Error('Malformed server-bound playback authority');
        }
        const playbackReceiptId = randomUUID();
        playbackSessions.set(playbackReceiptId, {
          segmentId,
          revision,
          mediaGrantId,
          finalized: false,
        });
        return {
          playbackReceiptId,
          segmentId,
          segmentRevision: revision,
          clipDurationMs: row.segment.durationMs,
          expiresAtMs: Date.now() + 30 * 60_000,
        };
      }
      case 'finalizePlayback': {
        const playbackReceiptId = String(payload.playbackReceiptId ?? '');
        const mediaGrantId = String(payload.mediaGrantId ?? '');
        const session = playbackSessions.get(playbackReceiptId);
        const intervals = Array.isArray(payload.intervals)
          ? (payload.intervals as Array<{ startMs?: unknown; endMs?: unknown }>)
          : [];
        const uniquePlayedMs = intervals.reduce(
          (total, interval) =>
            total + Math.max(0, Number(interval.endMs ?? 0) - Number(interval.startMs ?? 0)),
          0,
        );
        const row = session ? rows.get(session.segmentId) : null;
        if (
          !session ||
          !row ||
          session.mediaGrantId !== mediaGrantId ||
          session.revision !== row.revision ||
          uniquePlayedMs < row.segment.durationMs * 0.85 ||
          uniquePlayedMs > row.segment.durationMs
        ) {
          throw new Error('Insufficient or stale server-bound playback evidence');
        }
        session.finalized = true;
        return {
          playbackReceiptId,
          segmentId: session.segmentId,
          segmentRevision: session.revision,
          uniquePlayedMs,
          clipDurationMs: row.segment.durationMs,
          coverageRatio: uniquePlayedMs / row.segment.durationMs,
        };
      }
      case 'commit': {
        const request = payload.request as Record<string, unknown> | undefined;
        const operationId = String(request?.operationId ?? '');
        const segmentId = String(request?.segmentId ?? '');
        const baseRevision = Number(request?.baseRevision);
        const decision = request?.decision as DurableReviewDecision;
        const transcript = request?.transcript;
        const playbackReceiptId = String(request?.playbackReceiptId ?? '');
        if (!request || !UUID_PATTERN.test(operationId)) {
          throw new Error('Malformed exact commit_review_v1 operation identity');
        }
        const canonical = JSON.stringify(request);
        const priorReplay = committedOperations.get(operationId);
        if (priorReplay) {
          if (priorReplay.payload !== canonical) {
            throw new Error('Reused commit operation UUID with different payload');
          }
          return { ...priorReplay.response };
        }
        const row = rows.get(segmentId);
        const playback = playbackSessions.get(playbackReceiptId);
        if (
          !row ||
          row.revision !== baseRevision ||
          (decision !== 'accept' && decision !== 'edit' && decision !== 'reject') ||
          !playback?.finalized ||
          playback.segmentId !== segmentId ||
          playback.revision !== baseRevision ||
          (decision === 'reject'
            ? transcript !== null
            : typeof transcript !== 'string' || transcript.trim().length === 0)
        ) {
          throw new Error('Malformed exact commit_review_v1 request');
        }
        const effectEventId = nextEffectEventId++;
        const decisionPayloadHash = createHash('sha256').update(canonical).digest('hex');
        priorByEffect.set(effectEventId, {
          revision: row.revision,
          segment: cloneDurableSegment(row.segment),
        });
        row.revision += 1;
        row.segment.verified = true;
        row.segment.humanDecision = decision;
        row.segment.correctedAt = '2026-08-28T00:00:01.000Z';
        row.segment.verdict = decision === 'reject' ? 'human_reject' : `human_${decision}`;
        row.segment.verdictTranscript = typeof transcript === 'string' ? transcript : null;
        row.segment.annotatedTranscript = typeof transcript === 'string' ? transcript : null;
        drafts.delete(segmentId);
        const response = {
          segmentId,
          committedRevision: row.revision,
          authoritativeTranscript:
            typeof transcript === 'string' ? transcript : row.segment.rawTranscript,
          decisionId: `effect:${effectEventId}`,
        };
        committedOperations.set(operationId, { payload: canonical, response });
        undoAvailability = {
          status: 'available',
          target: {
            kind: 'decision',
            effectEventId,
            segmentId,
            decision,
            sourceOperationId: operationId,
            sourcePayloadHash: decisionPayloadHash,
            databaseGeneration: 1,
          },
        };
        latestCommit = {
          operationId,
          effectEventId,
          segmentId,
          decision,
          payloadHash: decisionPayloadHash,
        };
        return { ...response };
      }
      case 'flag': {
        const request = payload.request as Record<string, unknown> | undefined;
        const operationId = String(request?.operationId ?? '');
        const segmentId = String(request?.segmentId ?? '');
        const baseRevision = Number(request?.baseRevision);
        const rationale = String(request?.rationale ?? '');
        if (!request || !UUID_PATTERN.test(operationId)) {
          throw new Error('Malformed exact record_review_flag operation identity');
        }
        const canonical = JSON.stringify(request);
        const replay = flagOperations.get(operationId);
        if (replay) {
          if (replay.payload !== canonical) {
            throw new Error('Reused flag operation UUID with different payload');
          }
          return { ...replay.response };
        }
        const row = rows.get(segmentId);
        if (!row || row.revision !== baseRevision || rationale.trim().length === 0) {
          throw new Error('Malformed exact record_review_flag request');
        }
        const effectEventId = nextEffectEventId++;
        const flagPayloadHash = createHash('sha256').update(canonical).digest('hex');
        priorByEffect.set(effectEventId, {
          revision: row.revision,
          segment: cloneDurableSegment(row.segment),
        });
        row.revision += 1;
        row.segment.escalated = true;
        const response = {
          effectEventId,
          segmentId,
          priorRevision: baseRevision,
          flagRevision: row.revision,
          segment: cloneDurableSegment(row.segment),
        };
        flagOperations.set(operationId, { payload: canonical, response });
        undoAvailability = {
          status: 'available',
          target: {
            kind: 'flag',
            effectEventId,
            segmentId,
            sourceOperationId: operationId,
            sourcePayloadHash: flagPayloadHash,
            priorRevision: baseRevision,
            flagRevision: row.revision,
            flagKind: { kind: 'generic' },
            databaseGeneration: 1,
          },
        };
        latestFlag = {
          operationId,
          effectEventId,
          segmentId,
          payloadHash: flagPayloadHash,
          priorRevision: baseRevision,
          flagRevision: row.revision,
        };
        return { ...response };
      }
      case 'availability':
        availabilityReads += 1;
        return undoAvailability.status === 'available'
          ? { status: 'available', target: { ...undoAvailability.target } }
          : { ...undoAvailability };
      case 'undo': {
        const request = payload.request as Record<string, unknown> | undefined;
        const operationId = String(request?.operationId ?? '');
        if (!request || !UUID_PATTERN.test(operationId)) {
          throw new Error('Malformed exact Undo operation identity');
        }
        const canonical = JSON.stringify(request);
        const replay = undoOperations.get(operationId);
        if (replay) {
          if (replay.payload !== canonical) {
            throw new Error('Reused Undo operation UUID with different payload');
          }
          return {
            status: 'alreadyApplied',
            effectKind: replay.effectKind,
            effectEventId: replay.effectEventId,
          };
        }
        const target = request.target as Record<string, unknown> | undefined;
        const requestedTarget: DurableReviewUndoTarget =
          target?.kind === 'flag'
            ? {
                kind: 'flag',
                effectEventId: Number(target.effectEventId),
                segmentId: String(target.segmentId ?? ''),
                sourceOperationId: String(target.sourceOperationId ?? ''),
                sourcePayloadHash: String(target.sourcePayloadHash ?? ''),
                priorRevision: Number(target.priorRevision),
                flagRevision: Number(target.flagRevision),
                flagKind: {
                  kind: String(
                    (target.flagKind as Record<string, unknown> | undefined)?.kind ?? '',
                  ) as 'generic',
                },
                databaseGeneration: Number(target.databaseGeneration),
              }
            : {
                kind: target?.kind as 'decision',
                effectEventId: Number(target?.effectEventId),
                segmentId: String(target?.segmentId ?? ''),
                decision: target?.decision as DurableReviewDecision,
                sourceOperationId: String(target?.sourceOperationId ?? ''),
                sourcePayloadHash: String(target?.sourcePayloadHash ?? ''),
                databaseGeneration: Number(target?.databaseGeneration),
              };
        if (
          undoAvailability.status !== 'available' ||
          JSON.stringify(undoAvailability.target) !== JSON.stringify(requestedTarget)
        ) {
          throw {
            schema: 1,
            code: 'STALE_UNDO_TARGET',
            message: 'The exact persistent E2E Undo target is no longer active.',
            retryable: false,
            suggestedAction: 'reloadClip',
          };
        }
        const row = rows.get(requestedTarget.segmentId);
        const prior = priorByEffect.get(requestedTarget.effectEventId);
        if (!row || !prior) throw new Error('Server-owned prior review truth is unavailable');
        const restoredRevision = row.revision + 1;
        row.revision = restoredRevision;
        row.segment = cloneDurableSegment(prior.segment);
        undoOperations.set(operationId, {
          payload: canonical,
          effectEventId: requestedTarget.effectEventId,
          restoredRevision,
          effectKind: requestedTarget.kind,
        });
        latestUndo = { operationId, target: { ...requestedTarget } };
        undoAvailability = {
          status: 'blocked',
          reason: requestedTarget.kind === 'flag' ? 'latestFlagUndone' : 'latestDecisionUndone',
        };
        return {
          status: 'applied',
          effectKind: requestedTarget.kind,
          effectEventId: requestedTarget.effectEventId,
          restoredRevision,
          segment: cloneDurableSegment(row.segment),
        };
      }
      case 'audioDataUrl':
        audioDataUrl ??= readFile(DURABLE_REVIEW_AUDIO_FIXTURE).then(
          (bytes) => `data:audio/wav;base64,${bytes.toString('base64')}`,
        );
        return audioDataUrl;
      case 'snapshot':
        return snapshot();
      default:
        throw new Error(`Unknown persistent review backend action: ${action}`);
    }
  }

  return { invoke };
}

const installedTauriMocks = new WeakMap<
  Page,
  Promise<ReturnType<typeof createDurableReviewBackend>>
>();

/** Minimal Tauri internals mock for Vite-only Playwright runs (no desktop shell). */
export async function installTauriMock(page: Page): Promise<void> {
  let backendPromise = installedTauriMocks.get(page);
  if (!backendPromise) {
    const durableReviewBackend = createDurableReviewBackend();
    backendPromise = page
      .exposeFunction('__cortexE2EReviewBackendInvoke', (action: string, payload?: unknown) =>
        durableReviewBackend.invoke(action, payload),
      )
      .then(() => durableReviewBackend);
    installedTauriMocks.set(page, backendPromise);
  }
  try {
    await backendPromise;
  } catch (error) {
    installedTauriMocks.delete(page);
    throw error;
  }
  await page.addInitScript(() => {
    const mockSegment = {
      id: 'e2e-segment-1',
      audioPath: 'sample.wav',
      rawTranscript: 'hello world',
      normalizedTranscript: 'hello world',
      annotatedTranscript: 'hello world',
      alignmentJson: null as string | null,
      durationMs: 1500,
      speakerId: 'SPEAKER_00' as string | null,
      verified: false,
    };

    const mockSettings = {
      model_dir: '',
      output_dir: '',
      asr_provider: 'SherpaOnnxCtc',
      // Mirror the production factory contract: the fine-tuned OmniASR-7B champion is the sole
      // automatic ASR. Smaller/cloud engines must not appear merely because the browser fixture is
      // older than the backend default.
      asr_model_size: 'WSL7B',
      multi_engine_hypotheses: false,
      use_finetuned_asr: false,
      external_asr_script_path: '/root/cortex_env/cortex_7b_client.py',
      vad_threshold: 0.5,
      min_segment_duration_ms: 3000,
      max_segment_duration_ms: 15000,
      num_asr_threads: 4,
      enable_gpu: false,
      language: 'ckb',
      export_format: 'Json',
      auto_normalize: true,
      auto_align: false,
      assign_speaker_from_filename: true,
      enable_diarization: true,
      max_speakers: 8,
      max_wer_threshold: 0.35,
      max_cer_threshold: 0.2,
      enforce_quality_gates: false,
      theme: 'Dark',
    };

    const mockQuality = {
      totalSegments: 1,
      emptyTranscriptCount: 0,
      lowConfidenceCount: 0,
      duplicateTranscriptGroups: 0,
      duplicateTranscriptSegments: 0,
      durationOutlierCount: 0,
      medianDurationMs: 1500,
      q1DurationMs: 1500,
      q3DurationMs: 1500,
      duplicateGroups: [],
      durationOutliers: [],
      annotatedSegmentCount: 1,
      meanWer: 0.0,
      meanCer: 0.0,
      segmentsAboveWerThreshold: 0,
      segmentsAboveCerThreshold: 0,
      qualityGatePassed: true,
      werOutliers: [],
    };

    // Matches the real get_dataset_certificate contract (Result<ConformalCertificate, String>,
    // always Ok — sparse data yields a heuristic, not-calibrated cert). Without this the harness's
    // default null tripped `cert.threshold` and logged a misleading console.error every run.
    const mockCertificate = {
      targetError: 0.05,
      confidenceLevel: 0.95,
      threshold: 0.35,
      totalCertified: 0,
      certifiedSegmentIds: [],
      expectedErrorBound: 0.05,
      isCalibrated: false,
    };
    const longModelId = `model-${'x'.repeat(90)}`;
    const mockAgentReport = {
      id: 'e2e-agent-report',
      agentRunId: 'e2e-agent-run',
      source: 'file',
      status: 'completed',
      summary: {
        totalSegments: 1,
        agenticReadiness: null,
        sourceReferences: [
          {
            audioFileLabel: 'podcast.wav',
            modelId: longModelId,
            audioContentHash: null,
            audioSizeBytes: 42,
            transcriptFileLabel: 'podcast.txt',
            textChars: 120,
          },
        ],
        sourceReferenceCount: 1,
        sourceReferenceRequired: false,
        requiredSourceReferenceModels: [],
        requiredSourceReferenceModelCount: 0,
        sourceReferenceModels: [longModelId],
        sourceReferenceModelCount: 1,
        sourceReferenceCoverage: [],
        sourceReferenceCoverageCount: 0,
        longFileDossiers: [],
        longFileDossierCount: 0,
        hypothesisModels: [longModelId],
        hypothesisModelCount: 1,
        hypothesisModelCounts: { [longModelId]: 1 },
        hypothesisModelKindCount: 1,
        verdictCounts: { jury_accept: 1 },
        verdictKindCount: 1,
        escalatedSegments: [],
        escalatedSegmentCount: 0,
        trainingGradeSummary: {
          totalSegments: 1,
          trainingReadySegments: 1,
          goldSegments: 1,
          silverSegments: 0,
          reviewSegments: 0,
          rejectedSegments: 0,
        },
        trainingGradeReasonCounts: { human_verified: 1 },
        trainingGradeReasonKindCount: 1,
        hypothesisCoverageBlockers: [],
        hypothesisCoverageBlockerCount: 0,
        orchestrationStages: [],
        orchestrationStageCount: 0,
      },
      errorCode: null,
      createdAt: '2026-08-28T00:00:00Z',
    };
    const emptyLibrary = () => window.localStorage.getItem('__cortex_e2e_empty_library__') === '1';
    const durableReviewStory = () =>
      window.localStorage.getItem('__cortex_e2e_durable_review_restart__') === '1';
    const invokeDurableReviewBackend = <T>(action: string, payload?: unknown): Promise<T> => {
      const invoke = (
        window as unknown as {
          __cortexE2EReviewBackendInvoke?: (action: string, payload?: unknown) => Promise<T>;
        }
      ).__cortexE2EReviewBackendInvoke;
      if (!invoke) throw new Error('Persistent review backend is unavailable');
      return invoke(action, payload);
    };
    type MockUndoDecision = 'accept' | 'edit' | 'reject';
    type MockDesktopDecisionUndoTarget = {
      kind: 'decision';
      effectEventId: number;
      segmentId: string;
      decision: MockUndoDecision;
      sourceOperationId: string;
      sourcePayloadHash: string;
      databaseGeneration: number;
    };
    type MockDesktopFlagUndoTarget = {
      kind: 'flag';
      effectEventId: number;
      segmentId: string;
      sourceOperationId: string;
      sourcePayloadHash: string;
      priorRevision: number;
      flagRevision: number;
      flagKind: { kind: 'generic' };
      databaseGeneration: number;
    };
    type MockDesktopUndoTarget = MockDesktopDecisionUndoTarget | MockDesktopFlagUndoTarget;
    type MockDesktopUndoAvailability =
      | { status: 'available'; target: MockDesktopUndoTarget }
      | { status: 'none' }
      | { status: 'blocked'; reason: 'latestDecisionUndone' | 'latestFlagUndone' };
    const uuidPattern =
      /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
    const payloadHash = 'c'.repeat(64);
    let reviewRevision = 0;
    let nextReviewEffectId = 101;
    let desktopUndoAvailability: MockDesktopUndoAvailability = { status: 'none' };
    const committedReviewOperations = new Map<
      string,
      { payload: string; response: Record<string, unknown> }
    >();
    const reviewFlagOperations = new Map<
      string,
      { payload: string; response: Record<string, unknown> }
    >();
    let appliedDesktopUndo:
      | {
          operationId: string;
          payload: string;
          effectEventId: number;
          effectKind: 'decision' | 'flag';
        }
      | undefined;
    const importRunStatuses = new Map<string, 'running' | 'settled' | 'rejected'>();
    type MockBatchOperation = 'transcribe' | 'normalize';
    type MockBatchOutcome = {
      disposition: 'completed';
      total: number;
      succeeded: number;
      failed: number;
      skipped: number;
      abandoned: number;
      cancelled: false;
      errorCode: null;
    };
    type MockBatchRun = {
      operationId: string;
      operation: MockBatchOperation;
      total: number;
      status: 'running' | 'settled';
      outcome: MockBatchOutcome | null;
      acknowledged: boolean;
    };
    const batchRuns = new Map<string, MockBatchRun>();
    let activeBatchOperationId: string | null = null;
    let hangNextImportRefresh = false;
    let releaseHungImportRefresh: (() => void) | null = null;
    let releaseDelayedReadiness: (() => void) | null = null;
    let cancelWedgedFilePicker: (() => void) | null = null;
    let cancelWedgedDirectoryPicker: (() => void) | null = null;

    let eventId = 1;
    const eventHandlers = new Map<number, (payload: unknown) => void>();
    const eventListenerIds = new Map<string, number[]>();
    const emitMockEvent = (event: string, payload: unknown) => {
      const ids = eventListenerIds.get(event) ?? [];
      for (const id of ids) eventHandlers.get(id)?.({ event, id, payload });
    };
    const settleMockBatch = (run: MockBatchRun) => {
      if (run.status !== 'running') return;
      const outcome: MockBatchOutcome = {
        disposition: 'completed',
        total: run.total,
        succeeded: run.total,
        failed: 0,
        skipped: 0,
        abandoned: 0,
        cancelled: false,
        errorCode: null,
      };
      run.status = 'settled';
      run.outcome = outcome;
      emitMockEvent('batch-progress', {
        type: 'completed',
        operationId: run.operationId,
        operation: run.operation,
        total: run.total,
        succeeded: run.total,
        failed: 0,
        skipped: 0,
        abandoned: 0,
        cancelled: false,
        error: null,
      });
      emitMockEvent('batch-worker-settled', {
        operationId: run.operationId,
        operation: run.operation,
      });
    };

    window.__TAURI_EVENT_PLUGIN_INTERNALS__ = {
      unregisterListener: (event: string, id: number) => {
        eventHandlers.delete(id);
        const ids = eventListenerIds.get(event);
        if (ids) {
          eventListenerIds.set(
            event,
            ids.filter((existing) => existing !== id),
          );
        }
      },
    };

    window.__TAURI_INTERNALS__ = {
      transformCallback: (callback: (payload: unknown) => void, _once = false) => {
        const id = eventId++;
        eventHandlers.set(id, callback);
        return id;
      },
      unregisterCallback: (id: number) => {
        eventHandlers.delete(id);
      },
      invoke: async (
        cmd: string,
        args?: {
          ids?: string[];
          speakerId?: string;
          searchQuery?: string;
          sortOrder?: string;
          id?: string;
          jobId?: string;
          audioPath?: string;
          segmentId?: string;
          expectedRevision?: number;
          baseRevision?: number;
          text?: string;
          clientAttemptId?: string;
          playbackReceiptId?: string;
          mediaGrantId?: string;
          runId?: string;
          operationId?: string;
          rationale?: string;
          request?: Record<string, unknown>;
          intervals?: Array<{ startMs?: number; endMs?: number }>;
        },
      ) => {
        switch (cmd) {
          case 'get_segments_page':
            if (durableReviewStory()) {
              return invokeDurableReviewBackend('segmentsPage');
            }
            if (
              window.localStorage.getItem('__cortex_e2e_import_completion_refresh_hangs__') === '1'
            ) {
              const reads = Number(
                window.localStorage.getItem('__cortex_e2e_import_refresh_reads__') ?? '0',
              );
              window.localStorage.setItem('__cortex_e2e_import_refresh_reads__', String(reads + 1));
            }
            if (hangNextImportRefresh) {
              hangNextImportRefresh = false;
              return new Promise((resolve) => {
                releaseHungImportRefresh = () =>
                  resolve(
                    emptyLibrary()
                      ? { items: [], total: 0, nextCursor: null }
                      : { items: [mockSegment], total: 1, nextCursor: null },
                  );
              });
            }
            return emptyLibrary()
              ? { items: [], total: 0, nextCursor: null }
              : { items: [mockSegment], total: 1, nextCursor: null };
          case 'get_review_page_v1':
            if (durableReviewStory()) {
              return invokeDurableReviewBackend('reviewPage');
            }
            return emptyLibrary()
              ? {
                  items: [],
                  total: 0,
                  nextCursor: null,
                  scopeLabel: 'pending',
                  focusNarrowed: false,
                }
              : {
                  items: [
                    {
                      segment: mockSegment,
                      baseRevision: reviewRevision,
                      eligible: true,
                      disabledReason: null,
                    },
                  ],
                  total: 1,
                  nextCursor: null,
                  scopeLabel: 'pending',
                  focusNarrowed: false,
                };
          case 'commit_review_v1': {
            if (durableReviewStory()) {
              return invokeDurableReviewBackend('commit', { request: args?.request });
            }
            const request = args?.request;
            const operationId = request?.operationId;
            const segmentId = request?.segmentId;
            const baseRevision = request?.baseRevision;
            const decision = request?.decision;
            const transcript = request?.transcript;
            const playbackReceiptId = request?.playbackReceiptId;
            if (!request || typeof operationId !== 'string' || !uuidPattern.test(operationId)) {
              throw new Error('Malformed exact commit_review_v1 E2E operation identity');
            }
            const canonical = JSON.stringify(request);
            const prior = committedReviewOperations.get(operationId);
            if (prior) {
              if (prior.payload !== canonical) {
                throw new Error('Reused commit operation UUID with different E2E payload');
              }
              return { ...prior.response };
            }
            if (
              segmentId !== mockSegment.id ||
              baseRevision !== reviewRevision ||
              (decision !== 'accept' && decision !== 'edit' && decision !== 'reject') ||
              typeof playbackReceiptId !== 'string' ||
              playbackReceiptId.length === 0 ||
              (decision === 'reject'
                ? transcript !== null
                : typeof transcript !== 'string' || transcript.trim().length === 0)
            ) {
              throw new Error('Malformed exact commit_review_v1 E2E request');
            }
            const effectEventId = nextReviewEffectId++;
            const response = {
              segmentId: mockSegment.id,
              committedRevision: reviewRevision + 1,
              authoritativeTranscript:
                typeof transcript === 'string' ? transcript : mockSegment.rawTranscript,
              decisionId: `effect:${effectEventId}`,
            };
            committedReviewOperations.set(operationId, { payload: canonical, response });
            reviewRevision += 1;
            desktopUndoAvailability = {
              status: 'available',
              target: {
                kind: 'decision',
                effectEventId,
                segmentId: mockSegment.id,
                decision,
                sourceOperationId: operationId,
                sourcePayloadHash: payloadHash,
                databaseGeneration: 1,
              },
            };
            return { ...response };
          }
          case 'get_desktop_review_undo_target_v1':
            if (durableReviewStory()) {
              return invokeDurableReviewBackend('availability');
            }
            return desktopUndoAvailability.status === 'available'
              ? {
                  status: 'available',
                  target: { ...desktopUndoAvailability.target },
                }
              : { ...desktopUndoAvailability };
          case 'undo_desktop_review_action_v1': {
            if (durableReviewStory()) {
              return invokeDurableReviewBackend('undo', { request: args?.request });
            }
            const request = args?.request;
            const operationId = request?.operationId;
            if (!request || typeof operationId !== 'string' || !uuidPattern.test(operationId)) {
              throw new Error('Malformed undo_desktop_review_action_v1 E2E operation identity');
            }
            const target = request.target as MockDesktopUndoTarget | undefined;
            if (
              !target ||
              (target.kind !== 'decision' && target.kind !== 'flag') ||
              !Number.isSafeInteger(target.effectEventId) ||
              target.effectEventId <= 0 ||
              target.segmentId !== mockSegment.id ||
              (target.kind === 'decision' &&
                target.decision !== 'accept' &&
                target.decision !== 'edit' &&
                target.decision !== 'reject') ||
              (target.kind === 'flag' &&
                (target.flagKind.kind !== 'generic' ||
                  target.flagRevision !== target.priorRevision + 1)) ||
              !uuidPattern.test(target.sourceOperationId) ||
              !/^[0-9a-f]{64}$/i.test(target.sourcePayloadHash) ||
              !Number.isSafeInteger(target.databaseGeneration) ||
              target.databaseGeneration < 0
            ) {
              throw new Error('Malformed complete desktop Undo target in E2E request');
            }
            const canonical = JSON.stringify(request);
            if (appliedDesktopUndo?.operationId === operationId) {
              if (appliedDesktopUndo.payload !== canonical) {
                throw new Error('Reused Undo operation UUID with different E2E payload');
              }
              return {
                status: 'alreadyApplied',
                effectKind: appliedDesktopUndo.effectKind,
                effectEventId: appliedDesktopUndo.effectEventId,
              };
            }
            if (
              desktopUndoAvailability.status !== 'available' ||
              JSON.stringify(desktopUndoAvailability.target) !== JSON.stringify(target)
            ) {
              throw {
                schema: 1,
                code: 'STALE_UNDO_TARGET',
                message: 'The exact E2E Undo target is no longer active.',
                retryable: false,
                suggestedAction: 'reloadClip',
              };
            }
            appliedDesktopUndo = {
              operationId,
              payload: canonical,
              effectEventId: target.effectEventId,
              effectKind: target.kind,
            };
            desktopUndoAvailability = {
              status: 'blocked',
              reason: target.kind === 'flag' ? 'latestFlagUndone' : 'latestDecisionUndone',
            };
            reviewRevision += 1;
            return {
              status: 'applied',
              effectKind: target.kind,
              effectEventId: target.effectEventId,
              restoredRevision: reviewRevision,
              segment: { ...mockSegment, verified: false },
            };
          }
          case 'get_review_draft_v1':
            if (durableReviewStory()) {
              return invokeDurableReviewBackend('getDraft', { segmentId: args?.segmentId });
            }
            return null;
          case 'reserve_review_draft_write_v1':
            if (durableReviewStory()) {
              return invokeDurableReviewBackend('reserveDraft', {
                segmentId: args?.segmentId,
                operationId: args?.operationId,
              });
            }
            return null;
          case 'save_review_draft_v1':
            if (durableReviewStory()) {
              return invokeDurableReviewBackend('saveDraft', {
                segmentId: args?.segmentId,
                baseRevision: args?.baseRevision,
                text: args?.text,
              });
            }
            return {
              segmentId: String(args?.segmentId ?? mockSegment.id),
              baseRevision: Number(args?.baseRevision ?? reviewRevision),
              text: String(args?.text ?? ''),
              updatedAt: '2026-08-28T00:00:00.000Z',
            };
          case 'delete_review_draft_v1':
            if (durableReviewStory()) {
              return invokeDurableReviewBackend('deleteDraft', {
                segmentId: args?.segmentId,
                baseRevision: args?.baseRevision,
              });
            }
            return true;
          case 'get_segment':
            if (durableReviewStory()) {
              return invokeDurableReviewBackend('segment', { segmentId: args?.segmentId });
            }
            if (emptyLibrary()) throw new Error('Segment no longer exists');
            return mockSegment;
          case 'get_segment_ids_for_view':
            if (durableReviewStory()) {
              return invokeDurableReviewBackend('segmentIds');
            }
            return emptyLibrary() ? [] : [mockSegment.id];
          case 'get_signal_anomaly_segments':
            return [];
          case 'app_health':
            // Healthy report matching the real app_health contract, so the health loop's
            // real code path runs in e2e instead of dereferencing the default null.
            return {
              db_ok: true,
              db_size_bytes: 1024,
              memory_mb: 100,
              missing_models: [],
              missing_optional_models: [],
              snapshot_last_success_epoch_secs: Math.floor(Date.now() / 1000),
              snapshot_consecutive_failures: 0,
              free_disk_bytes: 100 * 1024 ** 3,
            };
          case 'get_segments':
            if (durableReviewStory()) {
              return invokeDurableReviewBackend('segments');
            }
            return [mockSegment];
          case 'get_settings':
            return mockSettings;
          case 'get_history_status_v1':
            return { undoAction: null, redoAction: null };
          case 'get_interrupted_import':
            return window.localStorage.getItem('__cortex_e2e_interrupted_import__') === '1'
              ? {
                  id: 'e2e-interrupted-import',
                  totalFiles: 14,
                  completedCount: 7,
                  createdAt: '2026-08-28T00:00:00Z',
                }
              : null;
          case 'get_quarantine_notice':
            return window.localStorage.getItem('__cortex_e2e_quarantine_notice__') === '1'
              ? { quarantinedFileCount: 2, snapshotCount: 1, newestSnapshotSegments: 100 }
              : { quarantinedFileCount: 0, snapshotCount: 0, newestSnapshotSegments: null };
          case 'resume_interrupted_import': {
            const runId = String(args?.runId ?? '');
            importRunStatuses.set(runId, 'running');
            window.localStorage.removeItem('__cortex_e2e_interrupted_import__');
            if (window.localStorage.getItem('__cortex_e2e_resume_never_returns__') === '1') {
              setTimeout(() => importRunStatuses.set(runId, 'settled'), 25);
              return new Promise<never>(() => undefined);
            }
            return {
              status: 'started',
              resuming: true,
              importJobId: String(args?.jobId ?? 'e2e-resumed-import'),
              runId,
            };
          }
          case 'check_agentic_readiness':
            if (window.localStorage.getItem('__cortex_e2e_readiness_never_returns__') === '1') {
              return new Promise<never>(() => undefined);
            }
            if (window.localStorage.getItem('__cortex_e2e_readiness_delayed__') === '1') {
              window.localStorage.setItem('__cortex_e2e_readiness_waiting__', '1');
              return new Promise((resolve) => {
                releaseDelayedReadiness = () => {
                  window.localStorage.removeItem('__cortex_e2e_readiness_delayed__');
                  window.localStorage.removeItem('__cortex_e2e_readiness_waiting__');
                  releaseDelayedReadiness = null;
                  resolve({
                    schema: 1,
                    status: 'ready',
                    checkedAt: '2026-08-28T00:00:00Z',
                    checks: [],
                  });
                };
              });
            }
            return {
              schema: 1,
              status: 'ready',
              checkedAt: '2026-08-28T00:00:00Z',
              checks: [],
            };
          case 'list_agent_import_reports':
            return window.localStorage.getItem('__cortex_e2e_agent_report__') === '1'
              ? [mockAgentReport]
              : [];
          case 'list_agent_stage_events':
            return [];
          case 'get_agent_import_report_by_run_id':
            return window.localStorage.getItem('__cortex_e2e_agent_report__') === '1'
              ? mockAgentReport
              : null;
          case 'get_import_run_status': {
            const runId = String(args?.runId ?? '');
            return { runId, status: importRunStatuses.get(runId) ?? 'unknown' };
          }
          case 'get_active_batch_run': {
            if (!activeBatchOperationId) return null;
            const run = batchRuns.get(activeBatchOperationId);
            if (!run || run.acknowledged) return null;
            return {
              operationId: run.operationId,
              operation: run.operation,
              total: run.total,
              status: run.status,
              outcome: run.outcome,
            };
          }
          case 'get_batch_run_status': {
            const operationId = String(args?.operationId ?? '');
            const run = batchRuns.get(operationId);
            if (!run) {
              return {
                operationId,
                operation: null,
                status: 'unknown',
                total: null,
                outcome: null,
              };
            }
            return {
              operationId: run.operationId,
              operation: run.operation,
              status: run.status,
              total: run.total,
              outcome: run.outcome,
            };
          }
          case 'acknowledge_batch_run': {
            const operationId = String(args?.operationId ?? '');
            const run = batchRuns.get(operationId);
            if (!run || run.status !== 'settled') return false;
            run.acknowledged = true;
            if (activeBatchOperationId === operationId) activeBatchOperationId = null;
            return true;
          }
          case 'open_audio_file': {
            const calls = Number(
              window.localStorage.getItem('__cortex_e2e_open_audio_file_calls__') ?? '0',
            );
            window.localStorage.setItem('__cortex_e2e_open_audio_file_calls__', String(calls + 1));
            if (window.localStorage.getItem('__cortex_e2e_file_picker_timeout__') === '1') {
              window.localStorage.removeItem('__cortex_e2e_file_picker_timeout__');
              throw new Error('E_FILE_PICKER_TIMEOUT');
            }
            if (window.localStorage.getItem('__cortex_e2e_file_picker_wedged__') === '1') {
              return new Promise<never>((_resolve, reject) => {
                cancelWedgedFilePicker = () => {
                  window.localStorage.removeItem('__cortex_e2e_file_picker_wedged__');
                  cancelWedgedFilePicker = null;
                  reject(new Error('E_FILE_PICKER_CANCELLED'));
                };
              });
            }
            return 'C:\\fixtures\\podcast.wav';
          }
          case 'import_audio_file': {
            const calls = Number(
              window.localStorage.getItem('__cortex_e2e_import_audio_file_calls__') ?? '0',
            );
            window.localStorage.setItem(
              '__cortex_e2e_import_audio_file_calls__',
              String(calls + 1),
            );
            const runId = String(args?.runId ?? '');
            importRunStatuses.set(runId, 'running');
            emitMockEvent('pipeline-started', { runId, total: 1 });
            emitMockEvent('pipeline-progress', {
              runId,
              current: 1,
              total: 1,
              fileLabel: 'podcast.wav',
              status: 'processing',
            });
            queueMicrotask(() => {
              emitMockEvent('import-complete', {
                runId,
                total: 1,
                succeeded: 1,
                failed: 0,
                source: 'file',
              });
              importRunStatuses.set(runId, 'settled');
              emitMockEvent('import-worker-settled', { runId, source: 'file' });
            });
            return { status: 'started', runId };
          }
          case 'import_directory': {
            const runId = String(args?.runId ?? '');
            const calls = Number(
              window.localStorage.getItem('__cortex_e2e_import_directory_calls__') ?? '0',
            );
            window.localStorage.setItem('__cortex_e2e_import_directory_calls__', String(calls + 1));
            importRunStatuses.set(runId, 'running');
            if (window.localStorage.getItem('__cortex_e2e_directory_picker_timeout__') === '1') {
              window.localStorage.removeItem('__cortex_e2e_directory_picker_timeout__');
              importRunStatuses.set(runId, 'rejected');
              throw new Error('E_DIRECTORY_PICKER_TIMEOUT');
            }
            if (window.localStorage.getItem('__cortex_e2e_directory_picker_wedged__') === '1') {
              return new Promise<never>((_resolve, reject) => {
                cancelWedgedDirectoryPicker = () => {
                  importRunStatuses.set(runId, 'rejected');
                  window.localStorage.removeItem('__cortex_e2e_directory_picker_wedged__');
                  cancelWedgedDirectoryPicker = null;
                  reject(new Error('E_DIRECTORY_PICKER_CANCELLED'));
                };
              });
            }
            if (window.localStorage.getItem('__cortex_e2e_import_delayed_cancel__') === '1') {
              return new Promise<never>((_resolve, reject) => {
                setTimeout(() => importRunStatuses.set(runId, 'rejected'), 4_800);
                setTimeout(() => reject(new Error('E_DIRECTORY_PICKER_CANCELLED')), 5_400);
              });
            }
            if (window.localStorage.getItem('__cortex_e2e_import_cancel__') === '1') {
              importRunStatuses.set(runId, 'rejected');
              throw new Error('E_DIRECTORY_PICKER_CANCELLED');
            }
            // Deliberately emit before the command response: the renderer must already be bound to
            // the caller-created run id. A different-run event is hostile/stale and must be ignored.
            emitMockEvent('pipeline-started', { runId, total: 5 });
            emitMockEvent('pipeline-progress', {
              runId: '00000000-0000-4000-8000-0000000000ff',
              current: 99,
              total: 99,
              fileLabel: 'wrong-run.wav',
              status: 'processing',
            });
            emitMockEvent('pipeline-progress', {
              runId,
              current: 2,
              total: 5,
              fileLabel: 'podcast.wav',
              status: 'processing',
            });
            const complete = () => {
              emitMockEvent('import-complete', {
                runId,
                total: 5,
                succeeded: 5,
                failed: 0,
                source: 'directory',
              });
              importRunStatuses.set(runId, 'settled');
              emitMockEvent('import-worker-settled', { runId, source: 'directory' });
            };
            if (
              window.localStorage.getItem('__cortex_e2e_import_completion_refresh_hangs__') === '1'
            ) {
              // The worker has durably added a row. The first completion refresh hangs; settlement
              // must launch a second read that makes this row visible without another import.
              window.localStorage.removeItem('__cortex_e2e_empty_library__');
              hangNextImportRefresh = true;
              complete();
              return { status: 'started', runId };
            }
            if (window.localStorage.getItem('__cortex_e2e_import_response_lost__') === '1') {
              setTimeout(complete, 250);
              throw new Error('Mock import response channel interrupted');
            }
            if (
              window.localStorage.getItem('__cortex_e2e_import_status_only_settlement__') === '1'
            ) {
              setTimeout(() => importRunStatuses.set(runId, 'settled'), 25);
            }
            if (window.localStorage.getItem('__cortex_e2e_import_never_returns__') === '1') {
              setTimeout(() => importRunStatuses.set(runId, 'settled'), 25);
              return new Promise<never>(() => undefined);
            }
            return { status: 'started', runId };
          }
          case 'cancel_operation':
            cancelWedgedFilePicker?.();
            cancelWedgedDirectoryPicker?.();
            return null;
          case 'undo':
          case 'redo':
            return { action: null, status: { undoAction: null, redoAction: null } };
          case 'get_dataset_quality':
            return mockQuality;
          case 'get_dataset_certificate':
            return mockCertificate;
          case 'register_media_asset':
          case 'register_review_media_asset':
            return {
              id: 'e2e-audio-grant',
              expiresAt: new Date(Date.now() + 60_000).toISOString(),
            };
          case 'get_media_asset_url':
            if (durableReviewStory()) {
              return invokeDurableReviewBackend('audioDataUrl');
            }
            // Valid empty WAV. Keeping playback on a data URL exercises the successful grant path
            // without leaking test requests to the Vite server or flooding logs with expected 404s.
            return 'data:audio/wav;base64,UklGRiQAAABXQVZFZm10IBAAAAABAAEARKwAAIhYAQACABAAZGF0YQAAAAA=';
          case 'begin_desktop_playback_session_v1':
            if (durableReviewStory()) {
              return invokeDurableReviewBackend('beginPlayback', {
                segmentId: args?.segmentId,
                mediaGrantId: args?.mediaGrantId,
                expectedRevision: args?.expectedRevision,
                clientAttemptId: args?.clientAttemptId,
              });
            }
            return {
              playbackReceiptId: crypto.randomUUID(),
              segmentId: String(args?.segmentId ?? mockSegment.id),
              segmentRevision: Number(args?.expectedRevision ?? 0),
              clipDurationMs: mockSegment.durationMs,
              expiresAtMs: Date.now() + 30 * 60_000,
            };
          case 'finalize_desktop_playback_session_v1': {
            if (durableReviewStory()) {
              return invokeDurableReviewBackend('finalizePlayback', {
                playbackReceiptId: args?.playbackReceiptId,
                mediaGrantId: args?.mediaGrantId,
                intervals: args?.intervals,
              });
            }
            const uniquePlayedMs = (args?.intervals ?? []).reduce(
              (total, interval) =>
                total + Math.max(0, Number(interval.endMs ?? 0) - Number(interval.startMs ?? 0)),
              0,
            );
            return {
              playbackReceiptId: String(args?.playbackReceiptId ?? ''),
              segmentId: String(args?.segmentId ?? mockSegment.id),
              segmentRevision: Number(args?.expectedRevision ?? 0),
              uniquePlayedMs,
              clipDurationMs: mockSegment.durationMs,
              coverageRatio: Math.min(1, uniquePlayedMs / mockSegment.durationMs),
            };
          }
          case 'cancel_desktop_playback_session_v1':
            return true;
          case 'get_waveform':
            return [0.1, 0.35, 0.8, 0.4, 0.15];
          case 'get_audio_duration':
            if (durableReviewStory()) return 8.22;
            return 1.5;
          case 'get_audio_health':
            if (durableReviewStory()) {
              return { totalFiles: 2, missingFiles: 0, missingPaths: [] };
            }
            return { totalFiles: 1, missingFiles: 0, missingPaths: [] };
          case 'take_last_crash':
            return null;
          case 'get_training_grade_breakdown':
            // Match the fail-closed readiness contract. Falling through to null/object stubs makes
            // the Insights panel log an error on every E2E page load and leaves the accessibility
            // scan racing a permanently degraded render.
            return {
              summary: {
                totalSegments: 1,
                trainingReadySegments: 0,
                goldSegments: 0,
                silverSegments: 0,
                reviewSegments: 1,
                rejectedSegments: 0,
              },
              reasonCounts: { not_human_or_high_confidence_agent_verified: 1 },
            };
          case 'get_configured_providers':
            // Names only, never key values — matches the real configured_providers() contract.
            return ['gemini'];
          case 'set_api_key':
            // Echo the post-save provider-NAMES list (never a key value), like the real command.
            return ['gemini', args?.provider ?? 'openrouter'];
          case 'update_segment_metadata_v1': {
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
            const speaker = request?.changes?.find((change) => change.field === 'speakerId');
            const alignment = request?.changes?.find((change) => change.field === 'alignmentJson');
            if (
              (speaker &&
                mockSegment.speakerId !== speaker.expected &&
                mockSegment.speakerId !== speaker.value) ||
              (alignment &&
                mockSegment.alignmentJson !== alignment.expected &&
                mockSegment.alignmentJson !== alignment.value)
            ) {
              throw {
                schema: 1,
                code: 'STALE_SEGMENT_METADATA',
                message: 'The mock metadata changed. Reload it before saving.',
                retryable: false,
                suggestedAction: 'reloadClip',
              };
            }
            if (speaker) mockSegment.speakerId = speaker.value ?? null;
            if (alignment) mockSegment.alignmentJson = alignment.value ?? null;
            return {
              segmentId: request?.segmentId ?? 'seg-1',
              speakerId: mockSegment.speakerId,
              alignmentJson: mockSegment.alignmentJson,
              changed: true,
            };
          }
          case 'record_review_flag': {
            if (durableReviewStory()) {
              return invokeDurableReviewBackend('flag', { request: args?.request });
            }
            const request = args?.request as
              | {
                  operationId?: unknown;
                  segmentId?: unknown;
                  baseRevision?: unknown;
                  rationale?: unknown;
                }
              | undefined;
            const operationId = String(request?.operationId ?? '');
            const segmentId = String(request?.segmentId ?? '');
            const baseRevision = Number(request?.baseRevision);
            const rationale = String(request?.rationale ?? '');
            if (
              !uuidPattern.test(operationId) ||
              segmentId !== mockSegment.id ||
              !Number.isSafeInteger(baseRevision) ||
              baseRevision < 0 ||
              rationale.trim().length === 0
            ) {
              throw new Error('Malformed exact record_review_flag E2E request');
            }
            const payload = JSON.stringify({ segmentId, baseRevision, rationale, operationId });
            const prior = reviewFlagOperations.get(operationId);
            if (prior) {
              if (prior.payload !== payload) {
                throw {
                  schema: 1,
                  code: 'OPERATION_ID_CONFLICT',
                  message: 'This flag operation identity is bound to a different request.',
                  retryable: false,
                  suggestedAction: 'reloadClip',
                  operationId,
                };
              }
              return { ...prior.response };
            }
            if (baseRevision !== reviewRevision) {
              throw {
                schema: 1,
                code: 'STALE_REVISION',
                message: 'This clip changed; reload it before flagging it.',
                retryable: false,
                suggestedAction: 'reloadClip',
                operationId,
                details: { expectedRevision: baseRevision, currentRevision: reviewRevision },
              };
            }
            const response = {
              segmentId,
              segment: { ...mockSegment, escalated: true },
              effectEventId: nextReviewEffectId++,
              priorRevision: baseRevision,
              flagRevision: baseRevision + 1,
            };
            reviewFlagOperations.set(operationId, { payload, response });
            reviewRevision += 1;
            desktopUndoAvailability = {
              status: 'available',
              target: {
                kind: 'flag',
                effectEventId: response.effectEventId,
                segmentId,
                sourceOperationId: operationId,
                sourcePayloadHash: payloadHash,
                priorRevision: baseRevision,
                flagRevision: baseRevision + 1,
                flagKind: { kind: 'generic' },
                databaseGeneration: 1,
              },
            };
            return { ...response };
          }
          case 'couch_review_status':
            return { running: false, reviewers: [] };
          case 'start_couch_review':
            // v43 multi-reviewer: one entry PER named reviewer, each with its own token. Mirror the real
            // command by echoing the requested names, so a mock session cannot pass with a shape the
            // backend no longer returns.
            return {
              running: true,
              reviewers: ((args?.reviewers as string[] | undefined)?.length
                ? (args?.reviewers as string[])
                : ['owner']
              ).map((name, i) => ({
                name,
                url: `http://192.168.0.2:8737/?t=mock-token-${i}`,
                tailscaleUrl: `http://100.64.0.2:8737/?t=mock-token-${i}`,
              })),
            };
          case 'stop_couch_review':
            return { running: false, reviewers: [] };
          case 'reviewer_throughput':
            return []; // an ARRAY, never null - see the spot_check_report note below
          case 'revoke_couch_reviewer':
            return { running: true, reviewers: [] };
          case 'spot_check_report':
            // An ARRAY, never null. Returning null here is what took the settings dialog down: the
            // panel rendered `spotChecks.length` on it and threw mid-render.
            return [];
          case 'export_agreement_sample':
            return null; // nothing double-reviewed in a mock session — the null path the UI must handle
          case 'get_fingerprint_count':
            return 1;
          case 'get_tracing_stats':
            return { total_spans: 2, failures: 0, total_duration_ms: 12.5, avg_duration_ms: 6.25 };
          case 'get_recent_spans':
            return [
              {
                operation: 'diff.compute',
                start: '0',
                duration_ms: 5.0,
                metadata: {},
                success: true,
                error: null,
              },
              {
                operation: 'asr.transcribe',
                start: '0',
                duration_ms: 7.5,
                metadata: {},
                success: true,
                error: null,
              },
            ];
          case 'clear_tracing_spans':
            return null;
          case 'import_model_checkpoint':
            return args?.id ?? 'imported-candidate';
          case 'plugin:dialog|open':
            // Simulate the native file picker returning a chosen path.
            return '/fake/path/to/checkpoint.onnx';
          case 'list_eval_runs':
            return [];
          case 'get_escalation_rate_trend':
            return [];
          case 'get_label_quality_lift':
            return null;
          case 'run_gold_eval_asr':
          case 'run_gold_eval_local':
            return {
              run: {
                id: 'eval-run-1',
                modelId: 'omniasr-ctc-300m',
                runAt: '2026-06-25T00:00:00Z',
                numSegs: 40,
                wer: 0.6,
                cer: 0.29,
              },
              segments: [],
            };
          case 'build_scorecard':
            return { scorecard: {}, markdown: '# Scorecard\n\nmicro CER: 29.0%\n' };
          case 'create_gold_from_file':
            return 5;
          case 'save_session': {
            // Persist view-state in localStorage so a reload restores it (the real backend persists
            // to session.json). Per-context, so it never leaks across tests.
            try {
              window.localStorage.setItem(
                '__cortex_session__',
                JSON.stringify({
                  search_query: args?.searchQuery ?? '',
                  sort_order: args?.sortOrder ?? 'newest',
                }),
              );
            } catch {
              /* ignore storage failures in tests */
            }
            return null;
          }
          case 'restore_session': {
            try {
              const raw = window.localStorage.getItem('__cortex_session__');
              if (!raw) return null;
              const parsed = JSON.parse(raw) as { search_query?: string; sort_order?: string };
              return {
                search_query: parsed.search_query ?? '',
                sort_order: parsed.sort_order ?? 'newest',
                segment_count: 1,
                verified_count: 0,
              };
            } catch {
              return null;
            }
          }
          case 'list_model_versions':
            return [
              {
                id: 'omniasr-7b-champion',
                family: 'omniasr-7b',
                modelCardName: 'Pinned Kurdish champion deployment',
                checkpointSha256:
                  'a1b2c3d4e5f600112233445566778899aabbccddeeff00112233445566778899',
                source: 'owner-finetune',
                license: 'Apache-2.0',
                status: 'champion',
              },
              {
                id: 'omniasr-7b-challenger',
                family: 'omniasr-7b',
                modelCardName: null,
                checkpointSha256:
                  '00112233445566778899aabbccddeeffa1b2c3d4e5f6000000000000deadbeef',
                source: 'owner-finetune',
                license: 'Apache-2.0',
                status: 'candidate',
              },
            ];
          case 'models_status':
            return [
              {
                name: 'Silero VAD v4',
                filename: 'silero_vad_v4.onnx',
                downloaded: true,
                exists: true,
                sizeBytes: 2_000_000,
                minSizeBytes: 1_000_000,
                version: '4.0',
                source: 'bundled',
                downloadable: true,
              },
              {
                name: 'CAM++ Speaker Embedding',
                filename: 'campp/model.onnx',
                downloaded: true,
                exists: true,
                sizeBytes: 12_000_000,
                minSizeBytes: 10_000_000,
                version: '1.0',
                source: 'bundled',
                downloadable: true,
              },
              {
                name: 'AI Audio Denoiser',
                filename: 'denoiser/model.onnx',
                downloaded: true,
                exists: true,
                sizeBytes: 500_000,
                minSizeBytes: 400_000,
                version: '1.0',
                source: 'bundled',
                downloadable: true,
              },
            ];
          case 'models_download_all':
            return { downloaded: 0, failed: 0, total: 0, skipped: 0 };
          case 'get_inference_stats':
            return {
              vad: { calls: 0, failures: 0, p50_ms: 0, p99_ms: 0 },
              asr: { calls: 0, failures: 0, p50_ms: 0, p99_ms: 0 },
              model_load_ms: 0,
            };
          case 'get_dataset_stats':
            if (durableReviewStory()) {
              return invokeDurableReviewBackend('stats');
            }
            if (emptyLibrary()) {
              return {
                totalSegments: 0,
                verifiedCount: 0,
                pendingCount: 0,
                totalDurationSeconds: 0,
                verificationRate: 0,
                uniqueSpeakers: 0,
                durationHistogram: {
                  under5s: 0,
                  under10s: 0,
                  under15s: 0,
                  under30s: 0,
                  over30s: 0,
                },
                topSpeakers: [],
              };
            }
            return {
              totalSegments: 1,
              verifiedCount: 0,
              pendingCount: 1,
              totalDurationSeconds: 1.5,
              verificationRate: 0,
              uniqueSpeakers: 1,
              durationHistogram: {
                under5s: 1,
                under10s: 0,
                under15s: 0,
                under30s: 0,
                over30s: 0,
              },
              topSpeakers: [
                { speakerId: 'SPEAKER_00', segmentCount: 1, totalDurationSeconds: 1.5 },
              ],
            };
          case 'validate_dataset_cmd':
            return {
              totalSegments: 1,
              totalAudioFiles: 1,
              passed: 1,
              warnings: [],
              errors: [],
              summary: '1 segment checked — no issues',
            };
          case 'delete_segments_v1': {
            const ids = (args?.request as { ids?: string[] } | undefined)?.ids ?? [];
            return { requestedCount: ids.length, deletedCount: ids.length };
          }
          case 'get_speaker_inventory_v1':
            return [{ speakerId: 'SPEAKER_00', segmentCount: 1, totalDurationSeconds: 1.5 }];
          case 'rename_speaker_v1': {
            const request = args?.request as
              | {
                  sourceSpeakerId?: string | null;
                  targetSpeakerId?: string;
                  expectedSourceCount?: number;
                  expectedTargetCount?: number;
                }
              | undefined;
            return {
              sourceSpeakerId: request?.sourceSpeakerId ?? null,
              targetSpeakerId: request?.targetSpeakerId ?? '',
              renamedCount: request?.expectedSourceCount ?? 0,
              targetCount:
                (request?.expectedSourceCount ?? 0) + (request?.expectedTargetCount ?? 0),
              merged: (request?.expectedTargetCount ?? 0) > 0,
            };
          }
          case 'assign_speakers_v1': {
            const request = args?.request as
              { ids?: string[]; targetSpeakerId?: string | null } | undefined;
            const ids = request?.ids ?? [];
            return { requestedCount: ids.length, changedCount: ids.length, unchangedCount: 0 };
          }
          case 'export_huggingface_dataset':
            return null;
          case 'batch_verify':
          case 'rediarize_segments':
            return { status: 'started' };
          case 'batch_normalize':
          case 'batch_transcribe': {
            const operationId = String(args?.operationId ?? '');
            const operation: MockBatchOperation =
              cmd === 'batch_transcribe' ? 'transcribe' : 'normalize';
            const total = args?.ids?.length ?? 0;
            if (!operationId || total < 1 || activeBatchOperationId) {
              throw new Error('mock batch admission refused');
            }
            const run: MockBatchRun = {
              operationId,
              operation,
              total,
              status: 'running',
              outcome: null,
              acknowledged: false,
            };
            batchRuns.set(operationId, run);
            activeBatchOperationId = operationId;
            emitMockEvent('batch-progress', {
              type: 'started',
              operationId,
              operation,
              total,
            });
            queueMicrotask(() => settleMockBatch(run));
            return { status: 'started', operationId, operation };
          }
          case 'plugin:event|listen': {
            const eventName = (args as { event?: string; handler?: number } | undefined)?.event;
            const handlerId = (args as { event?: string; handler?: number } | undefined)?.handler;
            if (eventName && typeof handlerId === 'number') {
              const ids = eventListenerIds.get(eventName) ?? [];
              ids.push(handlerId);
              eventListenerIds.set(eventName, ids);
            }
            return handlerId ?? 1;
          }
          case 'plugin:event|unlisten':
            return null;
          default:
            throw new Error(`Unknown E2E Tauri mock command: ${cmd}`);
        }
      },
      // @tauri-apps/api/window reads these labels before registering the close-request handler.
      // Without them Vite E2E emits a TypeError on every mount, hiding real console regressions.
      metadata: {
        currentWindow: { label: 'main' },
        currentWebview: { windowLabel: 'main', label: 'main' },
      },
    };

    (
      window as unknown as { __emitTauriEvent?: (event: string, payload: unknown) => void }
    ).__emitTauriEvent = emitMockEvent;
    (window as unknown as { __releaseHungImportRefresh?: () => void }).__releaseHungImportRefresh =
      () => releaseHungImportRefresh?.();
    (window as unknown as { __releaseDelayedReadiness?: () => void }).__releaseDelayedReadiness =
      () => releaseDelayedReadiness?.();
  });
}

/** Emit a mocked Tauri event into the page (for progress UI tests). */
export async function emitTauriEvent(page: Page, event: string, payload: unknown): Promise<void> {
  await page.evaluate(
    ({ event, payload }) => {
      const w = window as unknown as { __emitTauriEvent?: (e: string, p: unknown) => void };
      w.__emitTauriEvent?.(event, payload);
    },
    { event, payload },
  );
}

/** Enable the two-segment process-backed review store before the first document is created. */
export async function enableDurableReviewRestartStory(page: Page): Promise<void> {
  await page.addInitScript(
    (flag) => window.localStorage.setItem(flag, '1'),
    DURABLE_REVIEW_STORY_FLAG,
  );
}

/** Read only the process-side fake backend; no renderer-local review state is included. */
export async function durableReviewBackendSnapshot(
  page: Page,
): Promise<DurableReviewBackendSnapshot> {
  return page.evaluate(async () => {
    const invoke = (
      window as unknown as {
        __cortexE2EReviewBackendInvoke?: (
          action: string,
          payload?: unknown,
        ) => Promise<DurableReviewBackendSnapshot>;
      }
    ).__cortexE2EReviewBackendInvoke;
    if (!invoke) throw new Error('Persistent review backend is unavailable');
    return invoke('snapshot');
  });
}
