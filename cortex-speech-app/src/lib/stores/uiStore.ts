import { writable, type Writable } from 'svelte/store';

export type PipelinePhase =
  | 'idle'
  | 'importing'
  | 'reference_transcribing'
  | 'detecting'
  | 'transcribing'
  | 'adjudicating';

export const pipelinePhase = writable<PipelinePhase>('idle');
export const filesProcessed = writable(0);
export const pipelineTotal = writable(0);
export const pipelineCurrentFile = writable('');
export const pipelineStatus = writable('');
export interface AgentPipelineStage {
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

export type ViewMode = 'transcribe' | 'curate' | 'align' | 'export';
export type SidePanel = 'segments' | 'stats' | 'none';

export const viewMode = writable<ViewMode>('transcribe');
export const leftPanel = writable<SidePanel>('segments');
export const rightPanel = writable<SidePanel>('none');
export const showKeyboardHelp = writable(false);

export interface ConfirmDialog {
  title: string;
  message: string;
  onConfirm: () => void | Promise<void>;
  onCancel?: () => void;
}

export const showConfirmDialog = writable<ConfirmDialog | null>(null);
export const isProcessing = writable(false);
export const statusMessage = writable('Ready');

export function toggleLeftPanel() {
  leftPanel.update((v) => (v === 'segments' ? 'stats' : 'segments'));
}

export function toggleRightPanel() {
  rightPanel.update((v) => (v === 'none' ? 'segments' : 'none'));
}

export function openConfirm(dialog: ConfirmDialog) {
  showConfirmDialog.set(dialog);
}

export function closeConfirm() {
  showConfirmDialog.set(null);
}

// Re-export as an object for components that use the old `uiStore` API
export const uiStore = {
  subscribe: (run: (v: Record<string, unknown>) => void) => {
    const unsub1 = viewMode.subscribe((v) => run({ viewMode: v }));
    const unsub2 = isProcessing.subscribe((v) => run({ processing: v }));
    const unsub3 = showConfirmDialog.subscribe((v) => run({ confirm: v }));
    const unsub4 = showKeyboardHelp.subscribe((v) => run({ showShortcuts: v }));
    const unsub5 = leftPanel.subscribe((v) => run({ leftPanel: v }));
    const unsub6 = rightPanel.subscribe((v) => run({ rightPanel: v }));
    return () => {
      unsub1();
      unsub2();
      unsub3();
      unsub4();
      unsub5();
      unsub6();
    };
  },
  openConfirm,
  closeConfirm,
  toggleSettings: () => {},
  setViewMode: (mode: ViewMode) => viewMode.set(mode),
  setProcessing: (v: boolean) => isProcessing.set(v),
  toggleShortcuts: () => showKeyboardHelp.update((v) => !v),
};
