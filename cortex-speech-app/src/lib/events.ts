import { listen, type DesktopEvent, type DesktopUnlisten as UnlistenFn } from './adapters/desktop';
import { get } from 'svelte/store';
import { notifications } from './stores/notificationStore';
import { t, type Translate, type TranslationKey } from './i18n';

// True-10 audit: every notification here was hardcoded English, so in the CKB locale the app went
// mixed-language exactly where pipeline status/errors need to be clearest. Module-scope translator
// (evaluated per call, so a locale switch applies immediately).
const tr: Translate = (key, params) => get(t)(key, params);
import {
  isProcessing,
  pipelinePhase,
  filesProcessed,
  pipelineTotal,
  pipelineCurrentFile,
  pipelineStatus,
  agentPipelineStages,
  upsertAgentPipelineStage,
  batchProgress,
  type PipelinePhase,
} from './stores/uiStore';
import { isTauriRuntime } from './runtime';
import { segments } from './stores/segmentStore';
import type { CommandErrorV1, PipelineProgressStatusV1, PipelineProgressV1 } from './generated/ipc';

export type PipelineProgress = PipelineProgressV1;

export interface PipelineError {
  runId: string;
  file: string;
  code: 'IMPORT_PROCESSING_FAILED' | 'IMPORT_ENRICHMENT_FAILED';
}

function publicItemLabel(privateLabel: unknown, fallback: string): string {
  const raw = typeof privateLabel === 'string' ? privateLabel.slice(-1024) : '';
  const basename = raw.split(/[/\\]/).pop() ?? '';
  let bounded = '';
  for (const character of basename) {
    const codePoint = character.codePointAt(0) ?? 0;
    const isBidiControl =
      codePoint === 0x061c ||
      codePoint === 0x200e ||
      codePoint === 0x200f ||
      (codePoint >= 0x202a && codePoint <= 0x202e) ||
      (codePoint >= 0x2066 && codePoint <= 0x2069);
    if (codePoint > 31 && codePoint !== 127 && !isBidiControl) bounded += character;
    if (bounded.length >= 160) break;
  }
  return bounded.trim() || fallback;
}

/** Treat desktop events as untrusted wire input even though the current backend is typed. This
 * mapper is total, bounded, strips path ancestry/control characters, and never displays a raw
 * backend error left by an older executable. */
export function publicPipelineErrorPresentation(
  payload: unknown,
  translate: Translate = tr,
): { runId: string | null; file: string; detail: string } {
  let rawFile = '';
  let runId: string | null = null;
  let code: unknown;
  try {
    const record =
      payload && typeof payload === 'object' ? (payload as Record<string, unknown>) : {};
    rawFile = typeof record.file === 'string' ? record.file.slice(-1024) : '';
    runId = publicRunId(record.runId);
    code = record.code;
  } catch {
    // Hostile accessors/proxies still reduce to the closed generic presentation below.
  }
  const file = publicItemLabel(rawFile, translate('events.unknownFile'));
  const detail =
    code === 'IMPORT_ENRICHMENT_FAILED'
      ? translate('events.enrichmentErrorDetail')
      : translate('events.processingErrorDetail');
  return { runId, file, detail };
}

function isImportEnrichmentError(payload: unknown): boolean {
  try {
    return (
      !!payload &&
      typeof payload === 'object' &&
      (payload as Record<string, unknown>).code === 'IMPORT_ENRICHMENT_FAILED'
    );
  } catch {
    return false;
  }
}

const pipelineProgressStatuses = new Set<PipelineProgressStatusV1>([
  'resuming',
  'processing',
  'reference_transcribing',
  'transcribing',
  'adjudicating',
  'unknown',
]);

/** Runtime validation remains fail-closed so an older/hostile event cannot reintroduce a raw path,
 * free-form status, invalid counter, or spoofing control into visible desktop state. */
export function publicPipelineProgressPresentation(
  payload: unknown,
  translate: Translate = tr,
): {
  runId: string | null;
  current: number;
  total: number;
  file: string;
  phase: PipelinePhase;
  status: string;
} {
  let rawStatus: unknown;
  let rawRunId: unknown;
  let rawCurrent: unknown;
  let rawTotal: unknown;
  let rawFileLabel: unknown;
  try {
    const record =
      payload && typeof payload === 'object' ? (payload as Record<string, unknown>) : {};
    rawStatus = record.status;
    rawRunId = record.runId;
    rawCurrent = record.current;
    rawTotal = record.total;
    rawFileLabel = record.fileLabel;
  } catch {
    // Hostile proxies reduce to the generic, bounded presentation below.
  }
  const statusCode = pipelineProgressStatuses.has(rawStatus as PipelineProgressStatusV1)
    ? (rawStatus as PipelineProgressStatusV1)
    : 'unknown';
  const presentation: Record<
    PipelineProgressStatusV1,
    { phase: PipelinePhase; key: TranslationKey }
  > = {
    resuming: { phase: 'importing', key: 'pipeline.importing' },
    processing: { phase: 'importing', key: 'pipeline.importing' },
    reference_transcribing: {
      phase: 'reference_transcribing',
      key: 'pipeline.referenceTranscribing',
    },
    transcribing: { phase: 'transcribing', key: 'pipeline.transcribing' },
    adjudicating: { phase: 'adjudicating', key: 'pipeline.adjudicating' },
    unknown: { phase: 'importing', key: 'pipeline.importing' },
  };
  return {
    runId: publicRunId(rawRunId),
    current: publicProgressCount(rawCurrent),
    total: publicProgressCount(rawTotal),
    file: publicItemLabel(rawFileLabel, translate('events.unknownFile')),
    phase: presentation[statusCode].phase,
    status: translate(presentation[statusCode].key),
  };
}

