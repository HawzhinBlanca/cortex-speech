import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { get } from 'svelte/store';
import { notifications } from './stores/notificationStore';
import { t } from './i18n';

// True-10 audit: every notification here was hardcoded English, so in the CKB locale the app went
// mixed-language exactly where pipeline status/errors need to be clearest. Module-scope translator
// (evaluated per call, so a locale switch applies immediately).
const tr = (key: string, params?: Record<string, string>) => get(t)(key, params);
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
  // 'halted' is the champion hard stop (owner rule 2026-08-11): batch_transcribe emits it INSTEAD of
  // 'completed' and it is the run's only terminal event. Leaving it out of this union is why the UI
  // sat at "transcribing" forever after a halt and never named the cause.
  type: 'started' | 'progress' | 'completed' | 'halted';
  haltedBy?: string;
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
    notifications.error(tr('events.importFailed'), { detail: tr('events.importFailedDetail') });
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
      notifications.error(tr('events.refreshFailed'), { detail: String(e) });
    }
  }
}

async function refreshAfterBatch(payload: BatchProgressEvent): Promise<void> {
  batchProgress.set({ status: 'idle', completed: 0, total: 0, percent: 0 });
  isProcessing.set(false);
  pipelinePhase.set('idle');
  agentPipelineStages.set([]);

  const batchOps: Record<string, { partial: string; success: string }> = {
    transcribe: { partial: 'events.batchTranscribePartial', success: 'events.transcribed' },
    verify: { partial: 'events.batchVerifyPartial', success: 'events.verified' },
    assign_speaker: { partial: 'events.batchSpeakerPartial', success: 'events.speakerAssigned' },
    normalize: { partial: 'events.batchNormalizePartial', success: 'events.normalized' },
  };
  // Checked FIRST, and as an error carrying its cause: a hard-stopped run must never be softened into
  // the cancelled warning or a "{ok} OK, {failed} failed" partial tally. Canon 2026-08-11 — a
  // partly-drafted dataset that looks finished is worse than a run that stopped, so the cause the
  // backend named in haltedBy is the one thing the user has to see.
  if (payload.type === 'halted') {
    notifications.error(tr('errors.transcriptionFailed'), { detail: payload.haltedBy });
  } else if (payload.cancelled) {
    notifications.warning(tr('events.batchCancelled'));
  } else if (payload.operation && batchOps[payload.operation]) {
    const keys = batchOps[payload.operation];
    if ((payload.failed ?? 0) > 0) {
      notifications.warning(
        tr(keys.partial, {
          ok: String(payload.succeeded ?? 0),
          failed: String(payload.failed ?? 0),
        }),
      );
    } else if ((payload.succeeded ?? 0) > 0) {
      notifications.success(tr(keys.success, { n: String(payload.succeeded) }));
    }
  }

  if (onBatchComplete) {
    try {
      await onBatchComplete(payload);
    } catch (e) {
      notifications.error(tr('events.batchRefreshFailed'), { detail: String(e) });
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
    notifications.error(tr('events.processingError', { file }), { detail: error });
  });
  unlisteners.push(unlistenError);

  const unlistenImportComplete = await listen<ImportComplete>('import-complete', (event) => {
    void refreshAfterImport(event.payload);
  });
  unlisteners.push(unlistenImportComplete);

  const unlistenStarted = await listen('pipeline-started', () => {
    isProcessing.set(true);
    pipelinePhase.set('importing');
    agentPipelineStages.set([]);
    notifications.info(tr('events.pipelineStarted'));
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

  // App-scoped so a finished WSL 7B refinement batch still notifies + refreshes the segment list even
  // if the WSL console panel was closed mid-run. The panel keeps its own wsl-status listener purely
  // for in-panel display (status pill / running flag) — the side effects live here, fired exactly once.
  const unlistenWslStatus = await listen<{
    status: 'completed' | 'failed' | 'cancelled';
    transcribed?: number;
    failed?: number;
    exit_code: number;
  }>('wsl-status', (event) => {
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
  unlisteners.push(unlistenWslStatus);

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
    } else if (payload.type === 'completed' || payload.type === 'halted') {
      // Both are terminal, so both must clear isProcessing/pipelinePhase/batchProgress and end the
      // operation — a halt that skipped this left the app stuck "transcribing" with no way back.
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
