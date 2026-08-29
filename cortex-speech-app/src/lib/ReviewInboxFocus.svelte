<script lang="ts">
  import { t } from './i18n';
  import ReviewInboxAudio from './ReviewInboxAudio.svelte';
  import type { ReviewInboxDecisionController } from './reviewInboxDecisions.svelte';
  import type { ReviewInboxDraftController } from './reviewInboxDraft.svelte';
  import ReviewInboxDraftEditor from './ReviewInboxDraftEditor.svelte';
  import ReviewInboxEvidence from './ReviewInboxEvidence.svelte';
  import type { ReviewPlaybackController } from './reviewModePlayback.svelte';
  import type { SpeechSegment } from './types';

  interface Props {
    current: SpeechSegment;
    revision: number | undefined;
    autoplay: boolean;
    status: string;
    playback: ReviewPlaybackController;
    draft: ReviewInboxDraftController;
    decisions: ReviewInboxDecisionController;
    mutationBlocked?: boolean;
  }

  let {
    current,
    revision,
    autoplay,
    status,
    playback,
    draft,
    decisions,
    mutationBlocked = false,
  }: Props = $props();
</script>

<article class="focus-card" aria-label={$t('inbox.segmentQueue')}>
  <ReviewInboxEvidence {current} />
  <ReviewInboxAudio
    {current}
    {revision}
    {autoplay}
    {playback}
    {draft}
    {decisions}
    {mutationBlocked}
  />
  <ReviewInboxDraftEditor {draft} {decisions} {status} {mutationBlocked} />
</article>

<style>
  .focus-card {
    display: flex;
    flex: 1;
    min-width: 0;
    min-height: 0;
    flex-direction: column;
    gap: 16px;
    overflow-y: auto;
    padding: 20px 24px;
  }
  @media (max-width: 480px) {
    .focus-card {
      flex: 1 1 auto;
      width: 100%;
      padding: 14px 10px;
    }
  }
</style>