export interface ImportComplete {
  runId: string;
  total: number;
  succeeded: number;
  failed: number;
  segmentIds?: string[];
  segmentCount?: number;
  source: 'file' | 'directory';
}

export interface ImportEnrichmentComplete {
  runId: string;
  source: 'file';
  segmentIds?: string[];
  segmentCount?: number;
}

export interface ImportWorkerSettled {
  runId: string;
  source: 'file' | 'directory';
}

export interface ImportEventContext {
  readonly runId: string;
  readonly source: 'file' | 'directory';
  readonly generation: number;
  isCurrent: () => boolean;
  hasObservedEvent: () => boolean;
  hasTerminalEvent: () => boolean;
}

export type BatchOperationKind = 'transcribe' | 'normalize';

export interface BatchEventContext {
  readonly operationId: string;
  readonly operation: BatchOperationKind;
  readonly expectedTotal: number;
  readonly generation: number;
  isCurrent: () => boolean;
  hasObservedEvent: () => boolean;
  hasTerminalEvent: () => boolean;
  terminalEvent: () => BatchProgressEvent | null;
  hasSettledEvent: () => boolean;
}

export interface BatchWorkerSettled {
  operationId: string;
  operation: BatchOperationKind;
}

export interface PipelinePhaseEvent {
  runId: string;
  phase: string;
}

export interface AgentPipelineStageEvent {
  runId: string;
  stage: string;
  status: string;
  file: string;
  detail: string;
  current: number;
  total: number;
}

function publicRunId(value: unknown): string | null {
  if (typeof value !== 'string') return null;
  const bounded = value.slice(0, 96);
  return /^[A-Za-z0-9][A-Za-z0-9_.:@+-]{0,95}$/.test(bounded) && bounded === value ? bounded : null;
}

function publicImportSource(value: unknown): 'file' | 'directory' | null {
  return value === 'file' || value === 'directory' ? value : null;
}

function strictPublicCount(value: unknown): number | null {
  return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0
    ? Math.min(value, 10_000_000)
    : null;
}

function publicImportCompleteEvent(payload: unknown): ImportComplete | null {
  try {
    if (!payload || typeof payload !== 'object') return null;
    const record = payload as Record<string, unknown>;
    const runId = publicRunId(record.runId);
    const source = publicImportSource(record.source);
    const total = strictPublicCount(record.total);
    const succeeded = strictPublicCount(record.succeeded);
    const failed = strictPublicCount(record.failed);
    if (!runId || !source || total === null || succeeded === null || failed === null) return null;

    const segmentCount = strictPublicCount(record.segmentCount);
    const segmentIds = Array.isArray(record.segmentIds)
      ? record.segmentIds
          .slice(0, 128)
          .map(publicRunId)
          .filter((value): value is string => value !== null)
      : undefined;
    return {
      runId,
      source,
      total,
      succeeded,
      failed,
      ...(segmentCount === null ? {} : { segmentCount }),
      ...(segmentIds ? { segmentIds } : {}),
    };
  } catch {
    return null;
  }
}

function publicImportEnrichmentEvent(payload: unknown): ImportEnrichmentComplete | null {
  try {
    if (!payload || typeof payload !== 'object') return null;
    const record = payload as Record<string, unknown>;
    const runId = publicRunId(record.runId);
    if (!runId || record.source !== 'file') return null;
    const segmentCount = strictPublicCount(record.segmentCount);
    const segmentIds = Array.isArray(record.segmentIds)
      ? record.segmentIds
          .slice(0, 128)
          .map(publicRunId)
          .filter((value): value is string => value !== null)
      : undefined;
    return {
      runId,
      source: 'file',
      ...(segmentCount === null ? {} : { segmentCount }),
      ...(segmentIds ? { segmentIds } : {}),
    };
  } catch {
    return null;
  }
}

function publicImportWorkerSettledEvent(payload: unknown): ImportWorkerSettled | null {
  try {
    if (!payload || typeof payload !== 'object') return null;
    const record = payload as Record<string, unknown>;
    const runId = publicRunId(record.runId);
    const source = publicImportSource(record.source);
    return runId && source ? { runId, source } : null;
  } catch {
    return null;
  }
}

export function createImportRunId(): string {
  const runId = globalThis.crypto?.randomUUID?.();
  if (!runId || !publicRunId(runId)) {
    throw new Error('Secure import run identity generation is unavailable');
  }
  return runId;
}

export function createBatchOperationId(): string {
  const operationId = globalThis.crypto?.randomUUID?.();
  if (!operationId || !publicBatchOperationId(operationId)) {
    throw new Error('Secure batch operation identity generation is unavailable');
  }
  return operationId;
}

const publicAgentStages = new Set([
  'source_reference',
  'audio_chunking',
  'multi_model_hypotheses',
  'jury_adjudication',
  'agent_report',
]);

