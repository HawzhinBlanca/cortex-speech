import { fromStore } from 'svelte/store';
import * as api from './commands';
import type { AgentImportReport, AgentOrchestrationStage, AgentStageEvent } from './commands';
import { publicAgentStagePresentation, type ImportEventContext } from './events';
import { t } from './i18n';
import { notifications } from './stores/notificationStore';
import { segments } from './stores/segmentStore';
import { settings } from './stores/settingsStore';
import { agentPipelineStages } from './stores/uiStore';

export function createWorkstationDataController(isTauriAvailable: () => boolean) {
  const translation = fromStore(t);
  let segmentsLoading = $state(true);
  let latestAgentReport = $state<AgentImportReport | null>(null);
  let latestAgentStageEvents = $state<AgentStageEvent[]>([]);
  let agentHistoryGeneration = 0;

  const datasetPromotionStage = $derived.by(
    () =>
      latestAgentReport?.summary.orchestrationStages?.find(
        (stage) => stage.stage === 'dataset_promotion',
      ) ?? null,
  );
  const trainingExportBlocked = $derived(datasetPromotionStage?.status === 'blocked');
  const trainingExportTitle = $derived.by(() => {
    if (!isTauriAvailable()) return translation.current('desktopRuntimeRequired');
    const detail = trainingExportBlockDetail(datasetPromotionStage) ?? '';
    if (trainingExportBlocked) {
      return `${translation.current('exportHuggingface.blocked')}: ${detail}`;
    }
    if (datasetPromotionStage?.status === 'needs_review') {
      return `${translation.current('exportHuggingface.needsReview')}: ${detail}`;
    }
    return translation.current('exportHuggingface.label');
  });

  async function loadSettings(): Promise<void> {
    try {
      settings.set(await api.getSettings());
    } catch (error) {
      notifications.error(translation.current('settingsLoadFailed'), { cause: error });
    }
  }

  async function loadSegments(isCurrent: () => boolean = () => true): Promise<void> {
    if (!isCurrent()) return;
    segmentsLoading = true;
    try {
      await segments.load();
    } finally {
      if (isCurrent()) segmentsLoading = false;
    }
  }

  async function loadLatestAgentHistory(
    expectedRunId?: string,
    context?: ImportEventContext,
  ): Promise<void> {
    const generation = ++agentHistoryGeneration;
    const isCurrent = () =>
      generation === agentHistoryGeneration && (!context || context.isCurrent());
    if (!isTauriAvailable()) {
      if (!isCurrent()) return;
      latestAgentReport = null;
      latestAgentStageEvents = [];
      agentPipelineStages.set([]);
      return;
    }
    try {
      let candidate = expectedRunId
        ? await api.getAgentImportReportByRunId(expectedRunId)
        : ((await api.listAgentImportReports(1))[0] ?? null);
      if (!isCurrent()) return;
      if (expectedRunId && candidate?.agentRunId !== expectedRunId) candidate = null;
      const runId = candidate?.agentRunId ?? null;
      const events = runId ? await api.listAgentStageEvents(runId, 25) : [];
      if (!isCurrent()) return;
      latestAgentReport = candidate;
      latestAgentStageEvents = Array.isArray(events)
        ? events.filter((event) => event.runId === runId)
        : [];
      agentPipelineStages.set(
        latestAgentStageEvents.slice(-8).flatMap((event) => {
          const presentation = publicAgentStagePresentation(event);
          return presentation
            ? [{ ...presentation, updatedAt: new Date(event.createdAt).getTime() || Date.now() }]
            : [];
        }),
      );
    } catch (error) {
      if (!isCurrent()) return;
      notifications.error(translation.current('agentReport.loadFailed'), { cause: error });
    }
  }

  function clearAgentEvidence(): void {
    agentHistoryGeneration += 1;
    segmentsLoading = false;
    latestAgentReport = null;
    latestAgentStageEvents = [];
    agentPipelineStages.set([]);
  }

  function trainingExportBlockDetail(stage: AgentOrchestrationStage | null): string | undefined {
    return stage
      ? translation.current('agentReport.blockerCount', { count: String(stage.blockerCount) })
      : undefined;
  }

  return {
    get segmentsLoading() {
      return segmentsLoading;
    },
    set segmentsLoading(value: boolean) {
      segmentsLoading = value;
    },
    get latestAgentReport() {
      return latestAgentReport;
    },
    get latestAgentStageEvents() {
      return latestAgentStageEvents;
    },
    get datasetPromotionStage() {
      return datasetPromotionStage;
    },
    get trainingExportBlocked() {
      return trainingExportBlocked;
    },
    get trainingExportTitle() {
      return trainingExportTitle;
    },
    clearAgentEvidence,
    loadLatestAgentHistory,
    loadSegments,
    loadSettings,
    trainingExportBlockDetail,
  };
}
