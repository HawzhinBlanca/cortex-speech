<script lang="ts">
  import { showConfirmDialog } from './stores/uiStore';
  import { t } from './i18n';

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

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === 'Escape') cancel();
    if (e.key === 'Enter') confirm();
  }
</script>

{#if dialog}
  <div
    class="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm"
    role="dialog"
    aria-modal="true"
    aria-labelledby="confirm-title"
    tabindex="-1"
    onkeydown={handleKeydown}
  >
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div role="presentation" class="card p-6 max-w-md w-full mx-4 shadow-2xl" onclick={(e) => e.stopPropagation()}>
      <h3 id="confirm-title" class="text-lg font-semibold text-gray-100 mb-2">{dialog.title ?? $t('confirm')}</h3>
      <p class="text-sm text-gray-300 mb-6">{dialog.message}</p>
      <div class="flex justify-end gap-3">
        <button class="btn-secondary" onclick={cancel}>{$t('cancel')}</button>
        <button class="btn-danger" onclick={confirm}>{$t('confirm')}</button>
      </div>
    </div>
  </div>
{/if}