const publicAgentStageStatuses: Readonly<Record<string, TranslationKey>> = {
  running: 'agentReport.status.running',
  completed: 'agentReport.status.completed',
  blocked: 'agentReport.status.blocked',
  not_required: 'agentReport.status.notRequired',
};

function publicProgressCount(value: unknown): number {
  return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0
    ? Math.min(value, 10_000_000)
    : 0;
}

/**
 * Desktop events are an untrusted compatibility boundary. Accept only the closed stage/status
 * vocabulary, reduce every file identity to a basename, and derive the visible detail locally.
 * Legacy `detail` and new `detailCode` values are deliberately ignored because historical native
 * diagnostics can contain absolute paths, provider payloads, or database errors.
 */
export function publicAgentStagePresentation(
  payload: unknown,
  translate: Translate = tr,
): AgentPipelineStageEvent | null {
  try {
    if (!payload || typeof payload !== 'object') return null;
    const record = payload as Record<string, unknown>;
    const runId = publicRunId(record.runId);
    if (!runId) return null;
    if (typeof record.stage !== 'string' || !publicAgentStages.has(record.stage)) return null;
    if (
      typeof record.status !== 'string' ||
      !Object.prototype.hasOwnProperty.call(publicAgentStageStatuses, record.status)
    )
      return null;
    const statusKey = publicAgentStageStatuses[record.status];
    return {
      runId,
      stage: record.stage,
      status: record.status,
      file: publicItemLabel(record.fileLabel ?? record.file, translate('events.unknownFile')),
      detail: translate(statusKey),
      current: publicProgressCount(record.current),
      total: publicProgressCount(record.total),
    };
  } catch {
    return null;
  }
}

export interface BatchProgressEvent {
  // 'halted' is the champion hard stop (owner rule 2026-08-11): batch_transcribe emits it INSTEAD of
  // 'completed' and it is the run's only terminal event. Leaving it out of this union is why the UI
  // sat at "transcribing" forever after a halt and never named the cause.
  type: 'started' | 'progress' | 'completed' | 'halted';
  error?: CommandErrorV1;
  operationId: string;
  operation: BatchOperationKind;
  total: number;
  current?: number;
  file?: string;
  status?: string;
  succeeded?: number;
  failed?: number;
  skipped?: number;
  abandoned?: number;
  cancelled?: boolean;
}

export type BatchHaltCode =
  | 'CHAMPION_UNAVAILABLE'
  | 'CHAMPION_IDENTITY_MISMATCH'
  | 'MODEL_IDENTITY_CHANGED'
  | 'TRANSCRIPTION_SOURCE_CHANGED'
  | 'AUDIO_DECODE_FAILED'
  | 'BATCH_SEGMENT_MISSING'
  | 'BATCH_TRANSCRIPT_WRITE_FAILED'
  | 'BATCH_NORMALIZATION_FAILED'
  | 'BATCH_REFINEMENT_FAILED'
  | 'BATCH_JURY_FAILED'
  | 'BATCH_WORKER_START_FAILED'
  | 'BATCH_WORKER_PANICKED'
  | 'PROCESS_INTERRUPTED'
  | 'BATCH_EVIDENCE_INVALID'
  | 'BATCH_TRANSCRIPTION_FAILED';

const batchHaltDetailKeys: Readonly<Record<BatchHaltCode, TranslationKey>> = {
  CHAMPION_UNAVAILABLE: 'events.batchHalt.championUnavailable',
  CHAMPION_IDENTITY_MISMATCH: 'events.batchHalt.championIdentityMismatch',
  MODEL_IDENTITY_CHANGED: 'events.batchHalt.modelIdentityChanged',
  TRANSCRIPTION_SOURCE_CHANGED: 'events.batchHalt.sourceChanged',
  AUDIO_DECODE_FAILED: 'events.batchHalt.audioDecodeFailed',
  BATCH_SEGMENT_MISSING: 'events.batchHalt.segmentMissing',
  BATCH_TRANSCRIPT_WRITE_FAILED: 'events.batchHalt.writeFailed',
  BATCH_NORMALIZATION_FAILED: 'events.batchHalt.normalizationFailed',
  BATCH_REFINEMENT_FAILED: 'events.batchHalt.refinementFailed',
  BATCH_JURY_FAILED: 'events.batchHalt.juryFailed',
  BATCH_WORKER_START_FAILED: 'events.batchHalt.workerStartFailed',
  BATCH_WORKER_PANICKED: 'events.batchHalt.workerPanicked',
  PROCESS_INTERRUPTED: 'events.batchHalt.processInterrupted',
  BATCH_EVIDENCE_INVALID: 'events.batchHalt.evidenceInvalid',
  BATCH_TRANSCRIPTION_FAILED: 'events.batchHalt.generic',
};

/** One shared closed vocabulary for native halt events and durable terminal-status recovery. */
export function publicBatchHaltCode(value: unknown): BatchHaltCode | null {
  return typeof value === 'string' &&
    Object.prototype.hasOwnProperty.call(batchHaltDetailKeys, value)
    ? (value as BatchHaltCode)
    : null;
}

function publicBatchOperationId(value: unknown): string | null {
  return typeof value === 'string' &&
    /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(value)
    ? value
    : null;
}

function publicBatchOperation(value: unknown): BatchOperationKind | null {
  return value === 'transcribe' || value === 'normalize' ? value : null;
}

