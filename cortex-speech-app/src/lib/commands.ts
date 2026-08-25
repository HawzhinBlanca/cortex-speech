import { invoke } from '@tauri-apps/api/core';
import { commands as generatedCommands } from './generated/ipc';
import type {
  CommandErrorV1,
  CommitReviewRequestV1,
  CommittedReviewV1,
  ReviewPageV1,
  ReviewScope,
} from './generated/ipc';
import type {
  SpeechSegment,
  SegmentsPage,
  WordTimestamp,
  DatasetStats,
  SpeakerStat,
} from './types';
import type { AppSettings } from './stores/settingsStore';
import {
  mapBackendToFrontend,
  mapFrontendToBackend,
  type BackendSettings,
} from './settingsAdapter';

export async function openAudioFile(): Promise<string | null> {
  return invoke<string | null>('open_audio_file');
}

export async function importDirectory(): Promise<{ status: string }> {
  return invoke('import_directory');
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
  return invoke<ImportJob | null>('get_interrupted_import');
}

export async function resumeInterruptedImport(): Promise<{ status: string; resuming: boolean }> {
  return invoke('resume_interrupted_import');
}

export async function discardInterruptedImport(jobId: string): Promise<void> {
  return invoke('discard_interrupted_import', { jobId });
}

export async function importAudioFile(path: string): Promise<{ status: string; source?: string }> {
  return invoke('import_audio_file', { path });
}

export async function cancelOperation(): Promise<void> {
  return invoke<void>('cancel_operation');
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
  return String(msg).includes(ASR_7B_UNAVAILABLE_TAG) || String(e).includes(ASR_7B_UNAVAILABLE_TAG);
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
  return invoke('transcribe_segment', {
    segmentId: segmentId ?? null,
    audioPath,
    alignmentJson: alignmentJson ?? null,
  });
}

export async function batchTranscribe(ids: string[]): Promise<{ status: string }> {
  return invoke('batch_transcribe', { ids });
}

export async function normalizeText(text: string): Promise<string> {
  return invoke<string>('normalize_text', { text });
}

