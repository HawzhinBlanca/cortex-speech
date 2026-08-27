import { invokeCritical, invokeLegacy } from './adapters/legacyIpc';
import { commands as generatedCommands } from './generated/ipc';
import { formatUnknownError } from './errorText';
import type {
  ActiveVoiceFocusV1,
  AssignedSpeakersV1,
  AssignSpeakersRequestV1,
  AppHealthV1,
  CommandErrorV1,
  DeletedSegmentsV1,
  CommitReviewRequestV1,
  CommittedReviewV1,
  DesktopPlaybackReceiptV1,
  DesktopPlaybackSessionV1,
  HistoryMutationResultV1,
  HistoryStatusV1,
  MarkedSegmentUnusableV1,
  MarkSegmentUnusableRequestV1,
  InferenceStatsV1,
  PlaybackIntervalV1,
  RenamedSpeakerV1,
  RenameSpeakerRequestV1,
  ReviewDraftV1,
  ReviewPageV1,
  ReviewScope,
  SegmentMetadataChangeV1,
  SettingsPatchResultV1,
  SettingsPatchV1,
  SettingsSnapshotV1,
  SpeakerInventoryItemV1,
  TextDiff,
  TracingSpanV1,
  TracingStatsV1,
  UpdatedSegmentMetadataV1,
} from './generated/ipc';
export type {
  ActiveVoiceFocusV1,
  AppHealthV1 as AppHealth,
  DesktopPlaybackReceiptV1,
  DesktopPlaybackSessionV1,
  HistoryActionV1,
  HistoryMutationResultV1,
  HistoryStatusV1,
  MarkedSegmentUnusableV1,
  MarkSegmentUnusableRequestV1,
  InferenceStatsV1 as InferenceStats,
  PlaybackIntervalV1,
  ReviewDraftV1,
  ReviewPageV1,
  SpeakerInventoryItemV1,
  TechnicalUnusableReasonV1,
  TracingSpanV1 as TracingSpan,
  TracingStatsV1 as TracingStats,
} from './generated/ipc';
import type { SpeechSegment, SegmentsPage, WordTimestamp, DatasetStats } from './types';
import type { AppSettings } from './stores/settingsStore';
import {
  mapBackendToFrontend,
  mapFrontendToBackend,
  type BackendSettings,
} from './settingsAdapter';

export async function openAudioFile(): Promise<string | null> {
  return invokeCritical('open_audio_file');
}

export async function importDirectory(): Promise<{ status: string }> {
  return invokeCritical('import_directory');
}

/** P3.2: a directory import interrupted by a crash, offered for resume at startup. */
export interface ImportJob {
  id: string;
  dir: string;
  totalFiles: number;
  completedPaths: string[];
  createdAt: string;
}

export async function getInterruptedImport(): Promise<ImportJob | null> {
  return invokeCritical('get_interrupted_import');
}

export async function resumeInterruptedImport(): Promise<{ status: string; resuming: boolean }> {
  return invokeCritical('resume_interrupted_import');
}

export async function discardInterruptedImport(jobId: string): Promise<void> {
  return invokeCritical('discard_interrupted_import', { jobId });
}

export async function importAudioFile(path: string): Promise<{ status: string; source?: string }> {
  return invokeCritical('import_audio_file', { path });
}

export async function cancelOperation(): Promise<void> {
  const result = await generatedCommands.cancelOperation();
  if (result.status === 'error') throw result.error;
}

/**
 * Sentinel the backend embeds (pipeline.rs `ASR_7B_UNAVAILABLE_TAG`) in every error that means
 * "the OmniASR-7B champion is the selected engine but it is unavailable / failed". When a transcribe
 * call rejects carrying this token, the UI offers a champion retry. Optional engines require an
 * explicit non-champion selection in Settings; the production path never downgrades after failure.
 * Keep in sync with the Rust constant.
 */
export const ASR_7B_UNAVAILABLE_TAG = 'E_ASR_7B_UNAVAILABLE';

/** True when a transcription error is the "7B champion unavailable/failed" signal above. */
export function is7bUnavailableError(e: unknown): boolean {
  const msg = typeof e === 'string' ? e : ((e as { message?: unknown } | null)?.message ?? '');
  return (
    formatUnknownError(msg, '').includes(ASR_7B_UNAVAILABLE_TAG) ||
    formatUnknownError(e, '').includes(ASR_7B_UNAVAILABLE_TAG)
  );
}

export async function transcribeSegment(
  audioPath: string,
  alignmentJson?: string | null,
  segmentId?: string | null,
): Promise<{
  text: string;
  rawTranscript: string;
  confidence?: number | null;
  confidenceSource?: string | null;
  modelVersionId?: string | null;
  cloudCall?: boolean;
}> {
  return invokeLegacy<{
    text: string;
    rawTranscript: string;
    confidence?: number | null;
    confidenceSource?: string | null;
    modelVersionId?: string | null;
    cloudCall?: boolean;
  }>('transcribe_segment', {
    segmentId: segmentId ?? null,
    audioPath,
    alignmentJson: alignmentJson ?? null,
  });
}

export async function batchTranscribe(ids: string[]): Promise<{ status: string }> {
  return invokeLegacy<{ status: string }>('batch_transcribe', { ids });
}

export async function normalizeText(text: string): Promise<string> {
  const result = await generatedCommands.normalizeText(text);
  if (result.status === 'error') throw result.error;
  return result.data;
}

export async function alignSegment(
  audioPath: string,
  text: string,
  alignmentJson?: string | null,
  segmentId?: string | null,
): Promise<WordTimestamp[]> {
  return invokeLegacy<WordTimestamp[]>('align_segment', {
    audioPath,
    text,
    alignmentJson: alignmentJson ?? null,
    segmentId: segmentId ?? null,
  });
}

export interface ConsensusWord {
  text: string;
  agreement: number;
  modelsAgreeing: number;
  totalModels: number;
  alternatives: string[];
}

export interface SegmentConsensus {
  draft: string;
  words: ConsensusWord[];
  modelCount: number;
  minAgreement: number;
  meanAgreement: number;
  /** Distinct engine ids that produced this segment's hypotheses, recorded (never inferred). */
  models: string[];
}

/** Render read-only historical provenance. These labels are not selectable engines; unknown ids show
 * verbatim (never invented) so the review badge always names exactly what produced the stored draft. */
export function engineLabel(modelId: string): string {
  const id = (modelId || '').toLowerCase();
  if (id.includes('wsl-7b') || id.includes('omniasr-7b') || id === 'omniasr-llm-7b')
    return 'OmniASR-7B Champion';
  if (id.includes('finetuned') || id.includes('mms-ckb') || id.includes('mms_ctc'))
    return 'Fine-tuned MMS-1B';
  if (id.includes('ctc-1b') || id.includes('ctc_1b')) return 'OmniASR-CTC 1B (base)';
  if (id.includes('ctc-300m') || id.includes('ctc_300m')) return 'OmniASR-CTC 300M (base)';
  if (id.startsWith('unknown@') || id === 'unknown') return 'unknown (pre-registry)';
  return modelId;
}

/** Offline best-of-N consensus draft for a segment (ability-weighted vote over its ASR hypotheses). */
export async function getSegmentConsensus(segmentId: string): Promise<SegmentConsensus> {
  return invokeLegacy<SegmentConsensus>('get_segment_consensus', { segmentId });
}

export interface GetSegmentsPageOptions {
  verified?: boolean | null;
  query?: string | null;
  sort?: string;
  limit?: number;
  cursor?: string | null;
  /** Apply the active voice-focus allow-list (review queue only — the library stays unfocused). */
  focused?: boolean;
}

function isSpeechSegmentPayload(value: unknown): value is SpeechSegment {
  if (!value || typeof value !== 'object') return false;
  const segment = value as Partial<SpeechSegment>;
  return (
    typeof segment.id === 'string' &&
    typeof segment.audioPath === 'string' &&
    typeof segment.rawTranscript === 'string' &&
    typeof segment.durationMs === 'number' &&
    Number.isFinite(segment.durationMs) &&
    typeof segment.verified === 'boolean'
  );
}