function publicBatchHaltError(value: unknown, operationId: string): CommandErrorV1 {
  let code: BatchHaltCode = 'BATCH_TRANSCRIPTION_FAILED';
  try {
    const candidate =
      value && typeof value === 'object' ? (value as Record<string, unknown>).code : null;
    code = publicBatchHaltCode(candidate) ?? code;
  } catch {
    // Hostile accessors reduce to the closed generic code.
  }
  return {
    schema: 1,
    code,
    message: '',
    retryable: false,
    suggestedAction: null,
    operationId,
    details: {},
  };
}

/** Batch events cross a long-lived app channel and are treated as untrusted wire data. Only exact
 * operation identity/kind, closed event/status vocabularies and internally consistent bounded
 * counters may mutate the active workstation. Private native error text and path ancestry are
 * discarded here. */
export function publicBatchProgressEvent(payload: unknown): BatchProgressEvent | null {
  try {
    if (!payload || typeof payload !== 'object') return null;
    const record = payload as Record<string, unknown>;
    const operationId = publicBatchOperationId(record.operationId);
    const operation = publicBatchOperation(record.operation);
    const total = strictPublicCount(record.total);
    if (!operationId || !operation || total === null || total < 1) return null;
    if (!['started', 'progress', 'completed', 'halted'].includes(String(record.type))) return null;
    const type = record.type as BatchProgressEvent['type'];

    if (type === 'started') return { type, operationId, operation, total };

    if (type === 'progress') {
      const current = strictPublicCount(record.current);
      if (current === null || current > total) return null;
      const allowedStatuses =
        operation === 'transcribe' ? new Set(['transcribing']) : new Set(['normalizing', 'failed']);
      if (typeof record.status !== 'string' || !allowedStatuses.has(record.status)) return null;
      return {
        type,
        operationId,
        operation,
        total,
        current,
        file: publicItemLabel(record.file, tr('events.unknownFile')),
        status: record.status,
      };
    }

    const succeeded = strictPublicCount(record.succeeded);
    const failed = strictPublicCount(record.failed);
    const skipped = record.skipped === undefined ? 0 : strictPublicCount(record.skipped);
    const abandoned = strictPublicCount(record.abandoned);
    if (
      succeeded === null ||
      failed === null ||
      skipped === null ||
      abandoned === null ||
      succeeded + failed + skipped + abandoned !== total ||
      typeof record.cancelled !== 'boolean'
    ) {
      return null;
    }
    if (type === 'completed' && !record.cancelled && (failed !== 0 || abandoned !== 0)) {
      return null;
    }
    return {
      type,
      operationId,
      operation,
      total,
      succeeded,
      failed,
      ...(record.skipped === undefined ? {} : { skipped }),
      abandoned,
      cancelled: record.cancelled,
      ...(type === 'halted' ? { error: publicBatchHaltError(record.error, operationId) } : {}),
    };
  } catch {
    return null;
  }
}

function publicBatchWorkerSettledEvent(payload: unknown): BatchWorkerSettled | null {
  try {
    if (!payload || typeof payload !== 'object') return null;
    const record = payload as Record<string, unknown>;
    const operationId = publicBatchOperationId(record.operationId);
    const operation = publicBatchOperation(record.operation);
    return operationId && operation ? { operationId, operation } : null;
  } catch {
    return null;
  }
}

/** Map an untrusted terminal event to closed local copy. Native diagnostic prose is intentionally
 * ignored: it may contain paths, provider replies, database text or credentials. */
export function publicBatchHaltDetail(error: unknown, translate: Translate = tr): string {
  try {
    if (!error || typeof error !== 'object') return translate('events.batchHalt.generic');
    const code = (error as Record<string, unknown>).code;
    const validated = publicBatchHaltCode(code);
    if (!validated) {
      return translate('events.batchHalt.generic');
    }
    return translate(batchHaltDetailKeys[validated]);
  } catch {
    return translate('events.batchHalt.generic');
  }
}

export type ImportCompleteHandler = (
  payload: ImportComplete,
  context: ImportEventContext,
) => void | Promise<void>;
export type ImportEnrichmentCompleteHandler = (
  payload: ImportEnrichmentComplete,
  context: ImportEventContext,
) => void | Promise<void>;
export type ImportWorkerSettledHandler = (
  payload: ImportWorkerSettled,
  context: ImportEventContext,
) => void | Promise<void>;
export type BatchWorkerSettledHandler = (
  payload: BatchWorkerSettled,
  context: BatchEventContext,
) => void | Promise<void>;

const unlisteners: UnlistenFn[] = [];
let listenerGeneration = 0;
let activeListenerGeneration: number | null = null;

/** Subscribe to a typed desktop event without exposing the Tauri event API to components. */
export function subscribeDesktopEvent<T>(
  event: string,
  handler: (event: DesktopEvent<T>) => void,
): Promise<UnlistenFn> {
  return listen<T>(event, handler);
}

