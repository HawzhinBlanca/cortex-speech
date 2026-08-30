<script lang="ts">
  import { untrack } from 'svelte';
  import {
    createReviewModePlaybackController,
    type ReviewModePlaybackController,
  } from './reviewModePlayback.svelte';

  let { onReady }: { onReady: (controller: ReviewModePlaybackController) => void } = $props();
  const controller = createReviewModePlaybackController({ inboxOpen: () => false });
  untrack(() => onReady(controller));
</script>