function isSegmentsPagePayload(value: unknown): value is SegmentsPage {
  if (!value || typeof value !== 'object') return false;
  const page = value as Partial<SegmentsPage>;
  if (
    !Array.isArray(page.items) ||
    !page.items.every(isSpeechSegmentPayload) ||
    typeof page.total !== 'number' ||
    !Number.isSafeInteger(page.total) ||
    page.total < 0 ||
    page.total < page.items.length ||
    (page.nextCursor !== null && typeof page.nextCursor !== 'string') ||
    (page.focusNarrowed !== undefined && typeof page.focusNarrowed !== 'boolean')
  ) {
    return false;
  }
  if (page.revisions === undefined) return true;
  return (
    !!page.revisions &&
    typeof page.revisions === 'object' &&
    !Array.isArray(page.revisions) &&
    Object.values(page.revisions).every(
      (revision) => Number.isSafeInteger(revision) && revision >= 0,
    )
  );
}

export async function getSegment(segmentId: string): Promise<SpeechSegment> {
  const result = await generatedCommands.getSegment(segmentId);
  if (result.status === 'error') throw result.error;
  const data = result.data;
  if (!isSpeechSegmentPayload(data)) {
    throw new Error('get_segment returned an invalid payload');
  }
  return data;
}

export async function getSegmentsPage(options: GetSegmentsPageOptions = {}): Promise<SegmentsPage> {
  const result = await generatedCommands.getSegmentsPage(
    options.verified ?? null,
    options.query ?? null,
    options.sort ?? 'newest',
    options.limit ?? 300,
    options.cursor ?? null,
    options.focused ?? false,
  );
  if (result.status === 'error') throw result.error;
  const data = result.data;
  // THROW, never a benign empty result. Returning [] here turned "the IPC payload was not what this
  // app understands" into "your library is empty" — a failure that looks exactly like success. Every
  // caller of these three already has a user-visible error path (segmentStore raises a PERSISTENT
  // banner with Retry, ValidationPanel and ReviewMode toast, ReviewInbox writes a status line); the
  // silent fallback bypassed all of them and left console.error, which no user opens, as the only
  // record. An empty ValidationPanel reads as "no anomalies found" and an empty inbox as "nothing left
  // to review" — both are clean bills of health issued by a broken read.
  if (!isSegmentsPagePayload(data)) {
    throw new Error(
      `get_segments_page returned ${typeof data}, not a page payload — the library could not be read`,
    );
  }
  return data;
}

export function isCommandErrorV1(error: unknown, code?: string): error is CommandErrorV1 {
  if (!error || typeof error !== 'object') return false;
  const candidate = error as Partial<CommandErrorV1>;
  return (
    candidate.schema === 1 &&
    typeof candidate.code === 'string' &&
    typeof candidate.message === 'string' &&
    typeof candidate.retryable === 'boolean' &&
    (code === undefined || candidate.code === code)
  );
}

export function reviewErrorMessage(_error: unknown, fallback: string): string {
  // Backend messages are useful in explicit diagnostics, but ordinary review surfaces are localized.
  // Typed callers can separately retain the structured code, action and operation ID.
  return fallback;
}

/** Revision-paired queue read generated from the Rust contract. */
export async function getReviewPageV1(
  scope: ReviewScope,
  cursor: string | null = null,
  limit = 100,
): Promise<ReviewPageV1> {
  const result = await generatedCommands.getReviewPageV1(scope, limit, cursor);
  if (result.status === 'error') throw result.error;
  return result.data;
}

/** Discover the opaque identity of the active file-owned focus without exposing its private label,
 * segment ids or data-directory path. `null` means no focus policy is active. */
export async function getActiveVoiceFocusV1(): Promise<ActiveVoiceFocusV1 | null> {
  const result = await generatedCommands.getActiveVoiceFocusV1();
  if (result.status === 'error') throw result.error;
  return result.data;
}

/** Load only the exact focus generation previously returned by `getActiveVoiceFocusV1`. A changed,
 * missing or malformed owner policy is rejected by the backend rather than silently widened. */
export async function getVoiceFocusReviewPageV1(
  focusId: string,
  cursor: string | null = null,
  limit = 100,
): Promise<ReviewPageV1> {
  return getReviewPageV1({ kind: 'voiceFocus', focusId }, cursor, limit);
}

/**
 * Commit an exact versioned review request. A transport-level lost response gets one replay with the
 * SAME operation id and payload; a structured backend refusal is never retried blindly.
 */
export async function commitReviewV1(request: CommitReviewRequestV1): Promise<CommittedReviewV1> {
  const invokeExact = async (): Promise<CommittedReviewV1> => {
    const result = await generatedCommands.commitReviewV1(request);
    if (result.status === 'error') throw result.error;
    return result.data;
  };
  try {
    return await invokeExact();
  } catch (error) {
    if (error instanceof Error) return invokeExact();
    throw error;
  }
}

/**
 * Persist one closed technical media disposition. This is intentionally separate from
 * `commitReviewV1`: it records no transcript verdict and carries no playback receipt. A lost
 * transport response gets one replay with the byte-identical operation id and payload; a typed
 * backend refusal is definitive and is never retried blindly.
 */
export async function markSegmentUnusableV1(
  request: MarkSegmentUnusableRequestV1,
): Promise<MarkedSegmentUnusableV1> {
  const invokeExact = async (): Promise<MarkedSegmentUnusableV1> => {
    const result = await generatedCommands.markSegmentUnusableV1(request);
    if (result.status === 'error') throw result.error;
    return result.data;
  };
  try {
    return await invokeExact();
  } catch (error) {
    if (error instanceof Error) return invokeExact();
    throw error;
  }
}

/** Load one crash-safe draft. Drafts are non-authoritative and never enter corpus truth. */
export async function getReviewDraftV1(segmentId: string): Promise<ReviewDraftV1 | null> {
  const result = await generatedCommands.getReviewDraftV1(segmentId);
  if (result.status === 'error') throw result.error;
  return result.data;
}

/** Persist the exact clip/revision draft with a server-owned timestamp. */
export async function saveReviewDraftV1(
  segmentId: string,
  baseRevision: number,
  text: string,
): Promise<ReviewDraftV1> {
  const result = await generatedCommands.saveReviewDraftV1(segmentId, baseRevision, text);
  if (result.status === 'error') throw result.error;
  return result.data;
}

/** Revision-guarded explicit discard; returns false when no matching draft exists. */
export async function deleteReviewDraftV1(
  segmentId: string,
  baseRevision: number,
): Promise<boolean> {
  const result = await generatedCommands.deleteReviewDraftV1(segmentId, baseRevision);
  if (result.status === 'error') throw result.error;
  return result.data;
}

export function reviewEffectId(decisionId: string): number | null {
  const match = /^effect:([1-9][0-9]*)$/.exec(decisionId);
  const value = match ? Number(match[1]) : Number.NaN;
  return Number.isSafeInteger(value) ? value : null;
}

export async function getSegmentIdsForView(
  options: {
    verified?: boolean | null;
    query?: string | null;
    transcriptState?: 'any' | 'real' | 'missing';
  } = {},
): Promise<string[]> {
  const result = await generatedCommands.getSegmentIdsForView(
    options.verified ?? null,
    options.query ?? null,
    options.transcriptState ?? 'any',
  );
  if (result.status === 'error') throw result.error;
  const data = result.data;
  if (!Array.isArray(data) || data.some((id) => typeof id !== 'string')) {
    throw new Error('get_segment_ids_for_view returned an invalid payload');
  }
  return data;
}

