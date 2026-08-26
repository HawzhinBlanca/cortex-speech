<script lang="ts">
  import Search from '@lucide/svelte/icons/search';
  import Modal from './Modal.svelte';
  import { globalKeyboardManager } from './keyboard';
  import { t, type TranslationKey } from './i18n';

  export interface Command {
    id: string;
    label: string;
    category: string;
    hint?: string;
    run: () => void;
  }

  let {
    open = false,
    onClose = () => {},
    extraCommands = [],
    reviewActive = false,
  }: {
    open?: boolean;
    onClose?: () => void;
    extraCommands?: Command[];
    /** True while a review surface (Review & Correct / Review Inbox) owns the screen. The palette
     *  then lists only allowInReview shortcuts — mirroring the keyboard manager's suppression, so
     *  the palette can't run a non-review-safe global against the hidden curate selection. */
    reviewActive?: boolean;
  } = $props();

  let query = $state('');
  let activeIndex = $state(0);
  let listEl: HTMLDivElement | undefined = $state();

  const shortcutCategoryKeys: Readonly<Record<string, TranslationKey>> = {
    general: 'general',
    file: 'fileOperations',
    edit: 'editing',
    navigation: 'navigation',
    playback: 'playback',
  };

  function shortcutCategoryLabel(category: string): string {
    const key = shortcutCategoryKeys[category];
    return key ? $t(key) : category;
  }

  const commands = $derived.by<Command[]>(() => {
    const fromShortcuts: Command[] = (globalKeyboardManager?.getAll() ?? [])
      .filter((s) => !reviewActive || s.allowInReview)
      .map((s, i) => ({
        id: `sc-${i}`,
        label: s.descriptionKey ? $t(s.descriptionKey) : s.description,
        category: shortcutCategoryLabel(s.category),
        hint: globalKeyboardManager?.formatShortcut(s),
        run: s.action,
      }));
    return [...extraCommands, ...fromShortcuts];
  });

  // Subsequence fuzzy match — matches "ie" in "import export".
  function fuzzy(text: string, q: string): boolean {
    let i = 0;
    for (let c = 0; c < text.length && i < q.length; c++) {
      if (text[c] === q[i]) i++;
    }
    return i === q.length;
  }

  const filtered = $derived.by<Command[]>(() => {
    const q = query.trim().toLowerCase();
    if (!q) return commands;
    return commands.filter(
      (c) =>
        fuzzy(c.label.toLowerCase(), q) ||
        c.category.toLowerCase().includes(q) ||
        c.label.toLowerCase().includes(q),
    );
  });

  // Reset selection whenever the query or open-state changes.
  $effect(() => {
    void query;
    void open;
    activeIndex = 0;
  });

  function clamp(i: number): number {
    const n = filtered.length;
    return n === 0 ? 0 : ((i % n) + n) % n;
  }

  function scrollActiveIntoView() {
    requestAnimationFrame(() => {
      listEl
        ?.querySelector<HTMLElement>('[data-active="true"]')
        ?.scrollIntoView({ block: 'nearest' });
    });
  }

  function run(i: number) {
    const cmd = filtered[i];
    if (!cmd) return;
    onClose();
    // Defer so the modal teardown/focus-restore finishes before the action runs.
    setTimeout(() => cmd.run(), 0);
  }

  function onKeydown(e: KeyboardEvent) {
    if (e.key === 'ArrowDown') {
      e.preventDefault();
      activeIndex = clamp(activeIndex + 1);
      scrollActiveIntoView();
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      activeIndex = clamp(activeIndex - 1);
      scrollActiveIntoView();
    } else if (e.key === 'Enter') {
      e.preventDefault();
      run(activeIndex);
    }
  }
</script>

<Modal {open} {onClose} size="lg" ariaLabel={$t('cmdk.title')}>
  <div>
    <div class="flex items-center gap-3 border-b border-line px-4 py-3.5">
      <Search class="text-subtle" size={18} aria-hidden="true" />
      <!-- svelte-ignore a11y_autofocus -->
      <input
        autofocus
        role="combobox"
        aria-expanded="true"
        aria-controls="cmdk-list"
        aria-activedescendant={filtered[activeIndex]
          ? `cmdk-option-${filtered[activeIndex].id}`
          : undefined}
        class="flex-1 bg-transparent text-sm text-default outline-none placeholder:text-subtle"
        placeholder={$t('cmdk.search')}
        bind:value={query}
        aria-label={$t('cmdk.search')}
        spellcheck="false"
        autocomplete="off"
        onkeydown={onKeydown}
      />
      <kbd>Esc</kbd>
    </div>

    <div bind:this={listEl} id="cmdk-list" class="max-h-[58vh] overflow-auto p-2" role="listbox">
      {#each filtered as cmd, i (cmd.id)}
        <button
          id={`cmdk-option-${cmd.id}`}
          type="button"
          role="option"
          aria-selected={i === activeIndex}
          data-active={i === activeIndex}
          class="flex w-full items-center justify-between gap-3 rounded-token px-3 py-2 text-start text-sm transition-colors duration-100
                 {i === activeIndex
            ? 'bg-surface-3 text-default'
            : 'text-muted hover:bg-surface-2'}"
          onmouseenter={() => (activeIndex = i)}
          onclick={() => run(i)}
        >
          <span class="truncate">
            {cmd.label}
            <span class="ms-1 text-xs text-muted">· {cmd.category}</span>
          </span>
          {#if cmd.hint}<kbd class="shrink-0">{cmd.hint}</kbd>{/if}
        </button>
      {:else}
        <div class="px-4 py-10 text-center text-sm text-subtle">{$t('cmdk.noMatches')}</div>
      {/each}
    </div>

    <div class="flex items-center gap-3 border-t border-line px-4 py-2 text-[11px] text-subtle">
      <span><kbd>↑</kbd><kbd>↓</kbd> {$t('cmdk.navigate')}</span>
      <span><kbd>↵</kbd> {$t('cmdk.run')}</span>
      <span class="ms-auto tnum">{$t('cmdk.count').replace('{n}', String(filtered.length))}</span>
    </div>
  </div>
</Modal>