export type { DesktopEvent } from './adapters/desktop';
let onImportComplete: ImportCompleteHandler | null = null;
let onImportEnrichmentComplete: ImportEnrichmentCompleteHandler | null = null;
let onImportWorkerSettled: ImportWorkerSettledHandler | null = null;
let onBatchWorkerSettled: BatchWorkerSettledHandler | null = null;
let activeImportRunId: string | null = null;
let activeImportSource: 'file' | 'directory' | null = null;
let activeImportGeneration = 0;
let activeImportObservedEvent = false;
let activeImportTerminalEvent = false;
let activeImportEnrichmentComplete = false;
let activeBatchGeneration = 0;
let activeBatchOperationId: string | null = null;
let activeBatchOperation: BatchOperationKind | null = null;
let activeBatchExpectedTotal: number | null = null;
let activeBatchOpen = false;
let activeBatchObservedEvent = false;
let activeBatchMaxProgress = 0;
let activeBatchTerminal = false;
let activeBatchTerminalEvent: BatchProgressEvent | null = null;
let activeBatchSettled = false;

/** Bind the renderer to the caller-created run before invoking native code. A backend event can beat
 * the command response, so learning identity from the first `pipeline-started` event is unsafe. */
export function beginImportEventScope(
  runId: string,
  source: 'file' | 'directory',
): ImportEventContext {
  const validated = publicRunId(runId);
  const validatedSource = publicImportSource(source);
  if (!validated || !validatedSource) throw new Error('Invalid import event authority');
  activeImportRunId = validated;
  activeImportSource = validatedSource;
  activeImportGeneration += 1;
  activeImportObservedEvent = false;
  activeImportTerminalEvent = false;
  activeImportEnrichmentComplete = false;
  return importEventContext(validated, validatedSource, activeImportGeneration);
}

function importEventContext(
  runId: string,
  source: 'file' | 'directory',
  generation = activeImportGeneration,
): ImportEventContext {
  return {
    runId,
    source,
    generation,
    isCurrent: () =>
      activeImportRunId === runId &&
      activeImportSource === source &&
      activeImportGeneration === generation,
    hasObservedEvent: () =>
      activeImportRunId === runId &&
      activeImportSource === source &&
      activeImportGeneration === generation &&
      activeImportObservedEvent,
    hasTerminalEvent: () =>
      activeImportRunId === runId &&
      activeImportSource === source &&
      activeImportGeneration === generation &&
      activeImportTerminalEvent,
  };
}

function markImportEvent(context: ImportEventContext): void {
  if (!context.isCurrent()) return;
  activeImportObservedEvent = true;
}

/** Seal primary import events from either the physical worker-settled event or exact status
 * reconciliation. Single-file jury/report enrichment intentionally has its own later lane. */
export function markImportEventSettled(context: ImportEventContext): boolean {
  if (!context.isCurrent() || activeImportTerminalEvent) return false;
  activeImportObservedEvent = true;
  activeImportTerminalEvent = true;
  return true;
}

/** Invalidate a rejected/destroyed import without allowing delayed native events to revive it. */
export function closeImportEventScope(context: ImportEventContext): void {
  if (!context.isCurrent()) return;
  activeImportRunId = null;
  activeImportSource = null;
  activeImportGeneration += 1;
  activeImportObservedEvent = false;
  activeImportTerminalEvent = false;
  activeImportEnrichmentComplete = false;
}

function matchingImportEventContext(
  value: unknown,
  eventSource?: unknown,
): ImportEventContext | null {
  const runId = publicRunId(value);
  const source = activeImportSource;
  if (!runId || runId !== activeImportRunId || !source) return null;
  if (eventSource !== undefined && publicImportSource(eventSource) !== source) return null;
  return importEventContext(runId, source, activeImportGeneration);
}

function currentImportEventContext(
  value: unknown,
  eventSource?: unknown,
): ImportEventContext | null {
  // Settlement is a one-way boundary for primary-worker events. Native/WebView delivery can be
  // delayed, so accepting late progress could resurrect a spinner after controls reopen.
  const context = matchingImportEventContext(value, eventSource);
  return context && !activeImportTerminalEvent ? context : null;
}

function unsettledImportEventContext(
  value: unknown,
  eventSource: unknown,
): ImportEventContext | null {
  const context = matchingImportEventContext(value, eventSource);
  return context && !activeImportTerminalEvent ? context : null;
}

function currentImportEnrichmentContext(value: unknown): ImportEventContext | null {
  const context = matchingImportEventContext(value, 'file');
  return context && !activeImportEnrichmentComplete ? context : null;
}

function markImportEnrichmentComplete(context: ImportEventContext): boolean {
  if (!context.isCurrent() || context.source !== 'file' || activeImportEnrichmentComplete) {
    return false;
  }
  activeImportObservedEvent = true;
  activeImportEnrichmentComplete = true;
  return true;
}

export function beginBatchEventScope(
  operationId: string,
  operation: BatchOperationKind,
  expectedTotal: number,
): BatchEventContext {
  const validatedId = publicBatchOperationId(operationId);
  const validatedOperation = publicBatchOperation(operation);
  if (
    !validatedId ||
    !validatedOperation ||
    !Number.isSafeInteger(expectedTotal) ||
    expectedTotal < 1 ||
    expectedTotal > 100_000
  ) {
    throw new Error('Invalid batch event authority');
  }
  if (activeBatchOpen) throw new Error('A batch event authority is already active');
  activeBatchOperationId = validatedId;
  activeBatchOperation = validatedOperation;
  activeBatchExpectedTotal = expectedTotal;
  activeBatchGeneration += 1;
  activeBatchOpen = true;
  activeBatchObservedEvent = false;
  activeBatchMaxProgress = 0;
  activeBatchTerminal = false;
  activeBatchTerminalEvent = null;
  activeBatchSettled = false;
  return batchEventContext(validatedId, validatedOperation, expectedTotal, activeBatchGeneration);
}