export async function getSignalAnomalySegments(limit = 100): Promise<SpeechSegment[]> {
  const result = await generatedCommands.getSignalAnomalySegments(limit);
  if (result.status === 'error') throw result.error;
  const data = result.data;
  if (!Array.isArray(data) || !data.every(isSpeechSegmentPayload))
    throw new Error('get_signal_anomaly_segments returned an invalid payload');
  return data;
}

export type SegmentMetadataFields = Partial<Pick<SpeechSegment, 'speakerId' | 'alignmentJson'>>;
export type SegmentMetadataBaseline = Pick<SpeechSegment, 'speakerId' | 'alignmentJson'>;

/**
 * Compare-and-set only library-owned metadata against the exact server values last observed by the
 * renderer. Human transcript and verification truth are deliberately unrepresentable here.
 */
export async function updateSegmentMetadataV1(
  segmentId: string,
  expected: SegmentMetadataBaseline,
  fields: SegmentMetadataFields,
): Promise<UpdatedSegmentMetadataV1> {
  const changes: SegmentMetadataChangeV1[] = [];
  if ('speakerId' in fields) {
    changes.push({
      field: 'speakerId',
      expected: expected.speakerId,
      value: fields.speakerId ?? null,
    });
  }
  if ('alignmentJson' in fields) {
    changes.push({
      field: 'alignmentJson',
      expected: expected.alignmentJson,
      value: fields.alignmentJson ?? null,
    });
  }
  if (changes.length === 0) {
    throw new Error('Refusing an empty segment metadata update');
  }
  const result = await generatedCommands.updateSegmentMetadataV1({ segmentId, changes });
  if (result.status === 'error') throw result.error;
  return result.data;
}

export async function deleteSegment(id: string): Promise<void> {
  await deleteSegmentsV1([id]);
}

export async function deleteSegmentsBatch(ids: string[]): Promise<void> {
  await deleteSegmentsV1(ids);
}

export async function deleteSegmentsV1(ids: string[]): Promise<DeletedSegmentsV1> {
  const result = await generatedCommands.deleteSegmentsV1({ ids });
  if (result.status === 'error') throw result.error;
  return result.data;
}

export async function exportDataset(path: string, format: string): Promise<void> {
  return invokeCritical('export_dataset', { path, format });
}

/** Plain human transcript / subtitle export (format: 'txt' | 'srt' | 'vtt'). */
export async function exportTranscript(path: string, format: 'txt' | 'srt' | 'vtt'): Promise<void> {
  return invokeCritical('export_transcript', { path, format });
}

export interface EngineStatus {
  ready: boolean;
  port: number;
  identityMatches: boolean;
  expectedModelVersionId: string | null;
  expectedDeploymentSha256: string | null;
  loadedModelVersionId: string | null;
  loadedDeploymentSha256: string | null;
  reason: string | null;
}

/** Health of the champion (OmniASR-7B) warm server, for the engine-status pill. */
export async function getChampionEngineStatus(): Promise<EngineStatus> {
  return invokeLegacy<EngineStatus>('get_champion_engine_status');
}

/** A durable background job (P0 #3 Job Supervisor). Mirrors crate::jobs::Job (camelCase). */
export interface Job {
  id: string;
  kind: string;
  state: 'queued' | 'running' | 'succeeded' | 'failed' | 'cancelled';
  progress: number;
  completed: number;
  total: number | null;
  errorCode: string | null;
}

/** Recent durable jobs (newest first) for the activity surface. Cheap read; safe to poll. */
export async function getJobs(): Promise<Job[]> {
  return invokeLegacy<Job[]>('get_jobs');
}

/** Start the champion 7B server (WSL) from the app; returns immediately, then poll status. */
export async function startChampionEngine(): Promise<void> {
  return invokeLegacy<void>('start_champion_engine');
}

export interface AgentSourceReferenceSummary {
  audioPath: string;
  modelId: string;
  audioContentHash?: string | null;
  audioSizeBytes?: number | null;
  transcriptPath: string;
  textChars: number;
}

export interface AgentSourceReferenceCoverage {
  audioPath: string;
  requiredModels: string[];
  presentModels: string[];
  missingModels: string[];
  complete: boolean;
}

export interface AgentLongFileDossier {
  audioPath: string;
  chunkCount: number;
  totalDurationMs: number;
  sourceReferences: AgentSourceReferenceSummary[];
  sourceReferenceCoverage: AgentSourceReferenceCoverage;
  hypothesisModelCounts: Record<string, number>;
  verdictCounts: Record<string, number>;
  trainingReadySegments: number;
  escalatedSegments: string[];
  promotionStatus: string;
  promotionBlockers: string[];
}

export interface HypothesisCoverageReport {
  minimumNonEmptyModelCount: number;
  nonEmptyModelCount: number;
  passesMinimum: boolean;
  nonEmptyModels: string[];
  ignoredModels: string[];
}

export interface AgentHypothesisCoverageBlocker {
  segmentId: string;
  grade: string;
  trainingReady: boolean;
  coverage: HypothesisCoverageReport;
}

export interface AgentOrchestrationStage {
  stage: string;
  status: string;
  summary: string;
  blockerCount: number;
  blockers: string[];
}

export interface AgentImportSummary {
  totalSegments: number;
  agenticReadiness?: AgenticReadiness | null;
  sourceReferences: AgentSourceReferenceSummary[];
  sourceReferenceRequired: boolean;
  requiredSourceReferenceModels: string[];
  sourceReferenceModels: string[];
  sourceReferenceCoverage: AgentSourceReferenceCoverage[];
  longFileDossiers: AgentLongFileDossier[];
  hypothesisModels: string[];
  hypothesisModelCounts: Record<string, number>;
  verdictCounts: Record<string, number>;
  escalatedSegments: string[];
  trainingGradeSummary: {
    totalSegments: number;
    trainingReadySegments: number;
    goldSegments: number;
    silverSegments: number;
    reviewSegments: number;
    rejectedSegments: number;
  };
  trainingGradeReasonCounts: Record<string, number>;
  hypothesisCoverageBlockers: AgentHypothesisCoverageBlocker[];
  orchestrationStages: AgentOrchestrationStage[];
}

export interface AgentImportReport {
  id: string;
  agentRunId: string | null;
  source: string;
  status: string;
  audioPaths: string[];
  segmentIds: string[];
  summary: AgentImportSummary;
  juryReport: Record<string, unknown> | null;
  error: string | null;
  createdAt: string;
}

export interface AgentStageEvent {
  id: number;
  runId: string;
  source: string;
  stage: string;
  status: string;
  file: string;
  detail: string;
  current: number;
  total: number;
  createdAt: string;
}

export interface MediaGrant {
  id: string;
  path: string;
  expiresAt: string;
}

export async function listAgentImportReports(limit = 25): Promise<AgentImportReport[]> {
  return invokeLegacy<AgentImportReport[]>('list_agent_import_reports', { limit });
}

export async function listAgentStageEvents(
  runId?: string | null,
  limit = 50,
): Promise<AgentStageEvent[]> {
  return invokeLegacy<AgentStageEvent[]>('list_agent_stage_events', {
    runId: runId ?? null,
    limit,
  });
}

export async function registerMediaAsset(audioPath: string): Promise<MediaGrant> {
  return invokeCritical('register_media_asset', { audioPath });
}

/** A decoded-PCM-verified immutable grant. Only review workstations request this stronger authority. */
export async function registerReviewMediaAsset(audioPath: string): Promise<MediaGrant> {
  return invokeCritical('register_review_media_asset', { audioPath });
}

export async function getMediaAssetUrl(id: string): Promise<string> {
  return invokeCritical('get_media_asset_url', { id });
}

export async function beginDesktopPlaybackSessionV1(
  segmentId: string,
  mediaGrantId: string,
  expectedRevision: number,
  clientAttemptId: string,
): Promise<DesktopPlaybackSessionV1> {
  const result = await generatedCommands.beginDesktopPlaybackSessionV1(
    segmentId,
    mediaGrantId,
    expectedRevision,
    clientAttemptId,
  );
  if (result.status === 'error') throw result.error;
  return result.data;
}

