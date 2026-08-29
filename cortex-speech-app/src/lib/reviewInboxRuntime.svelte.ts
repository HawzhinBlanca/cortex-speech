import { get } from 'svelte/store';
import { tick } from 'svelte';
import * as api from './commands';
import { formatPublicErrorReference } from './errorText';
import { autonomyLabelKey, t, type AutonomyLevel, type TranslationKey } from './i18n';
import { sharedDurableReviewUndo } from './durableReviewUndo.svelte';
import { withReviewOperationTimeout } from './reviewOperationTimeout';
import type { ReviewInboxDraftController } from './reviewInboxDraft.svelte';
import type { ReviewInboxQueueController } from './reviewInboxQueue.svelte';
import { settings as settingsStore, type AppSettings } from './stores/settingsStore';
import { segments } from './stores/segmentStore';

interface RuntimeDependencies {
  queue: ReviewInboxQueueController;
  draft: ReviewInboxDraftController;
  onClose: () => void;
}

export function createReviewInboxRuntimeController(deps: RuntimeDependencies) {
  const state = $state({
    autonomyLevel: 'propose' as AutonomyLevel,
    settings: null as AppSettings | null,
    status: '',
    juryRunning: false,
    closePending: false,
  });

  const publicError = (error: unknown) =>
    formatPublicErrorReference(error) ?? get(t)('inbox.error.unknown');

  async function setAutonomy(value: AutonomyLevel) {
    if (state.juryRunning) {
      state.status = get(t)('inbox.disabled.juryRunning');
      return;
    }
    const previous = state.autonomyLevel;
    state.autonomyLevel = value;
    if (!state.settings) return;
    try {
      const next = { ...state.settings, juryAutonomyLevel: value };
      await api.updateSettings(next);
      state.settings = next;
      settingsStore.set({ ...next });
      state.status = get(t)('inbox.status.autonomySet', {
        level: get(t)(autonomyLabelKey(value)),
      });
    } catch (error) {
      state.autonomyLevel = previous;
      state.status = get(t)('inbox.status.autonomyFailed', { err: publicError(error) });
    }
  }

  async function runJury() {
    const disabledKey = juryDisabledKey();
    if (disabledKey) {
      state.status = get(t)(disabledKey);
      return;
    }
    const truthLease = sharedDurableReviewUndo.beginTruthWrite();
    if (!truthLease) {
      state.status = get(t)('inbox.disabled.saving');
      return;
    }
    state.juryRunning = true;
    state.status = get(t)('inbox.status.running');
    let writerInvoked = false;
    try {
      await deps.draft.flush();
      if (!sharedDurableReviewUndo.truthWriteStillCurrent(truthLease)) {
        state.status = get(t)('inbox.disabled.saving');
        return;
      }
      const targetIds = await withReviewOperationTimeout(
        api.getSegmentIdsForView({ verified: false }),
        'E_JURY_TARGET_DISCOVERY_TIMEOUT',
      );
      if (targetIds.length === 0) {
        state.status = get(t)('inbox.status.noUnverified');
        return;
      }
      writerInvoked = true;
      const report = await withReviewOperationTimeout(
        api.runJuryPipeline(targetIds),
        'E_JURY_PIPELINE_TIMEOUT',
      );
      if (!report) throw new Error(get(t)('inbox.error.juryNoResult'));
      if (!sharedDurableReviewUndo.invalidateForNewAction(truthLease)) {
        state.status = get(t)('review.truthWriteUncertainRestart');
        return;
      }
      const settled = await sharedDurableReviewUndo.reconcileTruthProjections(truthLease, segments);
      if (!settled) {
        state.status = get(t)('review.truthProjectionReloadRequired');
        return;
      }
      state.status = get(t)('inbox.status.juryFinished', {
        t0: String(report.t0AutoAccepted ?? 0),
        t1: String(report.t1Committed ?? 0),
        t2: String(report.t2Committed ?? 0),
        esc: String(report.humanInbox ?? 0),
      });
    } catch (error) {
      if (writerInvoked) {
        sharedDurableReviewUndo.markTruthWriteAmbiguous(truthLease, error);
        state.status = get(t)('review.truthWriteUncertainRestart');
      } else {
        state.status = get(t)('inbox.status.juryFailed', { err: publicError(error) });
      }
    } finally {
      state.juryRunning = false;
      sharedDurableReviewUndo.endTruthWrite(truthLease);
    }
  }

  function juryDisabledKey(): TranslationKey | null {
    if (state.juryRunning) return 'inbox.disabled.juryRunning';
    if (sharedDurableReviewUndo.state.truthWriteAmbiguous)
      return 'review.truthWriteUncertainRestart';
    if (
      sharedDurableReviewUndo.state.inFlight ||
      sharedDurableReviewUndo.state.truthWriteInFlight ||
      sharedDurableReviewUndo.state.truthProjectionPending
    )
      return 'inbox.disabled.saving';
    switch (sharedDurableReviewUndo.state.status) {
      case 'loading':
        return 'review.undoDisabled.loading';
      case 'failed':
        return 'review.undoDisabled.failed';
      case 'reconciling':
        return 'review.undoDisabled.reconciling';
      case 'projectionStale':
        return 'review.undoDisabled.projectionStale';
      case 'ready':
      case 'none':
      case 'blocked':
        return null;
    }
  }

  async function requestClose() {
    if (state.closePending) return;
    if (state.juryRunning) {
      state.status = get(t)('inbox.disabled.juryRunning');
      void tick().then(() => deps.draft.state.textarea?.focus());
      return;
    }
    if (sharedDurableReviewUndo.blocksSurfaceTransition()) {
      state.status = get(t)(
        sharedDurableReviewUndo.state.truthWriteAmbiguous
          ? 'review.truthWriteUncertainRestart'
          : 'inbox.disabled.saving',
      );
      void tick().then(() => deps.draft.state.textarea?.focus());
      return;
    }
    state.closePending = true;
    try {
      await deps.draft.flush();
      if (sharedDurableReviewUndo.blocksSurfaceTransition()) {
        state.status = get(t)('inbox.disabled.saving');
        return;
      }
      deps.onClose();
    } catch {
      state.status = get(t)('review.closeDraftFailed');
      void tick().then(() => deps.draft.state.textarea?.focus());
    } finally {
      state.closePending = false;
    }
  }

  async function initialize() {
    const settingsLoad = api
      .getSettings()
      .then((settings) => {
        state.settings = settings;
        state.autonomyLevel = settings.juryAutonomyLevel ?? 'propose';
      })
      .catch(() => {
        // Preserve the optimistic default; persistence becomes available after a later successful load.
      });
    await Promise.all([settingsLoad, deps.queue.load()]);
  }

  return {
    state,
    publicError,
    setAutonomy,
    runJury,
    juryDisabledKey,
    requestClose,
    initialize,
  };
}

export type ReviewInboxRuntimeController = ReturnType<typeof createReviewInboxRuntimeController>;