function batchEventContext(
  operationId: string,
  operation: BatchOperationKind,
  expectedTotal: number,
  generation = activeBatchGeneration,
): BatchEventContext {
  return {
    operationId,
    operation,
    expectedTotal,
    generation,
    isCurrent: () =>
      activeBatchOpen &&
      activeBatchOperationId === operationId &&
      activeBatchOperation === operation &&
      activeBatchExpectedTotal === expectedTotal &&
      activeBatchGeneration === generation,
    hasObservedEvent: () =>
      activeBatchOpen &&
      activeBatchOperationId === operationId &&
      activeBatchOperation === operation &&
      activeBatchExpectedTotal === expectedTotal &&
      activeBatchGeneration === generation &&
      activeBatchObservedEvent,
    hasTerminalEvent: () =>
      activeBatchOpen &&
      activeBatchOperationId === operationId &&
      activeBatchOperation === operation &&
      activeBatchExpectedTotal === expectedTotal &&
      activeBatchGeneration === generation &&
      activeBatchTerminal,
    terminalEvent: () =>
      activeBatchOpen &&
      activeBatchOperationId === operationId &&
      activeBatchOperation === operation &&
      activeBatchExpectedTotal === expectedTotal &&
      activeBatchGeneration === generation
        ? activeBatchTerminalEvent
        : null,
    hasSettledEvent: () =>
      activeBatchOpen &&
      activeBatchOperationId === operationId &&
      activeBatchOperation === operation &&
      activeBatchExpectedTotal === expectedTotal &&
      activeBatchGeneration === generation &&
      activeBatchSettled,
  };
}

function currentBatchEventContext(
  operationId: unknown,
  operation: unknown,
): BatchEventContext | null {
  const validatedId = publicBatchOperationId(operationId);
  const validatedOperation = publicBatchOperation(operation);
  if (
    !validatedId ||
    !validatedOperation ||
    !activeBatchOpen ||
    activeBatchOperationId !== validatedId ||
    activeBatchOperation !== validatedOperation ||
    activeBatchExpectedTotal === null
  ) {
    return null;
  }
  return batchEventContext(
    validatedId,
    validatedOperation,
    activeBatchExpectedTotal,
    activeBatchGeneration,
  );
}

function markBatchEvent(context: BatchEventContext): void {
  if (context.isCurrent()) activeBatchObservedEvent = true;
}

function markBatchTerminalEvent(context: BatchEventContext, payload: BatchProgressEvent): boolean {
  if (!context.isCurrent() || activeBatchTerminal || activeBatchSettled) return false;
  activeBatchObservedEvent = true;
  activeBatchTerminal = true;
  activeBatchTerminalEvent = payload;
  return true;
}

export function markBatchEventSettled(context: BatchEventContext): boolean {
  if (!context.isCurrent() || activeBatchSettled) return false;
  activeBatchObservedEvent = true;
  activeBatchSettled = true;
  return true;
}

export function closeBatchEventScope(context: BatchEventContext): void {
  if (!context.isCurrent()) return;
  activeBatchOperationId = null;
  activeBatchOperation = null;
  activeBatchExpectedTotal = null;
  activeBatchGeneration += 1;
  activeBatchOpen = false;
  activeBatchObservedEvent = false;
  activeBatchMaxProgress = 0;
  activeBatchTerminal = false;
  activeBatchTerminalEvent = null;
  activeBatchSettled = false;
}

export function setImportCompleteHandler(handler: ImportCompleteHandler | null): void {
  onImportComplete = handler;
}

export function setImportEnrichmentCompleteHandler(
  handler: ImportEnrichmentCompleteHandler | null,
): void {
  onImportEnrichmentComplete = handler;
}

export function setImportWorkerSettledHandler(handler: ImportWorkerSettledHandler | null): void {
  onImportWorkerSettled = handler;
}

export function setBatchWorkerSettledHandler(handler: BatchWorkerSettledHandler | null): void {
  onBatchWorkerSettled = handler;
}

async function refreshAfterImport(
  payload: ImportComplete,
  context: ImportEventContext,
): Promise<void> {
  if (!context.isCurrent()) return;
  if (payload.source !== 'file') {
    if (payload.failed > 0) {
      notifications.warning(
        tr('events.importPartial', {
          ok: String(payload.succeeded),
          failed: String(payload.failed),
          total: String(payload.total),
        }),
      );
    } else if (payload.total > 0) {
      notifications.success(tr('events.importSuccess', { n: String(payload.total) }));
    }
  } else if (payload.failed > 0) {
    notifications.error(tr('events.importFailed'), {
      publicDetail: tr('events.importFailedDetail'),
    });
  }

  if (onImportComplete) {
    try {
      await onImportComplete(payload, context);
    } catch (e) {
      if (context.isCurrent()) notifications.error(tr('events.refreshFailed'), { cause: e });
    }
  }
}

const LISTENER_START_CANCELLED = Symbol('listener-start-cancelled');

function disposeListeners(listeners: UnlistenFn[]): void {
  for (const unlisten of listeners.splice(0)) {
    try {
      unlisten();
    } catch {
      // Teardown is best-effort per subscription, but every remaining subscription is still tried.
    }
  }
}

