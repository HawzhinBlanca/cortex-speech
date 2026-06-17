<script lang="ts">
  import { showKeyboardHelp } from './stores/uiStore';
  import { globalKeyboardManager } from './keyboard';
  import { t } from './i18n';

  let shortcuts = $state<ReturnType<NonNullable<typeof globalKeyboardManager>['getAll']> | null>(null);

  $effect(() => {
    if (globalKeyboardManager) {
      shortcuts = globalKeyboardManager.getAll();
    }
  });

  const categories = [
    { id: 'file', labelKey: 'fileOperations' },
    { id: 'edit', labelKey: 'editing' },
    { id: 'navigation', labelKey: 'navigation' },
    { id: 'playback', labelKey: 'playback' },
  ];

  function modLabel(s: { ctrl?: boolean; shift?: boolean; alt?: boolean; key: string }) {
    const parts: string[] = [];
    if (s.ctrl) parts.push(navigator.platform.includes('Mac') ? '⌘' : 'Ctrl');
    if (s.shift) parts.push('⇧');
    if (s.alt) parts.push('⌥');
    parts.push(s.key.toUpperCase());
    return parts.join('+');
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === 'Escape') showKeyboardHelp.set(false);
  }
</script>

<div
  class="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm"
  data-testid="shortcuts-modal"
  role="dialog"
  aria-modal="true"
  aria-labelledby="shortcuts-title"
  tabindex="-1"
  onkeydown={handleKeydown}
>
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div role="presentation" class="card p-6 max-w-lg w-full mx-4 max-h-[80vh] overflow-y-auto shadow-2xl" onclick={(e) => e.stopPropagation()}>
    <div class="flex items-center justify-between mb-4">
      <h2 id="shortcuts-title" class="text-lg font-semibold text-gray-100">{$t('keyboardShortcuts')}</h2>
      <button class="text-gray-400 hover:text-gray-200" onclick={() => showKeyboardHelp.set(false)} aria-label={$t('close')}>✕</button>
    </div>

    {#if shortcuts}
      {#each categories as cat}
        {#if shortcuts.filter(s => s.category === cat.id).length > 0}
          <div class="mb-4">
            <h3 class="text-xs font-semibold text-cortex-400 uppercase tracking-wider mb-2">{$t(cat.labelKey)}</h3>
            <div class="space-y-1">
              {#each shortcuts.filter(s => s.category === cat.id) as s}
                <div class="flex items-center justify-between text-sm">
                  <span class="text-gray-300">{s.description}</span>
                  <kbd class="text-xs">{modLabel(s)}</kbd>
                </div>
              {/each}
            </div>
          </div>
        {/if}
      {/each}
    {:else}
      <p class="text-sm text-cortex-400">{$t('shortcuts.none')}</p>
    {/if}

    <p class="text-xs text-cortex-500 mt-4">
      {$t('shortcuts.closeHint', { key: $t('shortcuts.esc') })}
    </p>
  </div>
</div>
