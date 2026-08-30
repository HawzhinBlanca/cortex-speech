<script lang="ts">
  import { untrack } from 'svelte';
  import { createWorkstationViewController } from './workstationViewController.svelte';

  type WorkstationViewController = ReturnType<typeof createWorkstationViewController>;

  let { onReady }: { onReady: (controller: WorkstationViewController) => void } = $props();
  const controller = createWorkstationViewController(() => true);
  untrack(() => onReady(controller));
</script>