export async function alignSegment(
  audioPath: string,
  text: string,
  alignmentJson?: string | null,
  segmentId?: string | null,
): Promise<WordTimestamp[]> {
  return invoke<WordTimestamp[]>('align_segment', {
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
  return invoke<SegmentConsensus>('get_segment_consensus', { segmentId });
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

export async function getSegment(segmentId: string): Promise<SpeechSegment> {
  const data = await invoke<SpeechSegment>('get_segment', { segmentId });
  if (!data || typeof data.id !== 'string') {
    throw new Error(`get_segment returned an invalid payload for ${segmentId}`);
  }
  return data;
}

export async function getSegmentsPage(options: GetSegmentsPageOptions = {}): Promise<SegmentsPage> {
  const data = await invoke<SegmentsPage>('get_segments_page', {
    verified: options.verified ?? null,
    query: options.query ?? null,
    sort: options.sort ?? 'newest',
    limit: options.limit ?? 300,
    cursor: options.cursor ?? null,
    focused: options.focused ?? false,
  });
  // THROW, never a benign empty result. Returning [] here turned "the IPC payload was not what this
  // app understands" into "your library is empty" — a failure that looks exactly like success. Every
  // caller of these three already has a user-visible error path (segmentStore raises a PERSISTENT
  // banner with Retry, ValidationPanel and ReviewMode toast, ReviewInbox writes a status line); the
  // silent fallback bypassed all of them and left console.error, which no user opens, as the only
  // record. An empty ValidationPanel reads as "no anomalies found" and an empty inbox as "nothing left
  // to review" — both are clean bills of health issued by a broken read.
  if (!data || !Array.isArray(data.items)) {
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

export function reviewErrorMessage(error: unknown, fallback: string): string {
  return isCommandErrorV1(error) ? error.message : fallback;
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
  const data = await invoke<string[]>('get_segment_ids_for_view', {
    verified: options.verified ?? null,
    query: options.query ?? null,
    transcriptState: options.transcriptState ?? 'any',
  });
  if (!Array.isArray(data) || data.some((id) => typeof id !== 'string')) {
    throw new Error('get_segment_ids_for_view returned an invalid payload');
  }
  return data;
}

export async function getSignalAnomalySegments(limit = 100): Promise<SpeechSegment[]> {
  const data = await invoke<SpeechSegment[]>('get_signal_anomaly_segments', { limit });
  if (!Array.isArray(data))
    throw new Error('get_signal_anomaly_segments returned an invalid payload');
  return data;
}

/**
 * Partial metadata update for fields that do not own human-review truth. Transcript corrections and
 * verification are committed only by recordHumanDecision, so they are intentionally unrepresentable
 * in this frontend wrapper. Resolves false when the row no longer exists.
 */
export async function updateSegmentFields(
  segmentId: string,
  fields: Partial<Pick<SpeechSegment, 'speakerId' | 'alignmentJson'>>,
): Promise<boolean> {
  return invoke<boolean>('update_segment_fields', { segmentId, fields });
}

export async function deleteSegment(id: string): Promise<void> {
  return invoke<void>('delete_segment', { id });
}

export async function deleteSegmentsBatch(ids: string[]): Promise<void> {
  return invoke<void>('delete_segments_batch', { ids });
}

export async function exportDataset(path: string, format: string): Promise<void> {
  return invoke<void>('export_dataset', { path, format });
}

/** Plain human transcript / subtitle export (format: 'txt' | 'srt' | 'vtt'). */
export async function exportTranscript(path: string, format: 'txt' | 'srt' | 'vtt'): Promise<void> {
  return invoke<void>('export_transcript', { path, format });
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
  return invoke<EngineStatus>('get_champion_engine_status');
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
  return invoke<Job[]>('get_jobs');
}

/** Start the champion 7B server (WSL) from the app; returns immediately, then poll status. */
export async function startChampionEngine(): Promise<void> {
  return invoke<void>('start_champion_engine');
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
  return invoke<AgentImportReport[]>('list_agent_import_reports', { limit });
}

export async function listAgentStageEvents(
  runId?: string | null,
  limit = 50,
): Promise<AgentStageEvent[]> {
  return invoke<AgentStageEvent[]>('list_agent_stage_events', { runId: runId ?? null, limit });
}

export async function registerMediaAsset(audioPath: string): Promise<MediaGrant> {
  return invoke<MediaGrant>('register_media_asset', { audioPath });
}

export async function getMediaAssetUrl(id: string): Promise<string> {
  return invoke<string>('get_media_asset_url', { id });
}

/**
 * Lowercase names of cloud providers whose API key is present in `secrets.env`
 * ("gemini" and/or "openrouter"). Returns names only — never key
 * values — so it is safe to surface in the UI.
 */
export async function getConfiguredProviders(): Promise<string[]> {
  return invoke<string[]>('get_configured_providers');
}

/**
 * Save one provider API key into the local `secrets.env` (empty key clears it). The value goes
 * straight to the backend and is never logged or echoed back; the resolved list of configured
 * provider NAMES is returned so the UI can refresh its set/unset badges.
 */
export async function setApiKey(provider: 'gemini' | 'openrouter', key: string): Promise<string[]> {
  return invoke<string[]>('set_api_key', { provider, key });
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
  return invoke<CouchStatus>('start_couch_review', { reviewers });
}

export async function stopCouchReview(): Promise<CouchStatus> {
  return invoke<CouchStatus>('stop_couch_review');
}

export async function couchReviewStatus(): Promise<CouchStatus> {
  return invoke<CouchStatus>('couch_review_status');
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
  return invoke<SpotCheckScore[]>('spot_check_report');
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
  return invoke<ReviewerThroughput[]>('reviewer_throughput');
}

/** Revoke ONE reviewer's link; everyone else's keeps working. */
export async function revokeCouchReviewer(reviewer: string): Promise<CouchStatus> {
  return invoke<CouchStatus>('revoke_couch_reviewer', { reviewer });
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
  return invoke<AgreementExport | null>('export_agreement_sample');
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
  return invoke<ModelVersion[]>('list_model_versions');
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
  return invoke<string>('import_model_checkpoint', {
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
  return invoke<ModelVersion>('import_model_deployment', {
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
  return invoke<ModelVersion>('bootstrap_legacy_champion', {
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
  return invoke<SessionState | null>('restore_session');
}

/** Persist the current search query + sort order so they survive a restart. */
export async function saveSession(
  searchQuery: string,
  sortOrder: string,
  filterVerified: boolean | null = null,
): Promise<void> {
  return invoke('save_session', { searchQuery, sortOrder, filterVerified });
}

/** Number of audio fingerprints stored for duplicate-import detection. */
export async function getFingerprintCount(): Promise<number> {
  return invoke<number>('get_fingerprint_count');
}

/** Aggregate telemetry stats (snake_case, as serialized by the backend Tracer). */
export interface TracingStats {
  total_spans: number;
  failures: number;
  total_duration_ms: number;
  avg_duration_ms: number;
}

/** A single recorded operation span. */
export interface TracingSpan {
  operation: string;
  start: string;
  duration_ms: number;
  metadata: Record<string, string>;
  success: boolean;
  error: string | null;
}

export async function getTracingStats(): Promise<TracingStats> {
  return invoke<TracingStats>('get_tracing_stats');
}

export async function getRecentSpans(count?: number): Promise<TracingSpan[]> {
  return invoke<TracingSpan[]>('get_recent_spans', { count: count ?? null });
}

export async function clearTracingSpans(): Promise<void> {
  return invoke('clear_tracing_spans');
}

export async function getWaveform(
  path: string,
  numPoints: number,
  alignmentJson?: string | null,
): Promise<number[]> {
  return invoke<number[]>('get_waveform', {
    path,
    numPoints,
    alignmentJson: alignmentJson ?? null,
  });
}

export async function getDatasetStats(): Promise<DatasetStats> {
  return invoke<DatasetStats>('get_dataset_stats');
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
  return invoke<AudioHealth>('get_audio_health');
}

/** P3.3: relink missing source audio by basename against a folder the owner picks. */
export async function relinkAudio(searchDir: string): Promise<RelinkResult> {
  return invoke<RelinkResult>('relink_audio', { searchDir });
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
  return invoke<IntelligenceReport>('get_intelligence_report');
}

/** B2: a past corruption quarantine, if any, plus how many restore snapshots exist. */
export interface QuarantineNotice {
  quarantinedFiles: string[];
  snapshotCount: number;
  newestSnapshotSegments: number | null;
}

export async function getQuarantineNotice(): Promise<QuarantineNotice> {
  return invoke<QuarantineNotice>('get_quarantine_notice');
}

/** B2: one rotating auto-snapshot in the restore picker (newest first). */
export interface SnapshotInfo {
  name: string;
  timestamp: number;
  dbSizeBytes: number;
  segmentCount: number | null;
}

export async function listDbSnapshots(): Promise<SnapshotInfo[]> {
  return invoke<SnapshotInfo[]>('list_db_snapshots');
}

/** B2: restore the live database from a named auto-snapshot (destructive — confirm first). */
export async function restoreDbFromSnapshot(name: string): Promise<void> {
  return invoke<void>('restore_db_from_snapshot', { name });
}

/** Complete speaker list (not the truncated top-10 dashboard summary) for the speaker manager. */
export async function getSpeakers(): Promise<SpeakerStat[]> {
  return invoke<SpeakerStat[]>('get_speakers');
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
  return invoke<DatasetQuality>('get_dataset_quality');
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
  const data = await invoke<TrainingGradeBreakdown>('get_training_grade_breakdown');
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

export async function getSettings(): Promise<AppSettings> {
  const raw = await invoke<BackendSettings>('get_settings');
  return mapBackendToFrontend(raw);
}

export async function updateSettings(
  settings: AppSettings,
  existingBackend?: BackendSettings,
): Promise<void> {
  const existing = existingBackend ?? (await invoke<BackendSettings>('get_settings'));
  const backend = mapFrontendToBackend(settings, existing);
  return invoke<void>('update_settings', { settings: backend });
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
  return invoke<ValidationReport>('validate_dataset_cmd');
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
  return invoke('export_audio', { segmentIds, options });
}

export async function batchAssignSpeaker(
  ids: string[],
  speakerId: string,
): Promise<{ status: string }> {
  return invoke('batch_assign_speaker', { ids, speakerId });
}

export async function batchNormalize(ids: string[]): Promise<{ status: string }> {
  return invoke('batch_normalize', { ids });
}

export async function rediarizeSegments(ids: string[]): Promise<number> {
  return invoke<number>('rediarize_segments', { ids });
}

export async function renameSpeaker(oldId: string, newId: string): Promise<number> {
  return invoke<number>('rename_speaker', { oldId, newId });
}

export async function mergeDatasetJson(
  jsonContent: string,
): Promise<{ created: number; updated: number }> {
  return invoke<{ created: number; updated: number }>('merge_dataset_json', { jsonContent });
}

export async function exportHuggingfaceDataset(outputDir: string): Promise<void> {
  return invoke<void>('export_huggingface_dataset', { path: outputDir });
}

export async function undo(): Promise<string | null> {
  return invoke<string | null>('undo');
}

export async function redo(): Promise<string | null> {
  return invoke<string | null>('redo');
}

export async function canUndo(): Promise<boolean> {
  return invoke<boolean>('can_undo');
}

export async function canRedo(): Promise<boolean> {
  return invoke<boolean>('can_redo');
}

export async function computeDiff(
  raw: string,
  annotated: string,
): Promise<{
  raw: string;
  annotated: string;
  changes: Array<{ op: string; value: string }>;
  stats: {
    added_words: number;
    removed_words: number;
    changed_words: number;
    unchanged_words: number;
    similarity: number;
  };
}> {
  return invoke('compute_diff', { raw, annotated });
}

/** Back up the live library to `dest` on a DEDICATED connection (the UI stays responsive), then
 * verify the WRITTEN file (integrity check + segment count) — a disaster copy that is itself bad
 * must fail now, not at the disaster. */
export async function dbBackup(
  dest: string,
): Promise<{ integrityOk: boolean; segmentCount: number }> {
  return invoke('db_backup', { dest });
}

/** Archive every quarantined `*.corrupt.*` artifact into `<data_dir>/quarantine/`, releasing the
 * snapshot prune-pin explicitly (bytes stay salvageable). Returns how many files were archived. */
export async function acknowledgeQuarantine(): Promise<number> {
  return invoke('acknowledge_quarantine');
}

/** Restore the live library from a backup .db file (the counterpart to dbBackup). Destructive — the
 *  backend PRAGMA integrity_check's the source before overwriting, so a corrupt file fails fast. */
export async function dbRestore(src: string): Promise<void> {
  return invoke('db_restore', { src });
}

export async function dbVacuum(): Promise<void> {
  return invoke('db_vacuum');
}

export async function modelsStatus(): Promise<
  Array<{
    name: string;
    filename: string;
    downloaded: boolean;
    exists?: boolean;
    size_bytes: number | null;
    min_size_bytes: number;
    source?: 'user' | 'bundled' | 'missing';
    downloadable?: boolean;
  }>
> {
  return invoke('models_status');
}

export async function modelsDownloadAll(): Promise<{
  downloaded: number;
  failed: number;
  total: number;
  skipped: number;
}> {
  return invoke('models_download_all');
}

export async function getInferenceStats(): Promise<{
  vad: { calls: number; failures: number; p50_ms: number; p99_ms: number };
  asr: { calls: number; failures: number; p50_ms: number; p99_ms: number };
  model_load_ms: number;
}> {
  return invoke('get_inference_stats');
}

export interface AppHealth {
  status: string;
  db_size: number;
  uptime: number;
  segment_count: number;
  memory_mb: number;
  missing_models: string[];
  missing_optional_models?: string[];
  /** Epoch seconds of the last successful auto-snapshot, or 0 if none yet this session. */
  snapshot_last_success_epoch_secs?: number;
  /** Consecutive auto-snapshot failures — a rising streak means the safety net is silently down. */
  snapshot_consecutive_failures?: number;
  /** Free bytes on the volume holding the data dir, or null when it couldn't be determined. */
  free_disk_bytes?: number | null;
}

export async function appHealth(): Promise<AppHealth> {
  return invoke('app_health');
}

/** One-line summary of the previous session's crash (surfaced once), or null if it exited cleanly. */
export async function takeLastCrash(): Promise<string | null> {
  return invoke('take_last_crash');
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
  return invoke<AgenticReadiness>('check_agentic_readiness');
}

export interface WslRefinementOptions {
  limit_files?: number;
  limit_segments?: number;
  dry_run: boolean;
  test_one: boolean;
}

export async function runWslRefinement(options: WslRefinementOptions): Promise<{ status: string }> {
  return invoke('run_wsl_refinement', {
    limitFiles: options.limit_files ?? null,
    limitSegments: options.limit_segments ?? null,
    dryRun: options.dry_run,
    testOne: options.test_one,
  });
}

export async function cancelWslRefinement(): Promise<void> {
  return invoke('cancel_wsl_refinement');
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
  return invoke<ConformalCertificate>('get_dataset_certificate', { targetError, confidenceLevel });
}

export async function computeSignalAnomalyScores(): Promise<number> {
  return invoke<number>('compute_signal_anomaly_scores');
}

export async function getActiveLearningQueue(
  targetError: number,
  confidenceLevel: number,
  limit: number,
): Promise<SpeechSegment[]> {
  return invoke<SpeechSegment[]>('get_active_learning_queue', {
    targetError,
    confidenceLevel,
    limit,
  });
}

// ── Phase 1 — Gold-Set Eval Harness ────────────────────────────────────────

import type { EvalRun, EvalRunResult, EscalationTrendPoint, LabelQualityLift } from './types';

/** Run the real pinned champion over the gold set; the renderer cannot supply a model label. */
export async function runGoldEvalAsr(): Promise<EvalRunResult> {
  return invoke<EvalRunResult>('run_gold_eval_asr');
}

/** Create gold-eval segments from a verified file. Returns the number created. */
export async function createGoldFromFile(audioPath: string): Promise<number> {
  return invoke<number>('create_gold_from_file', { audioPath });
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
  return invoke<number>('import_verified_segments_as_gold');
}

/** M2.7 / P1.6: export the gold set (manifest.jsonl + 16 kHz WAV clips) under outDir. */
export async function exportGoldEvalSet(outDir: string): Promise<GoldEvalExport> {
  return invoke<GoldEvalExport>('export_gold_eval_set', { outDir });
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
  return invoke<FinetunePackResult>('export_finetune_pack', { outDir });
}

/** P0.2: the git SHA the running binary was built from (baked at build time). Used for build-info display. */
export async function appGitSha(): Promise<string> {
  return invoke<string>('app_git_sha');
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
  return invoke<ScorecardResponse>('build_scorecard', { system, baseline: baseline ?? null });
}

export async function listEvalRuns(): Promise<EvalRun[]> {
  return invoke<EvalRun[]>('list_eval_runs');
}

export async function getLabelQualityLift(): Promise<LabelQualityLift> {
  return invoke<LabelQualityLift>('get_label_quality_lift');
}

// ── Phase 2 — T0 Gate + Jury ───────────────────────────────────────────────

export async function getEscalationQueue(limit: number): Promise<SpeechSegment[]> {
  return invoke<SpeechSegment[]>('get_escalation_queue', { limit });
}

/** Cumulative MEDIA time a reviewer actually advanced through one clip, at one revision.
 *
 * Not wall-clock, not a `play()` call, not a download — those prove the file arrived, never that
 * anyone heard it. The backend binds the receipt to the segment, the revision AND the audio
 * fingerprint, so it cannot be replayed against a different clip or survive the audio changing.
 */
export async function recordPlaybackReceipt(args: {
  segmentId: string;
  playedMs: number;
  clipDurationMs: number;
  reviewer?: string | null;
  sessionId?: string | null;
  startedAtMs?: number;
}): Promise<void> {
  // Revision and audio fingerprint are resolved by the BACKEND from the row itself; a client that
  // could name them could mint a receipt for a clip it never heard.
  return invoke<void>('record_playback_receipt', {
    segmentId: args.segmentId,
    playedMs: Math.max(0, Math.round(args.playedMs)),
    clipDurationMs: Math.max(0, Math.round(args.clipDurationMs)),
    reviewer: args.reviewer ?? null,
    sessionId: args.sessionId ?? null,
    startedAtMs: args.startedAtMs ?? Date.now(),
  });
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
  | { status: 'alreadyApplied'; segment: SpeechSegment }
  | { status: 'conflict'; segment: SpeechSegment };

export async function recordHumanDecision(
  segmentId: string,
  decision: 'accept' | 'edit' | 'reject',
  correctedTranscript?: string | null,
  timestampMs: number = Date.now(),
): Promise<HumanDecisionCommit> {
  // Freeze one client-authored identity and one payload across the bounded replay. If the backend
  // committed but the WebView lost the response, the retry resolves that exact effect instead of
  // creating a second decision. A changed payload under the same UUID is rejected server-side.
  const operationId = crypto.randomUUID();
  const args = {
    segmentId,
    decision,
    correctedTranscript: correctedTranscript ?? null,
    timestampMs,
    operationId,
  };
  try {
    return await invoke<HumanDecisionCommit>('record_human_decision', args);
  } catch {
    return invoke<HumanDecisionCommit>('record_human_decision', args);
  }
}

/** Exact server-owned inverse of one committed human decision. */
export async function undoHumanDecision(
  effectEventId: number,
  operationId: string,
): Promise<HumanDecisionUndoOutcome> {
  return invoke<HumanDecisionUndoOutcome>('undo_human_decision', { effectEventId, operationId });
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
    return await invoke<HumanFlagCommit>('record_review_flag', args);
  } catch {
    return invoke<HumanFlagCommit>('record_review_flag', args);
  }
}

/** Exact server-owned inverse of one committed review flag. */
export async function undoReviewFlag(
  effectEventId: number,
  operationId: string,
): Promise<HumanFlagUndoOutcome> {
  return invoke<HumanFlagUndoOutcome>('undo_review_flag', { effectEventId, operationId });
}

export async function getEscalationRateTrend(): Promise<EscalationTrendPoint[]> {
  return invoke<EscalationTrendPoint[]>('get_escalation_rate_trend');
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
  return invoke<JuryPipelineReport>('run_jury_pipeline', { segmentIds });
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
  return invoke<T2Result>('run_t2_for_segment', { segmentId, apiKey });
}
