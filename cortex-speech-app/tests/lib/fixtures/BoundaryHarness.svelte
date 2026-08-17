<!--
  Two sibling ErrorBoundary panels, one of which throws while rendering. Used to prove a failure is
  contained: the exploding panel shows the fallback, the healthy one keeps rendering.
-->
<script lang="ts">
  import ErrorBoundary from '../../../src/lib/ErrorBoundary.svelte';
  import Exploder from './Exploder.svelte';

  let { explode = false }: { explode?: boolean } = $props();
</script>

<ErrorBoundary>
  <Exploder {explode} />
</ErrorBoundary>

<ErrorBoundary>
  <div data-testid="healthy-panel">still fine</div>
</ErrorBoundary>