/**
 * Retire one exact renderer playback attempt only while it has not produced an immutable receipt.
 * Replaying a successful cancellation returns false; finalized authority is refused by the backend.
 */
export async function cancelDesktopPlaybackSessionV1(
  playbackReceiptId: string,
  clientAttemptId: string,
): Promise<boolean> {
  const result = await generatedCommands.cancelDesktopPlaybackSessionV1(
    playbackReceiptId,
    clientAttemptId,
  );
  if (result.status === 'error') throw result.error;
  return result.data;
}

/**
 * Lowercase names of cloud providers whose API key is present in `secrets.env`
 * ("gemini" and/or "openrouter"). Returns names only — never key
 * values — so it is safe to surface in the UI.
 */
export async function getConfiguredProviders(): Promise<string[]> {
  const result = await generatedCommands.getConfiguredProviders();
  if (result.status === 'error') throw result.error;
  return result.data;
}

/**
 * Save one provider API key into the local `secrets.env` (empty key clears it). The value goes
 * straight to the backend and is never logged or echoed back; the resolved list of configured
 * provider NAMES is returned so the UI can refresh its set/unset badges.
 */
export async function setApiKey(provider: 'gemini' | 'openrouter', key: string): Promise<string[]> {
  const result = await generatedCommands.setApiKey(provider, key);
  if (result.status === 'error') throw result.error;
  return result.data;
}

/** One reviewer's private way in. Each named reviewer gets their own token, so two people never share
 *  a link and therefore never share an identity in the data. */
export interface CouchReviewer {
  /** The name recorded on every row this person decides (`speech_segments.reviewed_by`). */
  name: string;
  /** Wi-Fi (LAN) URL carrying this reviewer's own token. */
  url: string;
  /**
   * Same page over the owner's Tailscale tailnet — works from ANY network (4G, elsewhere),
   * end-to-end encrypted between devices in the tailnet. Null when no tailnet is up.
   */
  tailscaleUrl: string | null;
  funnelUrl: string | null;
}

/** Couch Review: the token-gated phone review server (off by default, per-session). */
export interface CouchStatus {
  running: boolean;
  /** One entry per named reviewer; empty when stopped. */
  reviewers: CouchReviewer[];
  /** SHA-256 of the TLS certificate, verified against the trusted desktop during first pairing. */
  certificateFingerprint: string | null;
}

/** Start the server. An empty list starts a single-reviewer session under the default name. */
export async function startCouchReview(reviewers: string[] = []): Promise<CouchStatus> {
  return invokeCritical('start_couch_review', { reviewers });
}

export async function stopCouchReview(): Promise<CouchStatus> {
  return invokeCritical('stop_couch_review');
}

export async function couchReviewStatus(): Promise<CouchStatus> {
  return invokeCritical('couch_review_status');
}

/** How one remote reviewer scored on clips whose answer was already known. */
export interface SpotCheckScore {
  reviewer: string;
  /** How many known-answer clips they were given. Read nothing into a handful. */
  checks: number;
  /** On how many they changed the wrong draft (or rejected it) rather than accepting it blindly. */
  noticed: number;
  /** Mean character error rate of their text against the known answer. */
  meanCer: number;
}

/** Worst `noticed` rate first — the reviewer who may not be listening comes top, not last. */
export async function spotCheckReport(): Promise<SpotCheckScore[]> {
  return invokeCritical('spot_check_report');
}

/** One reviewer's measured throughput, from the append-only review trail. */
export interface ReviewerThroughput {
  reviewer: string;
  /** DISTINCT clips decided — counting rows would let a network retry inflate it. */
  clips: number;
  /** Median seconds between their consecutive decisions, within their OWN stream. */
  medianSeconds: number | null;
  /** How many gaps that median is drawn from; a median over two samples is not a rate. */
  samples: number;
}

/** Busiest reviewer first. Partitioned per reviewer, unlike the global stats.rs timing. */
export async function reviewerThroughput(): Promise<ReviewerThroughput[]> {
  return invokeCritical('reviewer_throughput');
}

/** Revoke ONE reviewer's link; everyone else's keeps working. */
export async function revokeCouchReviewer(reviewer: string): Promise<CouchStatus> {
  return invokeCritical('revoke_couch_reviewer', { reviewer });
}

/** A two-rater agreement sample, ready for `scripts/agreement_kappa.py`. */
export interface AgreementExport {
  raterA: string;
  raterB: string;
  /** Clips BOTH raters answered. Kappa on a handful of items means nothing. */
  items: number;
  tsv: string;
  path: string;
  /** Reviewers excluded because Cohen's kappa takes exactly two — never silently dropped. */
  otherReviewers: string[];
}

/** Null when no clip has been answered by two different people yet. */
export async function exportAgreementSample(): Promise<AgreementExport | null> {
  return invokeCritical('export_agreement_sample');
}

/** A row of the model-version registry (snake_case, as serialized by the backend). */
export interface ModelVersion {
  id: string;
  family: string;
  model_card_name: string | null;
  checkpoint_sha256: string;
  source: string;
  license: string;
  /** "candidate" or "champion". */
  status: string;
}

/** The registered model versions, newest-first within each family. */
export async function listModelVersions(): Promise<ModelVersion[]> {
  return invokeLegacy<ModelVersion[]>('list_model_versions');
}

/**
 * Register an externally fine-tuned checkpoint as a gated candidate. The SHA-256 is computed
 * server-side from the file; promotion to champion is a separate gated step. Returns the new id.
 */
export async function importModelCheckpoint(args: {
  id: string;
  checkpointPath: string;
  source: string;
  license: string;
  modelCardName?: string | null;
}): Promise<string> {
  return invokeCritical('import_model_checkpoint', {
    id: args.id,
    checkpointPath: args.checkpointPath,
    source: args.source,
    license: args.license,
    modelCardName: args.modelCardName ?? null,
  });
}

/**
 * Register a complete OmniASR-7B deployment manifest as a candidate. The backend derives model,
 * family and component identities from the verified file; renderer input cannot override them.
 */
export async function importModelDeployment(args: {
  manifestPath: string;
  expectedDeploymentSha256: string;
  expectedModelId: string;
  source: string;
  license: string;
}): Promise<ModelVersion> {
  return invokeCritical('import_model_deployment', {
    manifestPath: args.manifestPath,
    expectedDeploymentSha256: args.expectedDeploymentSha256,
    expectedModelId: args.expectedModelId,
    source: args.source,
    license: args.license,
  });
}

/**
 * One-time registration of the measured pre-flywheel OmniASR-7B incumbent. The backend accepts only
 * the exact pinned legacy composite and only while the family has no rows; this is not a general
 * promotion shortcut.
 */
export async function bootstrapLegacyChampion(args: {
  manifestPath: string;
  expectedDeploymentSha256: string;
  expectedModelId: string;
  license: string;
}): Promise<ModelVersion> {
  return invokeCritical('bootstrap_legacy_champion', {
    manifestPath: args.manifestPath,
    expectedDeploymentSha256: args.expectedDeploymentSha256,
    expectedModelId: args.expectedModelId,
    license: args.license,
  });
}

/** Persisted session view-state (snake_case, as serialized by the backend). */
export interface SessionState {
  search_query: string;
  sort_order: string;
  // M2.6 / P1.5: the review cursor + filter, restored on launch (the backend already persists them
  // on every human decision; these were serialized but never read back by the UI).
  selected_segment_id: string | null;
  filter_verified: boolean | null;
  segment_count: number;
  verified_count: number;
}

/** Restore the last session's view-state, or null if there is no recent session. */
export async function restoreSession(): Promise<SessionState | null> {
  const result = await generatedCommands.restoreSession();
  if (result.status === 'error') throw result.error;
  return result.data;
}

