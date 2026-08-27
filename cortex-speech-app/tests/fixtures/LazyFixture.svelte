<script lang="ts">
  let { message = 'loaded workspace' }: { message?: string } = $props();
</script>

<div data-testid="lazy-fixture">{message}</div>
