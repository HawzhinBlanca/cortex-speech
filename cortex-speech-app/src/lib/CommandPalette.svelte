<script lang="ts">
  import Modal from './Modal.svelte';
  import { globalKeyboardManager } from './keyboard';
  import { t } from './i18n';

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
  }: {
    open?: boolean;
    onClose?: () => void;
    extraCommands?: Command[];
  } = $props();

  let query = $state('');
  let activeIndex = $state(0);
  let listEl: HTMLDivElement | undefined = $state();

  const commands = $derived.by<Command[]>(() => {
    const fromShortcuts: Command[] = (globalKeyboardManager?.getAll() ?? []).map((s, i) => ({
      id: `sc-${i}`,
      label: s.description,
      category: s.category,
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

<Modal {open} {onClose} size="lg">
  <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
  <div
    role="combobox"
    aria-expanded="true"
    aria-controls="cmdk-list"
    tabindex="-1"
    onkeydown={onKeydown}
  >
    <div class="flex items-center gap-3 border-b border-line px-4 py-3.5">
      <svg
        class="text-subtle"
        width="18"
        height="18"
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        stroke-width="2"
        stroke-linecap="round"
      >
        <circle cx="11" cy="11" r="7" /><path d="m20 20-3.5-3.5" />
      </svg>
      <!-- svelte-ignore a11y_autofocus -->
      <input
        autofocus
        class="flex-1 bg-transparent text-sm text-default outline-none placeholder:text-subtle"
        placeholder="Search commands…"
        bind:value={query}
        aria-label="Search commands"
        spellcheck="false"
        autocomplete="off"
      />
      <kbd>Esc</kbd>
    </div>

    <div bind:this={listEl} id="cmdk-list" class="max-h-[58vh] overflow-auto p-2" role="listbox">
      {#each filtered as cmd, i (cmd.id)}
        <button
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
            <span class="ms-1 text-xs text-subtle">· {cmd.category}</span>
          </span>
          {#if cmd.hint}<kbd class="shrink-0">{cmd.hint}</kbd>{/if}
        </button>
      {:else}
        <div class="px-4 py-10 text-center text-sm text-subtle">{$t('cmdk.noMatches')}</div>
      {/each}
    </div>

    <div class="flex items-center gap-3 border-t border-line px-4 py-2 text-[11px] text-subtle">
      <span><kbd>↑</kbd><kbd>↓</kbd> navigate</span>
      <span><kbd>↵</kbd> run</span>
      <span class="ms-auto tnum">{filtered.length} command{filtered.length === 1 ? '' : 's'}</span>
    </div>
  </div>
</Modal>