/** Persist the current search query + sort order so they survive a restart. */
export async function saveSession(
  searchQuery: string,
  sortOrder: string,
  filterVerified: boolean | null = null,
): Promise<void> {
  const result = await generatedCommands.saveSession(searchQuery, sortOrder, filterVerified);
  if (result.status === 'error') throw result.error;
}

/** Number of audio fingerprints stored for duplicate-import detection. */
export async function getFingerprintCount(): Promise<number> {
  const result = await generatedCommands.getFingerprintCount();
  if (result.status === 'error') throw result.error;
  return result.data;
}

/** Aggregate telemetry stats (snake_case, as serialized by the backend Tracer). */
export async function getTracingStats(): Promise<TracingStatsV1> {
  const result = await generatedCommands.getTracingStats();
  if (result.status === 'error') throw result.error;
  return result.data;
}

export async function getRecentSpans(count?: number): Promise<TracingSpanV1[]> {
  const result = await generatedCommands.getRecentSpans(count ?? null);
  if (result.status === 'error') throw result.error;
  return result.data;
}

export async function clearTracingSpans(): Promise<void> {
  const result = await generatedCommands.clearTracingSpans();
  if (result.status === 'error') throw result.error;
}

export async function getWaveform(
  path: string,
  numPoints: number,
  alignmentJson?: string | null,
): Promise<number[]> {
  return invokeLegacy<number[]>('get_waveform', {
    path,
    numPoints,
    alignmentJson: alignmentJson ?? null,
  });
}

export async function getDatasetStats(): Promise<DatasetStats> {
  return invokeLegacy<DatasetStats>('get_dataset_stats');
}

/** P3.3: which distinct source audio files are missing on disk. */
export interface AudioHealth {
  totalFiles: number;
  missingFiles: number;
  missingPaths: string[];
}

/** P3.3: outcome of a basename-based relink. */
export interface RelinkResult {
  relinked: number;
  stillMissing: number;
}

export async function getAudioHealth(): Promise<AudioHealth> {
  return invokeCritical('get_audio_health');
}

/** P3.3: relink missing source audio by basename against a folder the owner picks. */
export async function relinkAudio(searchDir: string): Promise<RelinkResult> {
  return invokeCritical('relink_audio', { searchDir });
}

/** Intelligence read-side: LOOP-0 shadow precision (C5 go-live evidence) + auto-accept precision (C4). */
export interface IntelligenceReport {
  loop0Shadow: {
    totalObservations: number;
    wouldFire: number;
    /** OVER-TRIGGER count — must be 0 before LOOP-0 firing may ever be enabled (C5). */
    firedButHumanAcceptedOriginal: number;
    firedAndHumanEdited: number;
    firedAndHumanRejected: number;
  };
  autoAcceptPrecision: {
    t0Accepts: number;
    t1Escalations: number;
    t0HumanConfirmed: number;
    t0HumanContradicted: number;
  };
  /** Honest distance-to-calibration for the T0 auto-accept gate: per-SNR-bucket verified counts vs
   * the minimum needed at ZERO CER (a hard lower bound — real data needs more). Explains why the
   * jury escalates everything at low data volumes instead of leaving it a mystery (C3). Optional so
   * an older backend (pre-v34 exe) doesn't break the dashboard. */
  conformalCalibration?: {
    targetErrorCer: number;
    perBucketDelta: number;
    minNeededAtZeroCer: number;
    buckets: Array<{ bucket: string; verifiedWithReference: number; minNeededAtZeroCer: number }>;
  };
}

export async function getIntelligenceReport(): Promise<IntelligenceReport> {
  return invokeLegacy<IntelligenceReport>('get_intelligence_report');
}

/** B2: a past corruption quarantine, if any, plus how many restore snapshots exist. */
export interface QuarantineNotice {
  quarantinedFiles: string[];
  snapshotCount: number;
  newestSnapshotSegments: number | null;
}

export async function getQuarantineNotice(): Promise<QuarantineNotice> {
  return invokeCritical('get_quarantine_notice');
}

/** B2: one rotating auto-snapshot in the restore picker (newest first). */
export interface SnapshotInfo {
  name: string;
  timestamp: number;
  dbSizeBytes: number;
  segmentCount: number | null;
}

export async function listDbSnapshots(): Promise<SnapshotInfo[]> {
  return invokeCritical('list_db_snapshots');
}

/** B2: restore the live database from a named auto-snapshot (destructive — confirm first). */
export async function restoreDbFromSnapshot(name: string): Promise<void> {
  return invokeCritical('restore_db_from_snapshot', { name });
}

/** Complete speaker inventory; SQL NULL/unassigned is distinct from every literal speaker id. */
export async function getSpeakerInventoryV1(): Promise<SpeakerInventoryItemV1[]> {
  const result = await generatedCommands.getSpeakerInventoryV1();
  if (result.status === 'error') throw result.error;
  return result.data;
}

export interface DatasetQuality {
  totalSegments: number;
  emptyTranscriptCount: number;
  lowConfidenceCount: number;
  duplicateTranscriptGroups: number;
  duplicateTranscriptSegments: number;
  durationOutlierCount: number;
  medianDurationMs: number;
  q1DurationMs: number;
  q3DurationMs: number;
  duplicateGroups: Array<{
    transcriptHash: string;
    segmentIds: string[];
    normalizedPreview: string;
  }>;
  durationOutliers: Array<{
    segmentId: string;
    durationMs: number;
    reason: string;
  }>;
  annotatedSegmentCount: number;
  meanWer: number | null;
  meanCer: number | null;
  segmentsAboveWerThreshold: number;
  segmentsAboveCerThreshold: number;
  qualityGatePassed: boolean;
  werOutliers: Array<{
    segmentId: string;
    wer: number;
    cer: number;
    referencePreview: string;
  }>;
}

export async function getDatasetQuality(): Promise<DatasetQuality> {
  return invokeLegacy<DatasetQuality>('get_dataset_quality');
}

/**
 * Library-wide training grade + the reasons behind it.
 *
 * `trainingReadySegments` is what an export would ACTUALLY write — it comes from the same
 * `training_grade_for_segment` the export gates on. Readiness must never be derived from the
 * verified count instead: a fully-reviewed library still exports nothing when, say, no word aligner
 * is installed and every clip carries `energy_heuristic_alignment`. `reasonCounts` is what turns
 * that dead end into an actionable blocker.
 */
export interface TrainingGradeBreakdown {
  summary: {
    totalSegments: number;
    trainingReadySegments: number;
    goldSegments: number;
    silverSegments: number;
    reviewSegments: number;
    rejectedSegments: number;
  };
  reasonCounts: Record<string, number>;
}

export async function getTrainingGradeBreakdown(): Promise<TrainingGradeBreakdown> {
  const data = await invokeLegacy<TrainingGradeBreakdown>('get_training_grade_breakdown');
  // THROW on a malformed payload, exactly like the library reads above. A caller that receives `{}`
  // and reads `.summary.trainingReadySegments` dies with a TypeError instead of showing "readiness
  // unknown" — which is what happened: the dev IPC mock has no case for this command, so it fell
  // through to the object catch-all and returned `{}`, crashing the whole Insights panel on every
  // load in dev. The readiness verdict already renders a null as "unknown"; making a bad shape reach
  // it as null is the difference between a degraded panel and a dead one.
  if (
    !data ||
    typeof data !== 'object' ||
    !data.summary ||
    typeof data.summary.totalSegments !== 'number'
  ) {
    throw new Error(
      `get_training_grade_breakdown returned ${typeof data} without a summary — readiness cannot be computed`,
    );
  }
  return data;
}

interface CachedSettingsSnapshot {
  revision: number;
  backend: BackendSettings;
}

let cachedSettingsSnapshot: CachedSettingsSnapshot | null = null;

