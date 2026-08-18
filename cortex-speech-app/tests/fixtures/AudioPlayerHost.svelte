<script lang="ts">
  import { untrack } from 'svelte';
  import AudioPlayer from '../../src/lib/AudioPlayer.svelte';

  // Holds `audioPath` FIXED while `clipKey` advances — the shape ReviewMode actually has, and the
  // one @testing-library's `rerender` cannot produce (it reassigns every prop, which re-runs
  // AudioPlayer's audioPath effect and reloads the element). Consecutive review clips from one
  // recording share a path, so this is the common case, not a synthetic one.
  interface Props {
    path: string;
    clipKey: string;
  }
  let { path, clipKey }: Props = $props();
  // Capturing only the INITIAL value is the entire point of this fixture, so take it untracked
  // rather than let a later `path` change leak in.
  const fixedPath = untrack(() => path);
</script>

<AudioPlayer audioPath={fixedPath} {clipKey} autoplay={true} />
