import { writable, type Writable } from 'svelte/store';

export type PipelinePhase =
  'idle' | 'importing' | 'reference_transcribing' | 'detecting' | 'transcribing' | 'adjudicating';

export const pipelinePhase = writable<PipelinePhase>('idle');
export const filesProcessed = writable(0);
export const pipelineTotal = writable(0);
export const pipelineCurrentFile = writable('');
export const pipelineStatus = writable('');
export interface AgentPipelineStage {
  runId: string;
  stage: string;
  status: string;
  file: string;
  detail: string;
  current: number;
  total: number;
  updatedAt: number;
}

export const agentPipelineStages = writable<AgentPipelineStage[]>([]);

export function upsertAgentPipelineStage(stage: Omit<AgentPipelineStage, 'updatedAt'>): void {
  agentPipelineStages.update((stages) => {
    const next = stages.filter((existing) => existing.stage !== stage.stage);
    next.push({ ...stage, updatedAt: Date.now() });
    return next.slice(-8);
  });
}

export const showValidationPanel = writable(false);
export const showSpeakerPanel = writable(false);
export const showDatasetMerge = writable(false);
export const showWslConsole = writable(false);
export const showReviewInbox = writable(false);

export interface BatchProgress {
  status: 'idle' | 'running';
  completed: number;
  total: number;
  percent: number;
}

export const batchProgress: Writable<BatchProgress> = writable({
  status: 'idle',
  completed: 0,
  total: 0,
  percent: 0,
});

export const showKeyboardHelp = writable(false);

export interface ConfirmDialog {
  title: string;
  message: string;
  onConfirm: () => void | Promise<void>;
  onCancel?: () => void;
  /** Confirm-button label. Defaults to t('confirm'). */
  confirmLabel?: string;
  /** Cancel-button label. Defaults to t('cancel'). */
  cancelLabel?: string;
  /**
   * Style the confirm button. Defaults to `true` (destructive red) so existing delete/reset
   * dialogs are unchanged; pass `false` for a non-destructive primary action (e.g. "Try again").
   */
  danger?: boolean;
  /** Optional third action rendered between Cancel and Confirm (e.g. an alternative path). */
  secondary?: { label: string; onClick: () => void | Promise<void> };
}

export const showConfirmDialog = writable<ConfirmDialog | null>(null);
export const isProcessing = writable(false);
export const statusMessage = writable('Ready');