const FRONTEND_PATCH_FIELDS = [
  'vad_threshold',
  'min_segment_duration_ms',
  'max_segment_duration_ms',
  'num_asr_threads',
  'enable_gpu',
  'language',
  'export_format',
  'auto_normalize',
  'verbalize_numbers',
  'auto_align',
  'assign_speaker_from_filename',
  'enable_diarization',
  'enable_denoising',
  'autoplay_segments',
  'max_speakers',
  'max_wer_threshold',
  'max_cer_threshold',
  'enforce_quality_gates',
  'theme',
  'llm_mode',
  'llm_endpoint',
  'llm_system_prompt',
  'llm_model',
  'external_asr_script_path',
  'hf_train_ratio',
  'hf_val_ratio',
  'hf_test_ratio',
  'hf_split_seed',
  'hf_speaker_disjoint',
  'hf_license',
  'jury_model',
  'jury_provider',
  'jury_self_consistency_n',
  'jury_autonomy_level',
  'jury_t1_threshold',
] as const satisfies readonly (keyof BackendSettings)[];

function checkedSettingsSnapshot(snapshot: SettingsSnapshotV1): CachedSettingsSnapshot {
  if (!Number.isSafeInteger(snapshot.settingsRevision) || snapshot.settingsRevision < 0) {
    throw new Error('The generated settings contract returned an invalid revision token.');
  }
  return {
    revision: snapshot.settingsRevision,
    backend: { ...snapshot.settings },
  };
}

async function loadSettingsSnapshotV1(): Promise<CachedSettingsSnapshot> {
  const result = await generatedCommands.getSettingsV1();
  if (result.status === 'error') throw result.error;
  const snapshot = checkedSettingsSnapshot(result.data);
  cachedSettingsSnapshot = snapshot;
  return snapshot;
}

function cacheSettingsResult(result: SettingsPatchResultV1): CachedSettingsSnapshot {
  const snapshot = checkedSettingsSnapshot({
    settingsRevision: result.settingsRevision,
    settings: result.settings,
  });
  cachedSettingsSnapshot = snapshot;
  return snapshot;
}

function changedSettingsFields(
  current: BackendSettings,
  desired: BackendSettings,
): SettingsPatchV1['changedFields'] {
  const currentRecord = current as unknown as Record<string, unknown>;
  const desiredRecord = desired as unknown as Record<string, unknown>;
  const changed: SettingsPatchV1['changedFields'] = {};
  for (const field of FRONTEND_PATCH_FIELDS) {
    const value = desiredRecord[field];
    if (Object.is(value, currentRecord[field])) continue;
    if (typeof value === 'string' || typeof value === 'number' || typeof value === 'boolean') {
      changed[field] = value;
    }
  }
  return changed;
}

async function patchSettingsExact(patch: SettingsPatchV1): Promise<SettingsPatchResultV1> {
  const invokeExact = async (): Promise<SettingsPatchResultV1> => {
    const result = await generatedCommands.patchSettingsV1(patch);
    if (result.status === 'error') throw result.error;
    return result.data;
  };
  try {
    return await invokeExact();
  } catch (error) {
    // `Error` means the invoke transport did not establish whether the backend committed. Replay the
    // exact CAS payload once; the backend returns alreadyApplied only when the whole requested effect
    // is already authoritative. Structured backend refusals are definitive and never retried.
    if (error instanceof Error) return invokeExact();
    throw error;
  }
}

async function setCloudConsentExact(
  expectedSettingsRevision: number,
  consent: 'llm' | 'jury',
  granted: boolean,
): Promise<SettingsPatchResultV1> {
  const request = { expectedSettingsRevision, consent, granted } as const;
  const invokeExact = async (): Promise<SettingsPatchResultV1> => {
    const result = await generatedCommands.setCloudConsentV1(request);
    if (result.status === 'error') throw result.error;
    return result.data;
  };
  try {
    return await invokeExact();
  } catch (error) {
    if (error instanceof Error) return invokeExact();
    throw error;
  }
}

/** A settings write failed after the renderer had optimistic state. When available, the attached
 * snapshot is a fresh server read and is the only safe rollback target after partial/stale writes. */
export class SettingsWriteError extends Error {
  readonly authoritativeSettings: AppSettings | null;
  readonly backendError: unknown;

  constructor(backendError: unknown, authoritativeSettings: AppSettings | null) {
    super('The settings change could not be confirmed.');
    this.name = 'SettingsWriteError';
    this.backendError = backendError;
    this.authoritativeSettings = authoritativeSettings;
  }
}

export function authoritativeSettingsFromWriteError(error: unknown): AppSettings | null {
  return error instanceof SettingsWriteError ? error.authoritativeSettings : null;
}

export async function getSettings(): Promise<AppSettings> {
  const snapshot = await loadSettingsSnapshotV1();
  return mapBackendToFrontend(snapshot.backend);
}

/** Persist changed settings through generated revision-guarded contracts. Secrets remain on
 * `setApiKey`; cloud grants/withdrawals are explicit consent transactions and never enter the
 * generic changed-field patch. The legacy whole-object command is compatibility-only. */
export async function updateSettings(
  settings: AppSettings,
  existingBackend?: BackendSettings,
): Promise<void> {
  let active = cachedSettingsSnapshot ?? (await loadSettingsSnapshotV1());
  const desired = mapFrontendToBackend(settings, existingBackend ?? active.backend);

  try {
    // Withdrawals are stop instructions and therefore happen before ordinary preferences. Grants
    // happen last so a preference-save failure can never accidentally enable cloud work.
    for (const [consent, field] of [
      ['llm', 'cloud_llm_opt_in'],
      ['jury', 'jury_cloud_opt_in'],
    ] as const) {
      if (active.backend[field] && !desired[field]) {
        active = cacheSettingsResult(await setCloudConsentExact(active.revision, consent, false));
      }
    }

    const changedFields = changedSettingsFields(active.backend, desired);
    if (Object.keys(changedFields).length > 0) {
      active = cacheSettingsResult(
        await patchSettingsExact({
          expectedSettingsRevision: active.revision,
          changedFields,
        }),
      );
    }

    for (const [consent, field] of [
      ['llm', 'cloud_llm_opt_in'],
      ['jury', 'jury_cloud_opt_in'],
    ] as const) {
      if (!active.backend[field] && desired[field]) {
        active = cacheSettingsResult(await setCloudConsentExact(active.revision, consent, true));
      }
    }
  } catch (error) {
    let authoritative: AppSettings | null = null;
    try {
      authoritative = mapBackendToFrontend((await loadSettingsSnapshotV1()).backend);
    } catch {
      // Keep the original mutation failure authoritative. A failed recovery read means the UI must
      // retain its previous confirmed state; it must never invent server truth.
    }
    throw new SettingsWriteError(error, authoritative);
  }
}

export const ValidationSeverity = {
  Error: 'Error',
  Warning: 'Warning',
} as const;

export const ValidationCategory = {
  MissingAudio: 'MissingAudio',
  EmptyTranscript: 'EmptyTranscript',
  DuplicateFingerprint: 'DuplicateFingerprint',
  DurationMismatch: 'DurationMismatch',
  InvalidSpeaker: 'InvalidSpeaker',
  CorruptAudio: 'CorruptAudio',
  AnnotationIncomplete: 'AnnotationIncomplete',
  Other: 'Other',
} as const;

export type ValidationSeverityValue = (typeof ValidationSeverity)[keyof typeof ValidationSeverity];
export type ValidationCategoryValue = (typeof ValidationCategory)[keyof typeof ValidationCategory];

export interface ValidationIssue {
  severity: ValidationSeverityValue;
  category: ValidationCategoryValue;
  segmentId: string | null;
  field: string;
  message: string;
  details: string | null;
}

export interface ValidationReport {
  totalSegments: number;
  totalAudioFiles: number;
  passed: number;
  warnings: ValidationIssue[];
  errors: ValidationIssue[];
  summary: string;
}

export async function validateDataset(): Promise<ValidationReport> {
  return invokeCritical('validate_dataset_cmd');
}

