import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { notifications } from './stores/notificationStore';
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

export interface PipelineProgress {
  current: number;
  total: number;
  file: string;
  status: string;
}

export interface PipelineError {
  file: string;
  error: string;
}

export interface PipelineComplete {
  total: number;
  succeeded: number;
  failed: number;
}

export interface ImportComplete {
  total: number;
  succeeded: number;
  failed: number;
  segmentIds?: string[];
  segmentCount?: number;
  source?: 'file' | 'directory';
}

export interface PipelinePhaseEvent {
  phase: string;
}

export interface AgentPipelineStageEvent {
  stage: string;
  status: string;
  file: string;
  detail: string;
  current: number;
  total: number;
}

export interface BatchProgressEvent {
  type: 'started' | 'progress' | 'completed';
  total: number;
  current?: number;
  file?: string;
  status?: string;
  succeeded?: number;
  failed?: number;
  cancelled?: boolean;
  operation?: string;
}

export type ImportCompleteHandler = (payload: ImportComplete) => void | Promise<void>;
export type BatchCompleteHandler = (payload: BatchProgressEvent) => void | Promise<void>;

const unlisteners: UnlistenFn[] = [];
let onImportComplete: ImportCompleteHandler | null = null;
let onBatchComplete: BatchCompleteHandler | null = null;

export function setImportCompleteHandler(handler: ImportCompleteHandler | null): void {
  onImportComplete = handler;
}

export function setBatchCompleteHandler(handler: BatchCompleteHandler | null): void {
  onBatchComplete = handler;
}

async function refreshAfterImport(payload: ImportComplete): Promise<void> {
  if (payload.source !== 'file') {
    if (payload.failed > 0) {
      notifications.warning(
        `Completed: ${payload.succeeded} OK, ${payload.failed} failed (of ${payload.total})`,
      );
    } else if (payload.total > 0) {
      notifications.success(
        `Successfully processed ${payload.total} file${payload.total === 1 ? '' : 's'}`,
      );
    }
  } else if (payload.failed > 0) {
    notifications.error('Import failed', { detail: 'See pipeline error for details' });
  }

  isProcessing.set(false);
  pipelinePhase.set('idle');
  pipelineCurrentFile.set('');
  pipelineStatus.set('');
  pipelineTotal.set(0);
  filesProcessed.set(0);
  agentPipelineStages.set([]);

  if (onImportComplete) {
    try {
      await onImportComplete(payload);
    } catch (e) {
      notifications.error('Failed to refresh segments', { detail: String(e) });
    }
  }
}

async function refreshAfterBatch(payload: BatchProgressEvent): Promise<void> {
  batchProgress.set({ status: 'idle', completed: 0, total: 0, percent: 0 });
  isProcessing.set(false);
  pipelinePhase.set('idle');
  agentPipelineStages.set([]);

  if (payload.cancelled) {
    notifications.warning('Batch operation cancelled');
  } else if (payload.operation === 'transcribe') {
    if ((payload.failed ?? 0) > 0) {
      notifications.warning(
        `Batch transcribe: ${payload.succeeded ?? 0} OK, ${payload.failed ?? 0} failed`,
      );
    } else if ((payload.succeeded ?? 0) > 0) {
      notifications.success(`Transcribed ${payload.succeeded} segment(s)`);
    }
  } else if (payload.operation === 'verify') {
    if ((payload.failed ?? 0) > 0) {
      notifications.warning(
        `Batch verify: ${payload.succeeded ?? 0} OK, ${payload.failed ?? 0} failed`,
      );
    } else if ((payload.succeeded ?? 0) > 0) {
      notifications.success(`Verified ${payload.succeeded} segment(s)`);
    }
  } else if (payload.operation === 'assign_speaker') {
    if ((payload.failed ?? 0) > 0) {
      notifications.warning(
        `Batch speaker assign: ${payload.succeeded ?? 0} OK, ${payload.failed ?? 0} failed`,
      );
    } else if ((payload.succeeded ?? 0) > 0) {
      notifications.success(`Assigned speaker on ${payload.succeeded} segment(s)`);
    }
  } else if (payload.operation === 'normalize') {
    if ((payload.failed ?? 0) > 0) {
      notifications.warning(
        `Batch normalize: ${payload.succeeded ?? 0} OK, ${payload.failed ?? 0} failed`,
      );
    } else if ((payload.succeeded ?? 0) > 0) {
      notifications.success(`Normalized ${payload.succeeded} segment(s)`);
    }
  }

  if (onBatchComplete) {
    try {
      await onBatchComplete(payload);
    } catch (e) {
      notifications.error('Failed to refresh after batch operation', { detail: String(e) });
    }
  }
}

