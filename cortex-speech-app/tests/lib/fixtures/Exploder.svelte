<script lang="ts">
  let { explode = false }: { explode?: boolean } = $props();

  // svelte-ignore state_referenced_locally
  // Deliberate: this fixture throws while first rendering, which is exactly what a boundary must
  // catch. Reading the prop once at init is the whole point.
  if (explode) {
    throw new Error('panel exploded');
  }
</script>

<div data-testid="exploding-panel">panel content</div>