export const AudioExportFormat = {
  Wav: 'Wav',
} as const;

export type AudioExportFormatValue = (typeof AudioExportFormat)[keyof typeof AudioExportFormat];

export async function exportAudio(
  segmentIds: string[],
  options: {
    output_dir: string;
    format: AudioExportFormatValue;
    sample_rate: number;
    include_metadata: boolean;
  },
): Promise<{
  total: number;
  succeeded: number;
  failed: number;
  output_dir: string;
  files: string[];
  errors: string[];
}> {
  return invokeCritical('export_audio', { segmentIds, options });
}

export async function assignSpeakersV1(
  request: AssignSpeakersRequestV1,
): Promise<AssignedSpeakersV1> {
  const result = await generatedCommands.assignSpeakersV1(request);
  if (result.status === 'error') throw result.error;
  return result.data;
}

export async function batchNormalize(ids: string[]): Promise<{ status: string }> {
  return invokeLegacy<{ status: string }>('batch_normalize', { ids });
}

export async function rediarizeSegments(ids: string[]): Promise<number> {
  return invokeLegacy<number>('rediarize_segments', { ids });
}

export async function renameSpeakerV1(request: RenameSpeakerRequestV1): Promise<RenamedSpeakerV1> {
  const result = await generatedCommands.renameSpeakerV1(request);
  if (result.status === 'error') throw result.error;
  return result.data;
}

export async function mergeDatasetJson(
  jsonContent: string,
): Promise<{ created: number; updated: number }> {
  return invokeCritical('merge_dataset_json', { jsonContent });
}

export async function exportHuggingfaceDataset(outputDir: string): Promise<void> {
  return invokeCritical('export_huggingface_dataset', { path: outputDir });
}

export async function undo(): Promise<HistoryMutationResultV1> {
  const result = await generatedCommands.undo();
  if (result.status === 'error') throw result.error;
  return result.data;
}

export async function redo(): Promise<HistoryMutationResultV1> {
  const result = await generatedCommands.redo();
  if (result.status === 'error') throw result.error;
  return result.data;
}

export async function getHistoryStatusV1(): Promise<HistoryStatusV1> {
  const result = await generatedCommands.getHistoryStatusV1();
  if (result.status === 'error') throw result.error;
  return result.data;
}

export async function computeDiff(raw: string, annotated: string): Promise<TextDiff> {
  const result = await generatedCommands.computeDiff(raw, annotated);
  if (result.status === 'error') throw result.error;
  return result.data;
}

/** Back up the live library to `dest` on a DEDICATED connection (the UI stays responsive), then
 * verify the WRITTEN file (integrity check + segment count) — a disaster copy that is itself bad
 * must fail now, not at the disaster. */
export async function dbBackup(
  dest: string,
): Promise<{ integrityOk: boolean; segmentCount: number }> {
  return invokeCritical('db_backup', { dest });
}

/** Archive every quarantined `*.corrupt.*` artifact into `<data_dir>/quarantine/`, releasing the
 * snapshot prune-pin explicitly (bytes stay salvageable). Returns how many files were archived. */
export async function acknowledgeQuarantine(): Promise<number> {
  return invokeCritical('acknowledge_quarantine');
}

/** Restore the live library from a backup .db file (the counterpart to dbBackup). Destructive — the
 *  backend PRAGMA integrity_check's the source before overwriting, so a corrupt file fails fast. */
export async function dbRestore(src: string): Promise<void> {
  return invokeCritical('db_restore', { src });
}

export async function dbVacuum(): Promise<void> {
  return invokeCritical('db_vacuum');
}

export interface ModelStatusEntry {
  name: string;
  filename: string;
  downloaded: boolean;
  exists?: boolean;
  size_bytes: number | null;
  min_size_bytes: number;
  source?: 'user' | 'bundled' | 'missing';
  downloadable?: boolean;
}

export async function modelsStatus(): Promise<ModelStatusEntry[]> {
  return invokeLegacy<ModelStatusEntry[]>('models_status');
}

export interface ModelDownloadSummary {
  downloaded: number;
  failed: number;
  total: number;
  skipped: number;
}

export async function modelsDownloadAll(): Promise<ModelDownloadSummary> {
  return invokeLegacy<ModelDownloadSummary>('models_download_all');
}

export async function getInferenceStats(): Promise<InferenceStatsV1> {
  const result = await generatedCommands.getInferenceStats();
  if (result.status === 'error') throw result.error;
  return result.data;
}

export async function appHealth(): Promise<AppHealthV1> {
  const result = await generatedCommands.appHealth();
  if (result.status === 'error') throw result.error;
  return result.data;
}

/** One-line summary of the previous session's crash (surfaced once), or null if it exited cleanly. */
export async function takeLastCrash(): Promise<string | null> {
  const result = await generatedCommands.takeLastCrash();
  if (result.status === 'error') throw result.error;
  return result.data;
}

export interface AgenticReadinessCheck {
  id: string;
  label: string;
  status: 'ready' | 'degraded' | 'blocked';
  detail: string;
}

export interface AgenticReadiness {
  status: 'ready' | 'degraded' | 'blocked';
  ready: boolean;
  sourceReferenceModels: string[];
  availableHypothesisModels: string[];
  requiredHypothesisModels: number;
  checks: AgenticReadinessCheck[];
}

export async function checkAgenticReadiness(): Promise<AgenticReadiness> {
  return invokeLegacy<AgenticReadiness>('check_agentic_readiness');
}

export interface WslRefinementOptions {
  limit_files?: number;
  limit_segments?: number;
  dry_run: boolean;
  test_one: boolean;
}

export async function runWslRefinement(options: WslRefinementOptions): Promise<{ status: string }> {
  return invokeLegacy<{ status: string }>('run_wsl_refinement', {
    limitFiles: options.limit_files ?? null,
    limitSegments: options.limit_segments ?? null,
    dryRun: options.dry_run,
    testOne: options.test_one,
  });
}

export async function cancelWslRefinement(): Promise<void> {
  const result = await generatedCommands.cancelWslRefinement();
  if (result.status === 'error') throw result.error;
}

export interface ConformalCertificate {
  targetError: number;
  confidenceLevel: number;
  threshold: number;
  totalCertified: number;
  certifiedSegmentIds: string[];
  expectedErrorBound: number;
  isCalibrated: boolean;
  /** Provenance of the calibration confidences: how many were real model posteriors vs the
   * heuristic/unknown fallback. On the default offline path realPosterior is 0 — the readout must not
   * imply a calibrated-posterior guarantee it does not have. */
  calibrationRealPosterior: number;
  calibrationHeuristic: number;
  /** Clips excluded because they carry NEITHER a confidence nor a ctc_score - the absence of any
   *  signal, as opposed to a non-posterior one. When this is the whole calibration set, nothing can be
   *  certified and no amount of reviewing changes it. */
  calibrationNoConfidence: number;
}

export async function getDatasetCertificate(
  targetError: number,
  confidenceLevel: number,
): Promise<ConformalCertificate> {
  return invokeLegacy<ConformalCertificate>('get_dataset_certificate', {
    targetError,
    confidenceLevel,
  });
}

export async function computeSignalAnomalyScores(): Promise<number> {
  return invokeLegacy<number>('compute_signal_anomaly_scores');
}

export async function getActiveLearningQueue(
  targetError: number,
  confidenceLevel: number,
  limit: number,
): Promise<SpeechSegment[]> {
  return invokeLegacy<SpeechSegment[]>('get_active_learning_queue', {
    targetError,
    confidenceLevel,
    limit,
  });
}

// ── Phase 1 — Gold-Set Eval Harness ────────────────────────────────────────

import type { EvalRun, EvalRunResult, EscalationTrendPoint, LabelQualityLift } from './types';

/** Run the real pinned champion over the gold set; the renderer cannot supply a model label. */
export async function runGoldEvalAsr(): Promise<EvalRunResult> {
  return invokeLegacy<EvalRunResult>('run_gold_eval_asr');
}

