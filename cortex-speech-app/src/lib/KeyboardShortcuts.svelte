<script lang="ts">
  import { showKeyboardHelp } from './stores/uiStore';
  import { globalKeyboardManager } from './keyboard';
  import { t, type TranslationKey } from './i18n';
  import Modal from './Modal.svelte';

  let shortcuts = $state<ReturnType<NonNullable<typeof globalKeyboardManager>['getAll']> | null>(
    null,
  );

  $effect(() => {
    if (globalKeyboardManager) {
      shortcuts = globalKeyboardManager.getAll();
    }
  });

  const categories = [
    { id: 'general', labelKey: 'general' },
    { id: 'file', labelKey: 'fileOperations' },
    { id: 'edit', labelKey: 'editing' },
    { id: 'navigation', labelKey: 'navigation' },
    { id: 'playback', labelKey: 'playback' },
  ] as const satisfies ReadonlyArray<{ id: string; labelKey: TranslationKey }>;

  function modLabel(s: { ctrl?: boolean; shift?: boolean; alt?: boolean; key: string }) {
    const parts: string[] = [];
    if (s.ctrl) parts.push(navigator.platform.includes('Mac') ? '⌘' : 'Ctrl');
    if (s.shift) parts.push('⇧');
    if (s.alt) parts.push('⌥');
    parts.push(s.key.toUpperCase());
    return parts.join('+');
  }

  function close() {
    showKeyboardHelp.set(false);
  }
</script>

<Modal
  open={$showKeyboardHelp}
  title={$t('keyboardShortcuts')}
  size="lg"
  testid="shortcuts-modal"
  onClose={close}
>
  <div class="space-y-5 px-5 py-4">
    {#if shortcuts}
      {#each categories as cat (cat.id)}
        {#if shortcuts.filter((s) => s.category === cat.id).length > 0}
          <section>
            <h3 class="mb-1.5 text-[11px] font-semibold uppercase tracking-wider text-subtle">
              {$t(cat.labelKey)}
            </h3>
            <div class="space-y-0.5">
              <!-- Key on the loop INDEX, not s.description: descriptions are NOT unique (two navigation
                   shortcuts both read "Keyboard shortcuts (? key)" — the / and ? help chords), so keying on
                   description throws Svelte's each_key_duplicate and crashes the help modal on open. The list
                   is static and never reorders, so an index key is safe. -->
              {#each shortcuts.filter((s) => s.category === cat.id) as s, i (i)}
                <div
                  class="flex items-center justify-between gap-4 rounded-token px-2 py-1.5 text-sm transition-colors hover:bg-surface-2"
                >
                  <span class="text-muted"
                    >{s.descriptionKey ? $t(s.descriptionKey) : s.description}</span
                  >
                  <kbd class="shrink-0">{modLabel(s)}</kbd>
                </div>
              {/each}
            </div>
          </section>
        {/if}
      {/each}
    {:else}
      <p class="text-sm text-subtle">{$t('shortcuts.none')}</p>
    {/if}
  </div>
</Modal>
