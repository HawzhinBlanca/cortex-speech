import { invoke } from '@tauri-apps/api/core';
import type { SpeechSegment, WordTimestamp, DatasetStats } from './types';
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

export async function importAudioFile(path: string): Promise<{ status: string; source?: string }> {
  return invoke('import_audio_file', { path });
}

export interface ImportStatus {
  running: boolean;
  current: number;
  total: number;
  file: string;
}

export async function getImportStatus(): Promise<ImportStatus> {
  return invoke<ImportStatus>('get_import_status');
}

export async function cancelOperation(): Promise<void> {
  return invoke<void>('cancel_operation');
}

export async function transcribeSegment(
  audioPath: string,
  alignmentJson?: string | null,
  segmentId?: string | null,
): Promise<{ text: string; rawTranscript: string }> {
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

export async function getSegments(verified?: boolean): Promise<SpeechSegment[]> {
  const data = await invoke<SpeechSegment[]>('get_segments', { verified });
  if (!Array.isArray(data)) {
    console.error('getSegments: expected array, got', typeof data);
    return [];
  }
  return data;
}

export async function searchSegments(query: string): Promise<SpeechSegment[]> {
  return invoke<SpeechSegment[]>('search_segments', { query });
}

export async function updateSegment(segment: SpeechSegment): Promise<void> {
  return invoke<void>('update_segment', { segment });
}

export async function updateSegmentBounds(
  id: string,
  startMs: number,
  endMs: number,
): Promise<void> {
  return invoke<void>('update_segment_bounds', {
    id,
    startMs: Math.round(startMs),
    endMs: Math.round(endMs),
  });
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

export interface DatasetRun {
  id: string;
  name: string;
  status: string;
  config: Record<string, unknown>;
  createdAt: string;
  completedAt: string | null;
}

export interface JobStatus {
  id: string;
  kind: string;
  status: string;
  progress: number;
  cancellable: boolean;
  summary: string | null;
  error: string | null;
  createdAt: string;
  updatedAt: string;
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

export interface BlockingValidationIssues {
  blocked: boolean;
  errorCount: number;
  warningCount: number;
  warningThreshold: number;
  errors: ValidationIssue[];
  warnings: ValidationIssue[];
}

export interface BundleExportResult {
  outputDir: string;
  production: boolean;
  manifestPath: string;
  files: string[];
  validation: BlockingValidationIssues;
}

export interface MediaGrant {
  id: string;
  path: string;
  expiresAt: string;
}

export async function exportDatasetBundle(
  path: string,
  production = false,
  warningThreshold = 0,
): Promise<BundleExportResult> {
  return invoke<BundleExportResult>('export_dataset_bundle', {
    path,
    production,
    warningThreshold,
  });
}

export async function createDatasetRun(name?: string): Promise<DatasetRun> {
  return invoke<DatasetRun>('create_dataset_run', { name: name ?? null });
}

export async function listDatasetRuns(): Promise<DatasetRun[]> {
  return invoke<DatasetRun[]>('list_dataset_runs');
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

export async function startJob(
  kind: string,
  summary?: string,
  cancellable = false,
): Promise<JobStatus> {
  return invoke<JobStatus>('start_job', {
    kind,
    summary: summary ?? null,
    cancellable,
  });
}

export async function getJobStatus(id: string): Promise<JobStatus> {
  return invoke<JobStatus>('get_job_status', { id });
}

export async function cancelJob(id: string): Promise<void> {
  return invoke<void>('cancel_job', { id });
}

export async function getBlockingValidationIssues(
  warningThreshold = 0,
): Promise<BlockingValidationIssues> {
  return invoke<BlockingValidationIssues>('get_blocking_validation_issues', { warningThreshold });
}

export async function registerMediaAsset(audioPath: string): Promise<MediaGrant> {
  return invoke<MediaGrant>('register_media_asset', { audioPath });
}

export async function getMediaAssetUrl(id: string): Promise<string> {
  return invoke<string>('get_media_asset_url', { id });
}

export async function checkExternalProvider(): Promise<{
  available: boolean;
  script?: string;
  message: string;
}> {
  return invoke('check_external_provider');
}

export async function getAudioDuration(path: string): Promise<number> {
  return invoke<number>('get_audio_duration', { path });
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

export async function getSettings(): Promise<AppSettings> {
  const raw = await invoke<BackendSettings>('get_settings');
  return mapBackendToFrontend(raw);
}

export async function updateSettings(
  settings: AppSettings,
  existingBackend?: BackendSettings,
): Promise<void> {
  const existing = existingBackend ?? await invoke<BackendSettings>('get_settings');
  const backend = mapFrontendToBackend(settings, existing);
  return invoke<void>('update_settings', { settings: backend });
}

export async function getCacheInfo(): Promise<{ entries: number; maxEntries: number }> {
  return invoke('get_cache_info');
}

export async function clearCache(): Promise<void> {
  return invoke<void>('clear_cache');
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

export async function checkAudio(path: string): Promise<{
  duration_ms: number;
  sample_rate: number;
  channels: number;
  format: string;
}> {
  return invoke('check_audio', { path });
}

export async function dbInfo(): Promise<{
  page_count: number;
  page_size: number;
  size_bytes: number;
  journal_mode: string;
  segment_count: number;
  free_pages: number;
  free_bytes: number;
  fragmentation_pct: number;
  wal_size_bytes: number;
  last_vacuum: string | null;
  suggestions: string[];
}> {
  return invoke('db_info');
}

export async function dbBackup(dest: string): Promise<void> {
  return invoke('db_backup', { dest });
}

export async function dbVacuum(): Promise<void> {
  return invoke('db_vacuum');
}

export async function dbWalCheckpoint(): Promise<void> {
  return invoke('db_wal_checkpoint');
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

export async function modelsDownload(filename: string): Promise<void> {
  return invoke('models_download', { filename });
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

export async function appHealth(): Promise<{
  status: string;
  db_size: number;
  uptime: number;
  segment_count: number;
  memory_mb: number;
  missing_models: string[];
}> {
  return invoke('app_health');
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
}

export async function runConsensusRefinery(): Promise<{
  status: string;
  segmentsUpdated: number;
  modelAbilities: Record<string, number>;
}> {
  return invoke('run_consensus_refinery');
}

export async function computeAcousticScores(): Promise<number> {
  return invoke('compute_acoustic_scores');
}

export async function getDatasetCertificate(
  targetError: number,
  confidenceLevel: number,
): Promise<ConformalCertificate> {
  return invoke<ConformalCertificate>('get_dataset_certificate', { targetError, confidenceLevel });
}

export async function computeOodScores(): Promise<number> {
  return invoke<number>('compute_ood_scores');
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

import type {
  GoldSegment,
  EvalRun,
  EvalRunResult,
  EscalationTrendPoint,
  FewShotExample,
  T0GateReport,
} from './types';

export async function importGoldSegments(
  inputs: { audioPath: string; reference: string; isHoldout?: boolean }[],
): Promise<number> {
  return invoke<number>('import_gold_segments', { inputs });
}

export async function runGoldEval(
  modelId: string,
  hypotheses: [string, string][],
): Promise<EvalRunResult> {
  return invoke<EvalRunResult>('run_gold_eval', { modelId, hypotheses });
}

export async function listEvalRuns(): Promise<EvalRun[]> {
  return invoke<EvalRun[]>('list_eval_runs');
}

export async function listGoldSegments(): Promise<GoldSegment[]> {
  return invoke<GoldSegment[]>('list_gold_segments');
}

// ── Phase 2 — T0 Gate + Jury ───────────────────────────────────────────────

export async function runT0Gate(segmentIds: string[]): Promise<T0GateReport> {
  return invoke<T0GateReport>('run_t0_gate', { segmentIds });
}

export async function getEscalationQueue(limit: number): Promise<SpeechSegment[]> {
  return invoke<SpeechSegment[]>('get_escalation_queue', { limit });
}

export async function recordHumanDecision(
  segmentId: string,
  decision: 'accept' | 'edit' | 'reject',
  correctedTranscript?: string | null,
): Promise<void> {
  return invoke<void>('record_human_decision', {
    segmentId,
    decision,
    correctedTranscript: correctedTranscript ?? null,
  });
}

/** P3-3: Revert a segment to unreviewed state. Use this for undo instead of re-recording a decision. */
export async function clearHumanDecision(segmentId: string): Promise<void> {
  return invoke<void>('clear_human_decision', { segmentId });
}

export async function writeSegmentVerdict(
  segmentId: string,
  verdict: string,
  transcript?: string | null,
  rationale?: string | null,
  evidenceJson?: string | null,
  agentConfidence?: number | null,
  escalated?: boolean,
): Promise<void> {
  return invoke<void>('write_segment_verdict', {
    segmentId,
    verdict,
    transcript: transcript ?? null,
    rationale: rationale ?? null,
    evidenceJson: evidenceJson ?? null,
    agentConfidence: agentConfidence ?? null,
    escalated: escalated ?? false,
  });
}

export async function getFewShotExamples(segmentId: string, k: number): Promise<FewShotExample[]> {
  return invoke<FewShotExample[]>('get_few_shot_examples', { segmentId, k });
}

export async function getEscalationRateTrend(): Promise<EscalationTrendPoint[]> {
  return invoke<EscalationTrendPoint[]>('get_escalation_rate_trend');
}

export async function runDpoUpdate(endpoint: string): Promise<string> {
  return invoke<string>('run_dpo_update', { endpoint });
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