async function stageListener<T>(
  generation: number,
  staged: UnlistenFn[],
  eventName: string,
  handler: (event: DesktopEvent<T>) => void,
): Promise<void> {
  const unlisten = await listen<T>(eventName, (event) => {
    if (activeListenerGeneration === generation && listenerGeneration === generation) {
      handler(event);
    }
  });
  if (listenerGeneration !== generation) {
    try {
      unlisten();
    } catch {
      // The generation is already dead; there is no state left for this callback to mutate.
    }
    throw LISTENER_START_CANCELLED;
  }
  staged.push(unlisten);
}

export async function startEventListeners() {
  if (!isTauriRuntime()) {
    return;
  }

  const generation = ++listenerGeneration;
  activeListenerGeneration = null;
  disposeListeners(unlisteners);
  const staged: UnlistenFn[] = [];

  try {
    await stageListener<PipelineProgress>(generation, staged, 'pipeline-progress', (event) => {
      const progress = publicPipelineProgressPresentation(event.payload);
      const context = currentImportEventContext(progress.runId);
      if (!context) return;
      markImportEvent(context);
      isProcessing.set(true);
      pipelinePhase.set(progress.phase);
      filesProcessed.set(progress.current);
      pipelineTotal.set(progress.total);
      pipelineCurrentFile.set(progress.file);
      pipelineStatus.set(progress.status);
    });

    await stageListener<PipelineError>(generation, staged, 'pipeline-error', (event) => {
      const presentation = publicPipelineErrorPresentation(event.payload);
      const context = isImportEnrichmentError(event.payload)
        ? currentImportEnrichmentContext(presentation.runId)
        : currentImportEventContext(presentation.runId);
      if (!context) return;
      markImportEvent(context);
      notifications.error(tr('events.processingError', { file: presentation.file }), {
        publicDetail: presentation.detail,
      });
    });

    await stageListener<unknown>(generation, staged, 'import-complete', (event) => {
      const payload = publicImportCompleteEvent(event.payload);
      const context = payload ? currentImportEventContext(payload.runId, payload.source) : null;
      if (!payload || !context) return;
      // Completion is emitted from inside the worker before its RAII gate is released. It proves
      // progress, never settlement; only `import-worker-settled` may set terminal authority.
      markImportEvent(context);
      void refreshAfterImport(payload, context);
    });

    await stageListener<unknown>(generation, staged, 'import-enrichment-complete', (event) => {
      const payload = publicImportEnrichmentEvent(event.payload);
      const context = payload ? currentImportEnrichmentContext(payload.runId) : null;
      if (!payload || !context || !onImportEnrichmentComplete) return;
      if (!markImportEnrichmentComplete(context)) return;
      const reportFailure = (error: unknown) => {
        notifications.error(tr('events.refreshFailed'), { cause: error });
      };
      try {
        void Promise.resolve(onImportEnrichmentComplete(payload, context)).catch((error) => {
          if (context.isCurrent()) reportFailure(error);
        });
      } catch (error) {
        if (context.isCurrent()) reportFailure(error);
      }
    });

    // The backend emits this only after releasing its live-import gate. `import-complete` is emitted
    // from inside the worker and can race its Drop; this second edge makes a failed resume journal
    // visible in the same session without ever presenting the still-live successor as interrupted.
    await stageListener<unknown>(generation, staged, 'import-worker-settled', (event) => {
      const payload = publicImportWorkerSettledEvent(event.payload);
      const context = payload ? unsettledImportEventContext(payload.runId, payload.source) : null;
      if (!payload || !context) return;
      if (!markImportEventSettled(context)) return;
      if (onImportWorkerSettled) {
        const reportFailure = (error: unknown) => {
          notifications.error(tr('events.refreshFailed'), { cause: error });
        };
        try {
          void Promise.resolve(onImportWorkerSettled(payload, context)).catch((error) => {
            if (context.isCurrent()) reportFailure(error);
          });
        } catch (error) {
          if (context.isCurrent()) reportFailure(error);
        }
      }
    });

    await stageListener<{ runId: string; total: number }>(
      generation,
      staged,
      'pipeline-started',
      (event) => {
        const context = currentImportEventContext(event.payload.runId);
        if (!context) return;
        markImportEvent(context);
        isProcessing.set(true);
        pipelinePhase.set('importing');
        agentPipelineStages.set([]);
        notifications.info(tr('events.pipelineStarted'));
      },
    );

    await stageListener<unknown>(generation, staged, 'pipeline-agent-stage', (event) => {
      const presentation = publicAgentStagePresentation(event.payload);
      const enrichmentStage =
        presentation?.stage === 'jury_adjudication' || presentation?.stage === 'agent_report';
      const context = presentation
        ? (currentImportEventContext(presentation.runId) ??
          (enrichmentStage ? currentImportEnrichmentContext(presentation.runId) : null))
        : null;
      if (presentation && context) {
        markImportEvent(context);
        upsertAgentPipelineStage(presentation);
      }
    });

    await stageListener<PipelinePhaseEvent>(generation, staged, 'pipeline-phase', (event) => {
      // The global phase belongs only to the still-live primary import worker. Single-file jury
      // enrichment may legitimately finish after that worker settles, but its delayed
      // `adjudicating` event must not overwrite a newer batch/import's phase or leave an idle app
      // looking permanently busy. Enrichment detail remains visible through its scoped stage lane.
      const context = currentImportEventContext(event.payload.runId);
      if (!context) return;
      markImportEvent(context);
      const { phase } = event.payload;
      if (
        phase === 'importing' ||
        phase === 'reference_transcribing' ||
        phase === 'detecting' ||
        phase === 'transcribing' ||
        phase === 'adjudicating'
      ) {
        pipelinePhase.set(phase as PipelinePhase);
      }
    });

    // App-scoped so a finished WSL 7B refinement batch still notifies + refreshes the segment list even
    // if the WSL console panel was closed mid-run. The panel keeps its own wsl-status listener purely
    // for in-panel display (status pill / running flag) — the side effects live here, fired exactly once.
    await stageListener<{
      status: 'completed' | 'failed' | 'cancelled';
      transcribed?: number;
      failed?: number;
      exit_code: number;
    }>(generation, staged, 'wsl-status', (event) => {
      const { status, transcribed = 0, failed = 0, exit_code } = event.payload;
      if (status === 'completed') {
        // Honest completion: never a plain green "success" when segments failed or nothing was written.
        void segments.load();
        if (failed > 0) {
          notifications.warning(
            tr('events.wslPartial', { ok: String(transcribed), failed: String(failed) }),
          );
        } else if (transcribed > 0) {
          notifications.success(tr('events.wslDone', { n: String(transcribed) }));
        } else {
          notifications.info(tr('events.wslNothing'));
        }
      } else if (status === 'cancelled') {
        void segments.load();
        notifications.warning(tr('events.wslCancelled', { n: String(transcribed) }));
      } else {
        notifications.error(tr('events.wslFailed', { code: String(exit_code) }));
      }
    });

    await stageListener<unknown>(generation, staged, 'batch-progress', (event) => {
      const payload = publicBatchProgressEvent(event.payload);
      const context = payload
        ? currentBatchEventContext(payload.operationId, payload.operation)
        : null;
      if (!payload || !context || payload.total !== context.expectedTotal) return;

      if (payload.type === 'started' && !context.hasObservedEvent() && !context.hasSettledEvent()) {
        markBatchEvent(context);
        batchProgress.set({ status: 'running', completed: 0, total: payload.total, percent: 0 });
        if (payload.operation === 'transcribe') {
          isProcessing.set(true);
          pipelinePhase.set('transcribing');
        } else {
          isProcessing.set(true);
        }
      } else if (
        payload.type === 'progress' &&
        !context.hasTerminalEvent() &&
        !context.hasSettledEvent()
      ) {
        markBatchEvent(context);
        // Workers reserve counters atomically, but concurrent event delivery is not ordered. Once
        // exact-run progress reaches N, a delayed N-1 event must never move the workstation backwards.
        activeBatchMaxProgress = Math.max(activeBatchMaxProgress, payload.current ?? 0);
        const current = activeBatchMaxProgress;
        const total = payload.total;
        batchProgress.set({
          status: 'running',
          completed: current,
          total,
          percent: total > 0 ? Math.round((current / total) * 100) : 0,
        });
      } else if (payload.type === 'completed' || payload.type === 'halted') {
        // Terminal events are progress telemetry only. They can be delayed, lost, or disagree with
        // retained backend truth, so they never notify success or refresh authority. The exact status
        // lookup after physical worker settlement is the sole terminal authority.
        markBatchTerminalEvent(context, payload);
      }
    });

    await stageListener<unknown>(generation, staged, 'batch-worker-settled', (event) => {
      const payload = publicBatchWorkerSettledEvent(event.payload);
      const context = payload
        ? currentBatchEventContext(payload.operationId, payload.operation)
        : null;
      if (!payload || !context || !markBatchEventSettled(context) || !onBatchWorkerSettled) {
        return;
      }
      const reportFailure = (error: unknown) => {
        notifications.error(tr('events.batchRefreshFailed'), { cause: error });
      };
      try {
        void Promise.resolve(onBatchWorkerSettled(payload, context)).catch((error) => {
          if (context.isCurrent()) reportFailure(error);
        });
      } catch (error) {
        if (context.isCurrent()) reportFailure(error);
      }
    });
    if (listenerGeneration !== generation) throw LISTENER_START_CANCELLED;
    unlisteners.push(...staged);
    activeListenerGeneration = generation;
  } catch (error) {
    disposeListeners(staged);
    if (error === LISTENER_START_CANCELLED) return;
    if (listenerGeneration === generation) {
      listenerGeneration += 1;
      activeListenerGeneration = null;
    }
    throw error;
  }
}

export function stopEventListeners() {
  listenerGeneration += 1;
  activeListenerGeneration = null;
  disposeListeners(unlisteners);
  onImportComplete = null;
  onImportEnrichmentComplete = null;
  onImportWorkerSettled = null;
  onBatchWorkerSettled = null;
  activeImportRunId = null;
  activeImportSource = null;
  activeImportGeneration += 1;
  activeImportObservedEvent = false;
  activeImportTerminalEvent = false;
  activeImportEnrichmentComplete = false;
  activeBatchGeneration += 1;
  activeBatchOperationId = null;
  activeBatchOperation = null;
  activeBatchExpectedTotal = null;
  activeBatchOpen = false;
  activeBatchObservedEvent = false;
  activeBatchMaxProgress = 0;
  activeBatchTerminal = false;
  activeBatchTerminalEvent = null;
  activeBatchSettled = false;
}
