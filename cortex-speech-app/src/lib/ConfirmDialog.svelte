<script lang="ts">
  import { showConfirmDialog } from './stores/uiStore';
  import { t } from './i18n';
  import Modal from './Modal.svelte';

  let dialog = $state<{ title?: string; message: string; onConfirm: () => void } | null>(null);

  $effect(() => {
    dialog = $showConfirmDialog;
  });

  function confirm() {
    if (dialog) {
      dialog.onConfirm();
      showConfirmDialog.set(null);
    }
  }

  function cancel() {
    showConfirmDialog.set(null);
  }
</script>

<!-- Routed through the shared Modal: focus trap, Escape-to-cancel, consistent
     backdrop + transitions. The safe action (Cancel) is autofocused, so a stray
     Enter dismisses rather than firing the destructive action. -->
<Modal open={!!dialog} title={dialog?.title ?? $t('confirm')} size="sm" onClose={cancel}>
  <p class="px-5 py-5 text-sm leading-relaxed text-muted">{dialog?.message}</p>

  {#snippet footer()}
    <!-- svelte-ignore a11y_autofocus -->
    <button class="btn btn-secondary" autofocus onclick={cancel}>{$t('cancel')}</button>
    <button class="btn btn-danger" onclick={confirm}>{$t('confirm')}</button>
  {/snippet}
</Modal>