/** Create gold-eval segments from a verified file. Returns the number created. */
export async function createGoldFromFile(audioPath: string): Promise<number> {
  return invokeCritical('create_gold_from_file', { audioPath });
}

/** M2.7 / P1.6: summary of an export_gold_eval_set run. */
export interface GoldEvalExport {
  manifestPath: string;
  totalGold: number;
  exported: number;
  skipped: number;
}

/** M2.7 / P1.6: bulk-promote every reviewed source file into the gold set; returns rows created. */
export async function importVerifiedSegmentsAsGold(): Promise<number> {
  return invokeCritical('import_verified_segments_as_gold');
}

/** M2.7 / P1.6: export the gold set (manifest.jsonl + 16 kHz WAV clips) under outDir. */
export async function exportGoldEvalSet(outDir: string): Promise<GoldEvalExport> {
  return invokeCritical('export_gold_eval_set', { outDir });
}

/** M5.1 / P5.1: summary of an export_finetune_pack run. */
export interface FinetunePackResult {
  manifestPath: string;
  /** P5.5: pins the exact rows this pack contains — the corpus-ledger key. */
  manifestSha256: string;
  totalVerified: number;
  excludedUnexportable: number;
  /** Rows the training-grade rubric refused (mark-bad, severe audio, placeholder) — the B1 guard. */
  excludedNotTrainingReady: number;
  emitted: number;
  skipped: number;
}

/** M5.1 / P5.1: export a fine-tune training pack from verified segments (holdout-excluded) under outDir. */
export async function exportFinetunePack(outDir: string): Promise<FinetunePackResult> {
  return invokeCritical('export_finetune_pack', { outDir });
}

/** P0.2: the git SHA the running binary was built from (baked at build time). Used for build-info display. */
export async function appGitSha(): Promise<string> {
  const result = await generatedCommands.appGitSha();
  if (result.status === 'error') throw result.error;
  return result.data;
}

/** A reproducible scorecard built from already-computed gold-eval results. */
export interface ScorecardResponse {
  scorecard: unknown;
  markdown: string;
}

/** Build a scorecard (system vs optional baseline) from gold-eval results. */
export async function buildScorecard(
  system: EvalRunResult,
  baseline?: EvalRunResult | null,
): Promise<ScorecardResponse> {
  return invokeLegacy<ScorecardResponse>('build_scorecard', { system, baseline: baseline ?? null });
}

export async function listEvalRuns(): Promise<EvalRun[]> {
  return invokeLegacy<EvalRun[]>('list_eval_runs');
}

export async function getLabelQualityLift(): Promise<LabelQualityLift> {
  return invokeLegacy<LabelQualityLift>('get_label_quality_lift');
}

// ── Phase 2 — T0 Gate + Jury ───────────────────────────────────────────────

export async function getEscalationQueue(limit: number): Promise<SpeechSegment[]> {
  return invokeLegacy<SpeechSegment[]>('get_escalation_queue', { limit });
}

/** Cumulative canonical MEDIA time the authorized renderer reported traversing for one clip revision.
 *
 * Not wall-clock, not a `play()` call, not a download, and not proof of human attention or
 * comprehension. The backend binds the receipt to the segment, revision, exact source span, and
 * decoded-PCM content hash, so it cannot be replayed against a different clip or survive the audio
 * changing.
 */
export async function recordPlaybackReceipt(args: {
  playbackReceiptId: string;
  mediaGrantId: string;
  intervals: readonly PlaybackIntervalV1[];
}): Promise<DesktopPlaybackReceiptV1> {
  // Policy 4 accepts no scalar duration claim. The backend checks this exact canonical interval
  // union against its short-lived media-grant session and server elapsed time, stores every interval,
  // and returns the immutable receipt identity consumed by commitReviewV1.
  const intervals = args.intervals.map((interval) => ({
    startMs: Math.max(0, Math.round(interval.startMs)),
    endMs: Math.max(0, Math.round(interval.endMs)),
  }));
  const result = await generatedCommands.finalizeDesktopPlaybackSessionV1(
    args.playbackReceiptId,
    args.mediaGrantId,
    intervals,
  );
  if (result.status === 'error') throw result.error;
  return result.data;
}

export interface HumanDecisionCommit {
  effectEventId: number;
  segmentId: string;
  effectiveAction: 'accept' | 'edit' | 'reject';
  priorRevision: number;
  decidedRevision: number;
  segment: SpeechSegment;
}

export type HumanDecisionUndoOutcome =
  | { status: 'applied'; restoredRevision: number; segment: SpeechSegment }
  | { status: 'alreadyApplied'; restoredRevision: number; segment: SpeechSegment }
  | { status: 'conflict'; segment: SpeechSegment };

export async function recordHumanDecision(
  _segmentId: string,
  _decision: 'accept' | 'edit' | 'reject',
  _correctedTranscript?: string | null,
  _timestampMs: number = Date.now(),
): Promise<HumanDecisionCommit> {
  throw {
    schema: 1,
    code: 'TYPED_REVIEW_REQUIRED',
    message: 'This legacy review writer is retired. Reload the review workstation and try again.',
    retryable: false,
    suggestedAction: 'reloadClip',
    operationId: null,
  } satisfies CommandErrorV1;
}

/** Exact server-owned inverse of one committed human decision. */
export async function undoHumanDecision(
  effectEventId: number,
  operationId: string,
): Promise<HumanDecisionUndoOutcome> {
  return invokeCritical('undo_human_decision', { effectEventId, operationId });
}

export interface HumanFlagCommit {
  effectEventId: number;
  segmentId: string;
  priorRevision: number;
  flagRevision: number;
  segment: SpeechSegment;
}

export type HumanFlagUndoOutcome =
  | { status: 'applied'; restoredRevision: number; segment: SpeechSegment }
  | { status: 'alreadyApplied'; segment: SpeechSegment }
  | { status: 'conflict'; segment: SpeechSegment };

/** Atomically flag one undecided row and retain a database-owned exact inverse. */
export async function recordReviewFlag(
  segmentId: string,
  rationale: string,
): Promise<HumanFlagCommit> {
  const operationId = crypto.randomUUID();
  const args = { segmentId, rationale, operationId };
  try {
    return await invokeCritical('record_review_flag', args);
  } catch {
    return invokeCritical('record_review_flag', args);
  }
}

/** Exact server-owned inverse of one committed review flag. */
export async function undoReviewFlag(
  effectEventId: number,
  operationId: string,
): Promise<HumanFlagUndoOutcome> {
  return invokeCritical('undo_review_flag', { effectEventId, operationId });
}

export async function getEscalationRateTrend(): Promise<EscalationTrendPoint[]> {
  return invokeLegacy<EscalationTrendPoint[]>('get_escalation_rate_trend');
}

// ── Jury Pipeline (Items 1 & 2) ───────────────────────────────────────────────

export interface JuryPipelineReport {
  totalInput: number;
  t0AutoAccepted: number;
  t0Escalated: number;
  t1Committed: number;
  t2Committed: number;
  humanInbox: number;
}

/** Run the full T0→T1→T2 cascade on a batch of segment IDs. */
export async function runJuryPipeline(segmentIds: string[]): Promise<JuryPipelineReport> {
  return invokeLegacy<JuryPipelineReport>('run_jury_pipeline', { segmentIds });
}

export interface T2Verdict {
  transcript: string;
  reason: string;
  confidence: number;
  evidence: unknown[];
  selfConsistencyAgreement: boolean;
  votes: number;
}

export interface T2Result {
  verdict: T2Verdict | null;
  mustEscalate: boolean;
  error: string | null;
}

/** Run Gemini audio T2 judge directly on a single segment. */
export async function runT2ForSegment(segmentId: string, apiKey: string): Promise<T2Result> {
  return invokeLegacy<T2Result>('run_t2_for_segment', { segmentId, apiKey });
}