export async function startEventListeners() {
  if (!isTauriRuntime()) {
    return;
  }

  const unlistenProgress = await listen<PipelineProgress>('pipeline-progress', (event) => {
    const { current, total, file, status } = event.payload;
    isProcessing.set(true);
    if (status === 'Building whole-file reference transcript') {
      pipelinePhase.set('reference_transcribing');
    } else if (status.toLowerCase().includes('adjudicat')) {
      pipelinePhase.set('adjudicating');
    } else if (status.toLowerCase().includes('transcribing chunk')) {
      pipelinePhase.set('transcribing');
    } else {
      pipelinePhase.set('importing');
    }
    filesProcessed.set(current);
    pipelineTotal.set(total);
    pipelineCurrentFile.set(file);
    pipelineStatus.set(status);
  });
  unlisteners.push(unlistenProgress);

  const unlistenError = await listen<PipelineError>('pipeline-error', (event) => {
    const { file, error } = event.payload;
    notifications.error(`Error processing ${file}`, { detail: error });
  });
  unlisteners.push(unlistenError);

  const unlistenComplete = await listen<PipelineComplete>('pipeline-complete', () => {
    // Legacy event — import-complete drives segment refresh.
  });
  unlisteners.push(unlistenComplete);

  const unlistenImportComplete = await listen<ImportComplete>('import-complete', (event) => {
    void refreshAfterImport(event.payload);
  });
  unlisteners.push(unlistenImportComplete);

  const unlistenStarted = await listen('pipeline-started', () => {
    isProcessing.set(true);
    pipelinePhase.set('importing');
    agentPipelineStages.set([]);
    notifications.info('Pipeline started');
  });
  unlisteners.push(unlistenStarted);

  const unlistenAgentStage = await listen<AgentPipelineStageEvent>(
    'pipeline-agent-stage',
    (event) => {
      upsertAgentPipelineStage(event.payload);
    },
  );
  unlisteners.push(unlistenAgentStage);

  const unlistenPhase = await listen<PipelinePhaseEvent>('pipeline-phase', (event) => {
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
  unlisteners.push(unlistenPhase);

  const unlistenBatch = await listen<BatchProgressEvent>('batch-progress', (event) => {
    const payload = event.payload;
    if (payload.type === 'started') {
      batchProgress.set({ status: 'running', completed: 0, total: payload.total, percent: 0 });
      if (payload.operation === 'transcribe') {
        isProcessing.set(true);
        pipelinePhase.set('transcribing');
      } else {
        isProcessing.set(true);
      }
    } else if (payload.type === 'progress') {
      const current = payload.current ?? 0;
      const total = payload.total;
      batchProgress.set({
        status: 'running',
        completed: current,
        total,
        percent: total > 0 ? Math.round((current / total) * 100) : 0,
      });
    } else if (payload.type === 'completed') {
      void refreshAfterBatch(payload);
    }
  });
  unlisteners.push(unlistenBatch);
}

export function stopEventListeners() {
  unlisteners.forEach((fn) => fn());
  unlisteners.length = 0;
  onImportComplete = null;
  onBatchComplete = null;
}
