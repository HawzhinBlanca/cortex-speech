import { invoke } from '@tauri-apps/api/core';
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
 * call rejects carrying this token, the UI offers the user an explicit choice — retry the champion
 * or transcribe this clip with the offline model — instead of a dead-end. The app NEVER silently
 * downgrades to a smaller model on the primary path. Keep in sync with the Rust constant.
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

/**
 * Opt-in: transcribe with the CONSTRAINED Kurdish-token CTC decode (guaranteed Kurdish-script
 * output) via the ort raw-logits path, instead of the default sherpa-onnx decode.
 */
export async function transcribeSegmentConstrained(
  audioPath: string,
  alignmentJson?: string | null,
): Promise<{ text: string; rawTranscript: string }> {
  // Pass alignmentJson so constrained decode transcribes only THIS segment's clip, not the whole file.
  return invoke('transcribe_segment_constrained', {
    audioPath,
    alignmentJson: alignmentJson ?? null,
  });
}

/**
 * Opt-in: transcribe with the embedded fine-tuned Kurdish Wav2Vec2-CTC model (measured 21.0% CER
 * [19.93, 22.04] N=900 vs stock OmniASR 29.4%). Falls back with an error if the model is not installed.
 */
export async function transcribeSegmentFinetuned(
  audioPath: string,
  alignmentJson?: string | null,
): Promise<{ text: string; rawTranscript: string }> {
  // Pass alignmentJson so the fine-tuned model transcribes only THIS segment's clip, not the whole file.
  return invoke('transcribe_segment_finetuned', {
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

/** Map a recorded ASR engine id to a short, honest human label. Unknown ids show verbatim (never
 * invented) so the review badge always names exactly what produced the draft. */
export function engineLabel(modelId: string): string {
  const id = (modelId || '').toLowerCase();
  if (id.includes('wsl-7b') || id.includes('omniasr-7b') || id === 'omniasr-llm-7b')
    return 'OmniASR-7B Champion';
  if (id.includes('finetuned') || id.includes('mms-ckb') || id.includes('mms_ctc'))
    return 'Fine-tuned MMS-1B';
  if (id.includes('ctc-1b') || id.includes('ctc_1b')) return 'OmniASR-CTC 1B (base)';
  if (id.includes('ctc-300m') || id.includes('ctc_300m')) return 'OmniASR-CTC 300M (base)';
  if (id.includes('scribe')) return 'ElevenLabs Scribe (cloud)';
  if (id.startsWith('unknown@') || id === 'unknown') return 'unknown (pre-registry)';
  return modelId;
}

/** Offline best-of-N consensus draft for a segment (ability-weighted vote over its ASR hypotheses). */
export async function getSegmentConsensus(segmentId: string): Promise<SegmentConsensus> {
  return invoke<SegmentConsensus>('get_segment_consensus', { segmentId });
}

export async function getSegments(verified?: boolean): Promise<SpeechSegment[]> {
  const data = await invoke<SpeechSegment[]>('get_segments', { verified });
  // THROW, never a benign empty result. Returning [] here turned "the IPC payload was not what this
  // app understands" into "your library is empty" — a failure that looks exactly like success. Every
  // caller of these three already has a user-visible error path (segmentStore raises a PERSISTENT
  // banner with Retry, ValidationPanel and ReviewMode toast, ReviewInbox writes a status line); the
  // silent fallback bypassed all of them and left console.error, which no user opens, as the only
  // record. An empty ValidationPanel reads as "no anomalies found" and an empty inbox as "nothing left
  // to review" — both are clean bills of health issued by a broken read.
  if (!Array.isArray(data)) {
    throw new Error(
      `get_segments returned ${typeof data}, not a segment array — the library could not be read`,
    );
  }
  return data;
}

export interface GetSegmentsPageOptions {
  verified?: boolean | null;
  query?: string | null;
  sort?: string;
  limit?: number;
  cursor?: string | null;
}

export async function getSegmentsPage(options: GetSegmentsPageOptions = {}): Promise<SegmentsPage> {
  const data = await invoke<SegmentsPage>('get_segments_page', {
    verified: options.verified ?? null,
    query: options.query ?? null,
    sort: options.sort ?? 'newest',
    limit: options.limit ?? 300,
    cursor: options.cursor ?? null,
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

/**
 * M2.5 / P1.4: segments ordered suspect-first — escalated (jury doubts) first, then lowest agent
 * confidence, then chronological. Feeds the ReviewMode suspect-first queue toggle so the reviewer
 * lands on the riskiest clips first.
 */
export async function getSegmentsSuspectFirst(verified?: boolean): Promise<SpeechSegment[]> {
  const data = await invoke<SpeechSegment[]>('get_segments_suspect_first', { verified });
  // THROW, never a benign empty result. Returning [] here turned "the IPC payload was not what this
  // app understands" into "your library is empty" — a failure that looks exactly like success. Every
  // caller of these three already has a user-visible error path (segmentStore raises a PERSISTENT
  // banner with Retry, ValidationPanel and ReviewMode toast, ReviewInbox writes a status line); the
  // silent fallback bypassed all of them and left console.error, which no user opens, as the only
  // record. An empty ValidationPanel reads as "no anomalies found" and an empty inbox as "nothing left
  // to review" — both are clean bills of health issued by a broken read.
  if (!Array.isArray(data)) {
    throw new Error(`get_segments_suspect_first returned ${typeof data}, not a segment array`);
  }
  return data;
}

export async function searchSegments(query: string): Promise<SpeechSegment[]> {
  return invoke<SpeechSegment[]>('search_segments', { query });
}

export async function updateSegment(segment: SpeechSegment): Promise<void> {
  return invoke<void>('update_segment', { segment });
}

/**
 * F10 root fix — partial autosave update. Sends ONLY the edited curation fields
 * (annotatedTranscript / speakerId / alignmentJson); the backend reads the FRESH row and applies them
 * under its lock, so a debounced save during a long batch can never merge against a stale store row
 * and revert concurrently-written columns. Resolves false when the row no longer exists (deleted
 * mid-debounce — a safe no-op, never a resurrecting upsert).
 */
/**
 * Lossless review-undo restore: writes the WHOLE pre-decision snapshot back, including the
 * jury/decision columns that updateSegment deliberately omits — so undoing a re-decision can never
 * erase the earlier human decision (the 2026-07-14 live-test data loss, reproduced + fixed).
 */
export async function restoreSegmentSnapshot(segment: SpeechSegment): Promise<void> {
  return invoke<void>('restore_segment_snapshot', { segment });
}

export async function updateSegmentFields(
  segmentId: string,
  fields: Partial<
    Pick<SpeechSegment, 'annotatedTranscript' | 'speakerId' | 'alignmentJson' | 'verified'>
  >,
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
 * (e.g. "elevenlabs", "gemini", "openrouter"). Returns names only — never key
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
export async function setApiKey(
  provider: 'gemini' | 'elevenlabs' | 'openrouter',
  key: string,
): Promise<string[]> {
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
}

/** Couch Review: the token-gated phone review server (off by default, per-session). */
export interface CouchStatus {
  running: boolean;
  /** One entry per named reviewer; empty when stopped. */
  reviewers: CouchReviewer[];
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
  checkpoint_path: string;
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
  family: string;
  checkpointPath: string;
  source: string;
  license: string;
  modelCardName?: string | null;
}): Promise<string> {
  return invoke<string>('import_model_checkpoint', {
    id: args.id,
    family: args.family,
    checkpointPath: args.checkpointPath,
    source: args.source,
    license: args.license,
    modelCardName: args.modelCardName ?? null,
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

/**
 * Transcribe one audio clip with ElevenLabs Scribe (cloud STT). Consent-gated server-side: errors
 * unless cloud-STT opt-in is enabled. Returns the transcription text.
 */
export async function transcribeWithScribe(
  audioPath: string,
  alignmentJson?: string | null,
): Promise<string> {
  // Pass alignmentJson so Scribe transcribes only THIS segment's clip, not the whole source file.
  return invoke<string>('transcribe_audio_with_scribe', {
    audioPath,
    alignmentJson: alignmentJson ?? null,
  });
}

/**
 * Add an independent ElevenLabs Scribe hypothesis (jury vote) for the given segments. Consent-gated
 * server-side. Skips segments that already have a Scribe vote; returns the number of votes added.
 */
export async function addScribeVotes(ids: string[]): Promise<number> {
  return invoke<number>('add_scribe_votes', { ids });
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

export async function batchVerify(ids: string[], verified: boolean): Promise<{ status: string }> {
  return invoke('batch_verify', { ids, verified });
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

/** The honest-CER entrypoint: runs the real ASR over the gold set (no caller-supplied hypotheses). */
export async function runGoldEvalAsr(modelId?: string | null): Promise<EvalRunResult> {
  return invoke<EvalRunResult>('run_gold_eval_asr', { modelId: modelId ?? null });
}

/** Run the gold eval against the local pipeline for a specific model id. */
export async function runGoldEvalLocal(modelId: string): Promise<EvalRunResult> {
  return invoke<EvalRunResult>('run_gold_eval_local', { modelId });
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

/** P3.4: on-demand full-SHA integrity check of the bundled fine-tuned model + vocab. Resolves to a
 *  "verified: <path>" message; rejects with a mismatch message if the model is corrupt/replaced. */
export async function verifyFinetunedModelIntegrity(): Promise<string> {
  return invoke<string>('verify_finetuned_model_integrity');
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

export async function recordHumanDecision(
  segmentId: string,
  decision: 'accept' | 'edit' | 'reject',
  correctedTranscript?: string | null,
  timestampMs: number = Date.now(),
): Promise<void> {
  // M2.1 / P1.1: send the decision timestamp so decision_log is actually populated (it was dead
  // before — the backend only inserts `if let Some(ts_ms)`, and this wrapper never passed one).
  return invoke<void>('record_human_decision', {
    segmentId,
    decision,
    correctedTranscript: correctedTranscript ?? null,
    timestampMs,
  });
}

/** P3-3: Revert a segment to unreviewed state. Use this for undo instead of re-recording a decision. */
export async function clearHumanDecision(segmentId: string): Promise<void> {
  return invoke<void>('clear_human_decision', { segmentId });
}

/** Undo a review-inbox flag(): clear the escalation it set (inverse of flag, unlike clearHumanDecision). */
export async function clearEscalation(segmentId: string): Promise<void> {
  return invoke<void>('clear_escalation', { segmentId });
}

export async function writeSegmentVerdict(
  segmentId: string,
  verdict: string,
  transcript?: string | null,
  rationale?: string | null,
  evidenceJson?: string | null,
  agreementScore?: number | null,
  escalated?: boolean,
): Promise<void> {
  return invoke<void>('write_segment_verdict', {
    segmentId,
    verdict,
    transcript: transcript ?? null,
    rationale: rationale ?? null,
    evidenceJson: evidenceJson ?? null,
    agreementScore: agreementScore ?? null,
    escalated: escalated ?? false,
  });
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
