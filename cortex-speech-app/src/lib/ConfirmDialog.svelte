<script lang="ts">
  import { showConfirmDialog, type ConfirmDialog } from './stores/uiStore';
  import { t } from './i18n';
  import Modal from './Modal.svelte';

  let dialog = $state<ConfirmDialog | null>(null);

  $effect(() => {
    dialog = $showConfirmDialog;
  });

  // Close BEFORE running the handler so a handler that opens a fresh dialog (e.g. "Try again" that
  // fails and re-prompts) isn't immediately wiped by our own close.
  async function confirm() {
    const d = dialog;
    showConfirmDialog.set(null);
    await d?.onConfirm();
  }

  async function runSecondary() {
    const d = dialog;
    showConfirmDialog.set(null);
    await d?.secondary?.onClick();
  }

  function cancel() {
    const d = dialog;
    showConfirmDialog.set(null);
    d?.onCancel?.();
  }
</script>

<!-- Routed through the shared Modal: focus trap, Escape-to-cancel, consistent
     backdrop + transitions. The safe action (Cancel) is autofocused, so a stray
     Enter dismisses rather than firing the destructive action. -->
<Modal open={!!dialog} title={dialog?.title ?? $t('confirm')} size="sm" onClose={cancel}>
  <p class="px-5 py-5 text-sm leading-relaxed text-muted">{dialog?.message}</p>

  {#snippet footer()}
    <!-- svelte-ignore a11y_autofocus -->
    <button class="btn btn-secondary" autofocus onclick={cancel}
      >{dialog?.cancelLabel ?? $t('cancel')}</button
    >
    {#if dialog?.secondary}
      <button class="btn btn-secondary" onclick={runSecondary}>{dialog.secondary.label}</button>
    {/if}
    <button class="btn {dialog?.danger === false ? 'btn-primary' : 'btn-danger'}" onclick={confirm}
      >{dialog?.confirmLabel ?? $t('confirm')}</button
    >
  {/snippet}
</Modal>
