<script lang="ts">
  import { onDestroy, onMount } from 'svelte';
  import { chunkPlaybackRange, parseSourceMeta } from './alignment';
  import { t } from './i18n';
  import { registerReviewDraftFlusher } from './reviewDraftFlush';
  import { createReviewInboxDecisionController } from './reviewInboxDecisions.svelte';
  import { createReviewInboxDraftController } from './reviewInboxDraft.svelte';
  import { handleReviewInboxKeydown } from './reviewInboxKeyboard';
  import { createReviewInboxQueueController } from './reviewInboxQueue.svelte';
  import { createReviewInboxRuntimeController } from './reviewInboxRuntime.svelte';
  import ReviewInboxWorkspace from './ReviewInboxWorkspace.svelte';
  import { createReviewPlaybackController } from './reviewModePlayback.svelte';

  interface Props {
    onClose?: () => void;
  }

  let { onClose = () => {} }: Props = $props();
  const controllerRefs: {
    draft?: ReturnType<typeof createReviewInboxDraftController>;
    decisions?: ReturnType<typeof createReviewInboxDecisionController>;
    runtime?: ReturnType<typeof createReviewInboxRuntimeController>;
  } = {};

  const setStatus = (message: string) => {
    if (controllerRefs.runtime) controllerRefs.runtime.state.status = message;
  };
  const playbackController = createReviewPlaybackController({
    inboxOpen: () => false,
    onPlaybackRequired: () => setStatus($t('review.mustListen')),
  });
  const queueController = createReviewInboxQueueController({
    flushDraft: () => controllerRefs.draft!.flush(),
    resetSessionAuthority: () => {
      playbackController.resetForSelection();
      controllerRefs.decisions?.resetSession();
    },
    setStatus,
    publicError: (error) => controllerRefs.runtime!.publicError(error),
    focusEditor: () => controllerRefs.draft?.state.textarea?.focus(),
    navigationBlocked: () => controllerRefs.decisions?.editMutationBlocked() ?? false,
  });
  const draftController = createReviewInboxDraftController({
    current: queueController.current,
    currentRevision: queueController.currentRevision,
    resetSelectionAuthority: () => {
      playbackController.resetForSelection();
      controllerRefs.decisions?.resetSelection();
    },
    setStatus,
  });
  controllerRefs.draft = draftController;
  const decisionController = createReviewInboxDecisionController({
    queue: queueController,
    draft: draftController,
    playback: playbackController,
    setStatus,
  });
  controllerRefs.decisions = decisionController;
  const runtimeController = createReviewInboxRuntimeController({
    queue: queueController,
    draft: draftController,
    onClose: () => onClose(),
  });
  controllerRefs.runtime = runtimeController;

  $effect(() => {
    queueController.current();
    queueController.currentRevision();
    draftController.syncSelection();
  });

  function handleKey(event: KeyboardEvent) {
    if (decisionController.editMutationBlocked()) {
      if (event.key === 'Escape') void runtimeController.requestClose();
      return;
    }
    handleReviewInboxKeydown(event, {
      editing: () => draftController.state.editing,
      queueLength: () => queueController.state.rows.length,
      currentIndex: () => queueController.state.index,
      commitEdit: () => void decisionController.commitEdit(),
      cancelEdit: () => void draftController.cancelEdit(),
      accept: () => void decisionController.accept(),
      startEdit: () => void draftController.startEdit(),
      reject: () => void decisionController.reject(),
      togglePlayback: () => (playbackController.state.playing = !playbackController.state.playing),
      replay: () => {
        const current = queueController.current();
        playbackController.state.currentTime = current
          ? chunkPlaybackRange(parseSourceMeta(current.alignmentJson)).startTime
          : 0;
        playbackController.state.playing = true;
      },
      skip: () => void decisionController.skip(),
      flag: () => void decisionController.flag(),
      undo: () => void decisionController.undo(),
      close: () => void runtimeController.requestClose(),
      select: (index) => void queueController.select(index, true, true),
    });
  }

  const unregisterDraftFlusher = registerReviewDraftFlusher(draftController.flush);

  onMount(() => {
    void decisionController.refreshUndo();
    void runtimeController.initialize();
    window.addEventListener('keydown', handleKey);
  });

  onDestroy(() => {
    window.removeEventListener('keydown', handleKey);
    decisionController.disposeUndoProjection();
    // Reserve the final visible edit while this controller is still live, then retire every
    // selection/read continuation. The normal close path already awaits this same flush.
    const pendingFlush = draftController.flush();
    draftController.dispose();
    void pendingFlush.catch(() => undefined);
    unregisterDraftFlusher();
  });
</script>

<ReviewInboxWorkspace
  queue={queueController}
  draft={draftController}
  decisions={decisionController}
  playback={playbackController}
  runtime={runtimeController}
/>
