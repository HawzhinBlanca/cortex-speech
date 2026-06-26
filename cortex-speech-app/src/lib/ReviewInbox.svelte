<script lang="ts">
  /**
   * ReviewInbox.svelte — Phase 3: The Review Inbox
   *
   * Single-item focus card + queue rail (riskiest first) with full Prodigy
   * keyboard map: a accept · e edit · x reject · space skip · f flag · ⌫ undo
   *
   * RTL rules: Sorani text in dir="rtl" / lang="ckb" blocks.
   * LTR exceptions: waveform, timecodes, model names (wrapped in <bdi>).
   * Confidence shown as band + icon + verb — never as a raw float.
   */

  import { onMount, onDestroy, tick } from 'svelte';
  import * as api from './commands';
  import AudioPlayer from './AudioPlayer.svelte';
  import { parseSourceMeta, chunkPlaybackRange } from './alignment';
  import type { SpeechSegment } from './types';

  // ── Props ───────────────────────────────────────────────────────────────────
  export let onClose: () => void = () => {};

  // ── State ───────────────────────────────────────────────────────────────────
  let queue: SpeechSegment[] = [];
  let currentIndex = 0;
  let isLoading = false;
  let isEditing = false;
  let editText = '';
  let editTextarea: HTMLTextAreaElement | null = null;
  let statusMsg = '';
  let history: { id: string; decision: string; prev: SpeechSegment }[] = [];
  let autonomyLevel: 'observe' | 'propose' | 'act_confirm' | 'act_auto' = 'propose';
  // Persisted jury autonomy (mirrors Settings). Seeded on mount, written on dial change.
  let appSettings: Awaited<ReturnType<typeof api.getSettings>> | null = null;
  // Guard against double-submission from rapid key presses.
  let isSubmitting = false;

  $: current = queue[currentIndex] ?? null;
  $: pendingCount = queue.filter((s) => !s.humanDecision).length;
  // Bound the player to this segment's window so Play hears only this clip, not the whole file.
  $: inboxRange = current
    ? chunkPlaybackRange(parseSourceMeta(current.alignmentJson))
    : { startTime: 0, endTime: 0 };

  // ── Confidence bands ─────────────────────────────────────────────────────────
  function confidenceBand(conf: number | null | undefined): {
    label: string;
    icon: string;
    color: string;
  } {
    if (conf == null)
      return { label: 'Unknown confidence', icon: '❓', color: 'var(--text-subtle)' };
    if (conf >= 0.9)
      return {
        label: `AI is very confident (${Math.round(conf * 100)}%) — quick glance 👀`,
        icon: '✅',
        color: 'var(--success)',
      };
    if (conf >= 0.75)
      return {
        label: `AI is fairly sure (${Math.round(conf * 100)}%) — quick listen 👂`,
        icon: '🟡',
        color: 'var(--warning)',
      };
    if (conf >= 0.55)
      return {
        label: `AI is unsure (${Math.round(conf * 100)}%) — listen carefully ⚠`,
        icon: '⚠️',
        color: 'rgb(var(--orange-400-rgb))',
      };
    return {
      label: `AI has low confidence (${Math.round(conf * 100)}%) — careful review needed 🔴`,
      icon: '🔴',
      color: 'var(--danger)',
    };
  }

  // ── Queue loading ─────────────────────────────────────────────────────────────
  async function loadQueue() {
    isLoading = true;
    try {
      queue = await api.getEscalationQueue(200);
      currentIndex = 0;
      // Drop the undo stack: it references the PREVIOUS queue's segments. A stale undo after a
      // reload would fire a backend clear against a segment no longer in view.
      history = [];
    } catch (e) {
      statusMsg = `Failed to load queue: ${e}`;
    } finally {
      isLoading = false;
    }
  }

  // ── Autonomy dial: real settings round-trip (not a cosmetic local toggle) ──────
  async function loadAutonomy() {
    try {
      appSettings = await api.getSettings();
      autonomyLevel = appSettings.juryAutonomyLevel;
    } catch (e) {
      statusMsg = `Failed to load settings: ${e}`;
    }
  }

  async function setAutonomy(val: typeof autonomyLevel) {
    autonomyLevel = val;
    if (!appSettings) return;
    appSettings = { ...appSettings, juryAutonomyLevel: val };
    try {
      await api.updateSettings(appSettings);
    } catch (e) {
      statusMsg = `Failed to save autonomy level: ${e}`;
    }
  }

  let isRunningJury = false;

  async function triggerJuryPipeline() {
    if (isRunningJury) return;
    isRunningJury = true;
    statusMsg = '⏳ Running Jury Pipeline...';
    try {
      const allSegs = await api.getSegments();
      const targetIds = allSegs.filter((s) => !s.verified).map((s) => s.id);
      if (targetIds.length === 0) {
        statusMsg = 'ℹ️ No unverified segments to run jury on.';
        isRunningJury = false;
        return;
      }
      const report = await api.runJuryPipeline(targetIds);
      if (!report) throw new Error('Jury pipeline returned no result');
      statusMsg = `⚡ Jury finished! T0 accepted: ${report.t0AutoAccepted ?? 0}, T1 committed: ${report.t1Committed ?? 0}, T2 committed: ${report.t2Committed ?? 0}, Escalated: ${report.humanInbox ?? 0}`;
      await loadQueue();
    } catch (e) {
      statusMsg = `❌ Jury pipeline failed: ${e}`;
    } finally {
      isRunningJury = false;
    }
  }

  // ── Actions ──────────────────────────────────────────────────────────────────
  async function accept() {
    if (!current || isSubmitting) return;
    // Snapshot the target before the await — currentIndex/current can change mid-flight if the user
    // clicks another rail item (the rail is not disabled during submit), which would otherwise stamp
    // this decision onto the wrong segment's queue slot.
    const cur = current;
    const idx = currentIndex;
    isSubmitting = true;
    try {
      history.push({ id: cur.id, decision: 'accept', prev: { ...cur } });
      await api.recordHumanDecision(cur.id, 'accept', null);
      queue[idx] = { ...cur, humanDecision: 'accept' };
      statusMsg = '✅ Accepted';
      advance();
    } catch (e) {
      // The decision did not persist: drop the phantom undo entry pushed above and
      // tell the reviewer, rather than silently swallowing it (unhandled rejection).
      history.pop();
      statusMsg = `Failed to accept: ${e}`;
    } finally {
      isSubmitting = false;
    }
  }

  async function startEdit() {
    if (!current) return;
    editText = current.verdictTranscript ?? current.rawTranscript ?? '';
    isEditing = true;
    await tick();
    editTextarea?.focus();
    editTextarea?.select();
  }

  async function commitEdit() {
    if (!current || !editText.trim() || isSubmitting) return;
    const cur = current;
    const idx = currentIndex;
    const text = editText.trim();
    isSubmitting = true;
    try {
      history.push({ id: cur.id, decision: 'edit', prev: { ...cur } });
      await api.recordHumanDecision(cur.id, 'edit', text);
      queue[idx] = {
        ...cur,
        humanDecision: 'edit',
        verdictTranscript: text,
      };
      isEditing = false;
      statusMsg = '✏️ Edited';
      advance();
    } catch (e) {
      history.pop();
      statusMsg = `Failed to save edit: ${e}`;
    } finally {
      isSubmitting = false;
    }
  }

  async function reject() {
    if (!current || isSubmitting) return;
    const cur = current;
    const idx = currentIndex;
    isSubmitting = true;
    try {
      history.push({ id: cur.id, decision: 'reject', prev: { ...cur } });
      await api.recordHumanDecision(cur.id, 'reject', null);
      queue[idx] = { ...cur, humanDecision: 'reject' };
      statusMsg = '❌ Rejected';
      advance();
    } catch (e) {
      history.pop();
      statusMsg = `Failed to reject: ${e}`;
    } finally {
      isSubmitting = false;
    }
  }

  function skip() {
    if (!current) return;
    statusMsg = '⏭ Skipped';
    advance();
  }

  // Guard against a malformed evidence_json (truncated / legacy / externally-written row): a raw
  // JSON.parse in the template throws synchronously during render and breaks the focus card so the
  // segment can't be adjudicated. Fall back to showing the raw string.
  function safeEvidence(j: string | null | undefined): string {
    try {
      return JSON.stringify(JSON.parse(j ?? '[]'), null, 2);
    } catch {
      return j ?? '';
    }
  }

  async function flag() {
    if (!current || isSubmitting) return;
    const cur = current;
    const idx = currentIndex;
    isSubmitting = true;
    try {
      await api.writeSegmentVerdict(
        cur.id,
        'escalated',
        null,
        'Flagged for second-pass adjudication',
        null,
        null,
        true,
      );
      queue[idx] = { ...cur, escalated: true };
      statusMsg = '🚩 Flagged for second pass';
      advance();
    } catch (e) {
      // flag() records no undo history, so just surface the failure.
      statusMsg = `Failed to flag: ${e}`;
    } finally {
      isSubmitting = false;
    }
  }

  async function undo() {
    const last = history.pop();
    if (!last) return;
    try {
      // P3-3: Clear the human decision entirely (set to NULL) instead of
      // overwriting it with a fake 'accept' — that was corrupting agent_examples.
      await api.clearHumanDecision(last.id);
      const idx = queue.findIndex((s) => s.id === last.id);
      if (idx >= 0) {
        queue[idx] = { ...last.prev };
        // Navigate directly to the undone segment, not blindly to currentIndex-1.
        // If the user accepted seg#5 then scrolled to seg#10, undo should show seg#5.
        currentIndex = idx;
      }
      statusMsg = '↩ Undone';
    } catch (e) {
      // The decision was NOT cleared — put the history entry back so the undo can be
      // retried, and tell the user instead of failing silently (which previously also
      // dropped the entry, making the undo permanently unretryable).
      history.push(last);
      statusMsg = `Failed to undo: ${e}`;
    }
  }

  function advance() {
    if (currentIndex < queue.length - 1) {
      currentIndex++;
    }
  }

  // ── Keyboard handler ─────────────────────────────────────────────────────────
  function handleKey(e: KeyboardEvent) {
    if (isEditing) {
      if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) {
        e.preventDefault();
        commitEdit();
      }
      if (e.key === 'Escape') {
        e.preventDefault();
        isEditing = false;
      }
      return;
    }
    switch (e.key) {
      case 'a':
        e.preventDefault();
        accept();
        break;
      case 'e':
        e.preventDefault();
        startEdit();
        break;
      case 'x':
        e.preventDefault();
        reject();
        break;
      case ' ':
        e.preventDefault();
        skip();
        break;
      case 'f':
        e.preventDefault();
        flag();
        break;
      case 'Backspace':
        e.preventDefault();
        undo();
        break;
      case 'Escape':
        e.preventDefault();
        onClose();
        break;
      default:
        if (e.key >= '1' && e.key <= '9') {
          const idx = parseInt(e.key) - 1;
          if (idx < queue.length) {
            currentIndex = idx;
          }
        }
    }
  }

  onMount(() => {
    loadQueue();
    loadAutonomy();
    window.addEventListener('keydown', handleKey);
  });

  onDestroy(() => {
    window.removeEventListener('keydown', handleKey);
  });
</script>

<!-- ── Root container ──────────────────────────────────────────────────────── -->
<div class="inbox-root" role="main" aria-label="Review Inbox">
  <!-- Header -->
  <div class="inbox-header">
    <div class="inbox-title">
      <span class="inbox-icon">📬</span>
      <h2>Review Inbox</h2>
      {#if pendingCount > 0}
        <span class="inbox-badge">{pendingCount}</span>
      {/if}
    </div>

    <!-- Run Jury Button -->
    <button
      class="btn btn-primary btn-sm"
      onclick={triggerJuryPipeline}
      disabled={isRunningJury}
      title="Run full T0->T1->T2 pipeline on all unverified segments"
    >
      {#if isRunningJury}
        <span class="spinner inline-block" style="width:10px;height:10px;"></span> Running Jury…
      {:else}
        ⚡ Run Jury
      {/if}
    </button>
    {#if appSettings && !appSettings.juryCloudOptIn}
      <!-- Consent affordance: the jury's T2 tier can send audio to Gemini, but cloud T2 is opt-in.
           The backend hard-refuses T2 egress when the opt-in is off (the run stays local) — surface
           that state here so it's not silent, mirroring the gated Scribe buttons. -->
      <span
        data-testid="jury-local-only"
        style="font-size: 0.68rem; opacity: 0.75; margin-left: 6px; white-space: nowrap;"
        title="Cloud T2 (Gemini) escalation is OFF in Settings — this run stays fully local (T0/T1); contested segments go to your inbox and no audio leaves your machine."
        >🔒 Local only</span
      >
    {/if}

    <!-- Autonomy Dial -->
    <div class="autonomy-dial" role="group" aria-label="Autonomy level">
      {#each [['observe', '👁 Observe'], ['propose', '💡 Propose'], ['act_confirm', '✅ Act+Confirm'], ['act_auto', '🤖 Act Auto']] as [val, label]}
        <button
          class="dial-btn"
          class:active={autonomyLevel === val}
          onclick={() => setAutonomy(val as typeof autonomyLevel)}
          title={val}>{label}</button
        >
      {/each}
    </div>

    <button class="close-btn" onclick={onClose} aria-label="Close inbox">✕</button>
  </div>

  {#if isLoading}
    <div class="inbox-loading">
      <span class="spinner"></span> Loading escalation queue…
    </div>
  {:else if queue.length === 0}
    <div class="inbox-empty">
      <div class="empty-icon">🎉</div>
      <h3>Inbox zero!</h3>
      <p>No segments need review right now.</p>
      <div style="display: flex; gap: 10px; margin-top: 10px;">
        <button class="btn btn-primary" onclick={triggerJuryPipeline} disabled={isRunningJury}>
          {isRunningJury ? 'Running Jury...' : '⚡ Run Jury Pipeline'}
        </button>
        <button class="btn btn-secondary" onclick={loadQueue}>Refresh</button>
      </div>
    </div>
  {:else}
    <div class="inbox-body">
      <!-- Queue Rail -->
      <nav class="queue-rail" aria-label="Segment queue">
        <div class="rail-header">Queue ({queue.length})</div>
        <ul class="rail-list">
          {#each queue as seg, i}
            {@const band = confidenceBand(seg.agentConfidence)}
            <li class="rail-row">
              <button
                type="button"
                class="rail-item"
                class:active={i === currentIndex}
                class:done={!!seg.humanDecision}
                onclick={() => (currentIndex = i)}
                aria-label="Segment {i + 1}"
                aria-current={i === currentIndex ? 'true' : undefined}
              >
                <span class="rail-icon" style="color:{band.color}">{band.icon}</span>
                <span class="rail-id">{seg.id.slice(0, 8)}…</span>
                {#if seg.humanDecision}
                  <span class="rail-done">✓</span>
                {/if}
              </button>
            </li>
          {/each}
        </ul>
      </nav>

      <!-- Focus Card -->
      {#if current}
        {@const band = confidenceBand(current.agentConfidence)}
        <article class="focus-card" aria-label="Current segment">
          <!-- Segment ID + meta -->
          <div class="card-meta">
            <span class="meta-id"><bdi>{current.id.slice(0, 16)}</bdi></span>
            <span class="meta-dur"><bdi>{Math.round(current.durationMs / 1000)}s</bdi></span>
            {#if current.speakerId}
              <span class="meta-speaker"><bdi>{current.speakerId}</bdi></span>
            {/if}
          </div>

          <!-- Bounded clip playback so the reviewer can HEAR the segment before deciding -->
          <div class="audio-zone" dir="ltr" aria-label="Segment audio player">
            {#if current.audioPath}
              {#key current.id}
                <AudioPlayer
                  audioPath={current.audioPath}
                  startTime={inboxRange.startTime}
                  endTime={inboxRange.endTime}
                  autoplay={false}
                />
              {/key}
            {:else}
              <div class="waveform-stub">🔊 <bdi>no audio</bdi></div>
            {/if}
          </div>

          <!-- Hypotheses section (RTL for Kurdish text) -->
          <section class="hyp-section">
            <h3 class="section-label">Transcription hypotheses</h3>
            <div class="hyp-raw" dir="rtl" lang="ckb">
              <span class="hyp-label-inline">Raw ASR:</span>
              <span class="hyp-text">{current.rawTranscript}</span>
            </div>
            {#if current.normalizedTranscript && current.normalizedTranscript !== current.rawTranscript}
              <div class="hyp-norm" dir="rtl" lang="ckb">
                <span class="hyp-label-inline">Normalized:</span>
                <span class="hyp-text">{current.normalizedTranscript}</span>
              </div>
            {/if}
          </section>

          <!-- Jury verdict (RTL) -->
          {#if current.verdictTranscript}
            <section class="verdict-section">
              <h3 class="section-label">🤖 Jury proposes</h3>
              <div class="verdict-text" dir="rtl" lang="ckb">{current.verdictTranscript}</div>
            </section>
          {/if}

          <!-- Evidence & reasoning -->
          {#if current.rationale || current.evidenceJson}
            <section class="rationale-section">
              <h3 class="section-label">📋 Jury Rationale & Evidence</h3>
              <details class="rationale-details" open>
                <summary>Evidence & reasoning</summary>
                {#if current.rationale}
                  <p class="rationale-text">{current.rationale}</p>
                {/if}
                {#if current.evidenceJson}
                  <pre class="evidence-pre">{safeEvidence(current.evidenceJson)}</pre>
                {/if}
              </details>
            </section>
          {/if}

          <!-- Confidence band -->
          <div class="confidence-strip" style="border-left-color:{band.color}">
            <span class="conf-icon">{band.icon}</span>
            <span class="conf-label">{band.label}</span>
          </div>

          <!-- Edit area (shown when e pressed) -->
          {#if isEditing}
            <div class="edit-area">
              <label class="edit-label" for="edit-textarea"
                >Edit transcript (Ctrl+Enter to save, Esc to cancel):</label
              >
              <textarea
                id="edit-textarea"
                class="edit-textarea"
                dir="rtl"
                lang="ckb"
                bind:value={editText}
                bind:this={editTextarea}
                rows={3}
              ></textarea>
              <div class="edit-actions">
                <button class="btn btn-primary" onclick={commitEdit}>Save edit (Ctrl+↵)</button>
                <button class="btn btn-secondary" onclick={() => (isEditing = false)}
                  >Cancel (Esc)</button
                >
              </div>
            </div>
          {/if}

          <!-- Verb bar (Prodigy-style) -->
          <div class="verb-bar" role="group" aria-label="Review actions">
            <button class="verb-btn accept" onclick={accept} title="Accept (a)" id="inbox-accept">
              <span class="verb-key">A</span> Accept
            </button>
            <button class="verb-btn edit" onclick={startEdit} title="Edit (e)" id="inbox-edit">
              <span class="verb-key">E</span> Edit
            </button>
            <button class="verb-btn reject" onclick={reject} title="Reject (x)" id="inbox-reject">
              <span class="verb-key">X</span> Reject
            </button>
            <button class="verb-btn skip" onclick={skip} title="Skip (space)" id="inbox-skip">
              <span class="verb-key">⎵</span> Skip
            </button>
            <button
              class="verb-btn flag"
              onclick={flag}
              title="Flag for second pass (f)"
              id="inbox-flag"
            >
              <span class="verb-key">F</span> Flag
            </button>
            <button
              class="verb-btn undo"
              onclick={undo}
              title="Undo (⌫)"
              id="inbox-undo"
              disabled={history.length === 0}
            >
              <span class="verb-key">⌫</span> Undo
            </button>
          </div>

          {#if statusMsg}
            <div class="status-bar" role="status" aria-live="polite">{statusMsg}</div>
          {/if}
        </article>
      {/if}
    </div>
  {/if}
</div>

<style>
  /* ── Root ──────────────────────────────────────────────────────────────────── */
  .inbox-root {
    display: flex;
    flex-direction: column;
    height: 100%;
    background: var(--app-bg);
    color: var(--text);
    font-family: var(--font-sans);
    border-radius: 12px;
    overflow: hidden;
  }

  /* ── Header ─────────────────────────────────────────────────────────────────── */
  .inbox-header {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 12px 16px;
    background: var(--surface-1);
    border-bottom: 1px solid var(--border);
    flex-shrink: 0;
  }
  .inbox-title {
    display: flex;
    align-items: center;
    gap: 8px;
    flex: 1;
  }
  .inbox-title h2 {
    margin: 0;
    font-size: 0.95rem;
    font-weight: 600;
    color: var(--accent);
  }
  .inbox-icon {
    font-size: 1.1rem;
  }
  .inbox-badge {
    background: var(--accent);
    color: var(--text-on-accent);
    font-size: 0.7rem;
    font-weight: 700;
    padding: 1px 7px;
    border-radius: 999px;
  }
  .close-btn {
    background: none;
    border: none;
    color: var(--text-muted);
    cursor: pointer;
    font-size: 1rem;
    padding: 4px;
  }
  .close-btn:hover {
    color: var(--text);
  }

  /* ── Autonomy Dial ───────────────────────────────────────────────────────────── */
  .autonomy-dial {
    display: flex;
    gap: 4px;
  }
  .dial-btn {
    background: var(--surface-2);
    border: 1px solid var(--border);
    color: var(--text-muted);
    font-size: 0.65rem;
    padding: 3px 8px;
    border-radius: 6px;
    cursor: pointer;
    transition: all 0.15s;
    white-space: nowrap;
  }
  .dial-btn:hover {
    border-color: var(--accent);
    color: var(--text);
  }
  .dial-btn.active {
    background: var(--accent);
    border-color: var(--accent);
    color: var(--text-on-accent);
  }

  /* ── Loading / Empty ─────────────────────────────────────────────────────────── */
  .inbox-loading,
  .inbox-empty {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    flex: 1;
    gap: 12px;
    color: var(--text-muted);
    font-size: 0.9rem;
  }
  .empty-icon {
    font-size: 3rem;
  }
  .inbox-empty h3 {
    margin: 0;
    color: var(--text);
  }
  .inbox-empty p {
    margin: 0;
    text-align: center;
    max-width: 300px;
  }
  .spinner {
    display: inline-block;
    width: 18px;
    height: 18px;
    border: 2px solid currentColor;
    border-top-color: transparent;
    border-radius: 50%;
    animation: spin 0.7s linear infinite;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }

  /* ── Body ───────────────────────────────────────────────────────────────────── */
  .inbox-body {
    display: flex;
    flex: 1;
    overflow: hidden;
  }

  /* ── Queue Rail ─────────────────────────────────────────────────────────────── */
  .queue-rail {
    width: 140px;
    flex-shrink: 0;
    background: var(--surface-1);
    border-right: 1px solid var(--border);
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }
  .rail-header {
    padding: 8px 10px;
    font-size: 0.65rem;
    font-weight: 600;
    color: var(--text-muted);
    text-transform: uppercase;
    letter-spacing: 0.05em;
    border-bottom: 1px solid var(--border);
  }
  .rail-list {
    flex: 1;
    overflow-y: auto;
    list-style: none;
    margin: 0;
    padding: 4px 0;
  }
  .rail-row {
    margin: 0;
    padding: 0;
  }
  .rail-item {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 6px 10px;
    cursor: pointer;
    transition: background 0.1s;
    font-size: 0.72rem;
    color: var(--text-muted);
    border-radius: 4px;
    margin: 1px 4px;
    user-select: none;
    width: calc(100% - 8px);
    border: 0;
    background: transparent;
    text-align: left;
  }
  .rail-item:hover {
    background: var(--surface-3);
  }
  .rail-item.active {
    background: var(--accent-soft);
    color: var(--accent);
  }
  .rail-item.done {
    opacity: 0.45;
  }
  .rail-icon {
    font-size: 0.8rem;
  }
  .rail-id {
    flex: 1;
    font-family: var(--font-mono);
    font-size: 0.65rem;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .rail-done {
    color: var(--success);
    font-size: 0.7rem;
  }

  /* ── Focus Card ─────────────────────────────────────────────────────────────── */
  .focus-card {
    flex: 1;
    overflow-y: auto;
    padding: 20px 24px;
    display: flex;
    flex-direction: column;
    gap: 16px;
  }

  /* ── Meta ────────────────────────────────────────────────────────────────────── */
  .card-meta {
    display: flex;
    gap: 10px;
    align-items: center;
    flex-wrap: wrap;
  }
  .meta-id,
  .meta-dur,
  .meta-speaker {
    background: var(--surface-2);
    border: 1px solid var(--border);
    padding: 2px 8px;
    border-radius: 4px;
    font-size: 0.7rem;
    font-family: var(--font-mono);
    color: var(--text-muted);
  }

  /* ── Waveform zone ───────────────────────────────────────────────────────────── */
  .audio-zone {
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 12px 16px;
    font-size: 0.8rem;
    color: var(--text-muted);
  }
  .waveform-stub {
    font-size: 0.8rem;
  }

  /* ── Sections ────────────────────────────────────────────────────────────────── */
  .hyp-section,
  .verdict-section,
  .rationale-section {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .section-label {
    margin: 0;
    font-size: 0.72rem;
    font-weight: 600;
    color: var(--text-muted);
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }

  .hyp-raw,
  .hyp-norm {
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 10px 14px;
    font-family: var(--font-kurdish);
    font-size: 1.05rem;
    line-height: 1.9;
    color: var(--text);
    text-align: start;
  }
  .hyp-label-inline {
    font-size: 0.65rem;
    color: var(--text-muted);
    font-family: var(--font-mono);
    display: inline-block;
    margin-inline-end: 8px;
  }
  .hyp-text {
    direction: rtl;
    unicode-bidi: embed;
  }

  .verdict-text {
    background: var(--accent-soft);
    border: 1px solid color-mix(in srgb, var(--accent) 35%, transparent);
    border-radius: 8px;
    padding: 12px 16px;
    font-family: var(--font-kurdish);
    font-size: 1.1rem;
    line-height: 1.9;
    color: var(--text);
    text-align: start;
  }

  /* ── Rationale ────────────────────────────────────────────────────────────────── */
  .rationale-details {
    background: var(--surface-inset);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 8px 12px;
    font-size: 0.8rem;
  }
  .rationale-details summary {
    cursor: pointer;
    color: var(--text-muted);
  }
  .rationale-text {
    color: var(--text-muted);
    margin: 6px 0 0;
    line-height: 1.6;
  }
  .evidence-pre {
    background: var(--surface-inset);
    border-radius: 4px;
    padding: 8px;
    font-size: 0.7rem;
    color: var(--text-muted);
    overflow-x: auto;
    white-space: pre-wrap;
    word-break: break-all;
  }

  /* ── Confidence strip ─────────────────────────────────────────────────────────── */
  .confidence-strip {
    display: flex;
    align-items: center;
    gap: 8px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-left-width: 3px;
    border-radius: 6px;
    padding: 8px 14px;
    font-size: 0.8rem;
    color: var(--text);
  }
  .conf-icon {
    font-size: 1rem;
  }
  .conf-label {
    flex: 1;
  }

  /* ── Edit area ───────────────────────────────────────────────────────────────── */
  .edit-area {
    display: flex;
    flex-direction: column;
    gap: 8px;
    background: var(--surface-2);
    border: 1px solid var(--accent);
    border-radius: 8px;
    padding: 12px 16px;
  }
  .edit-label {
    font-size: 0.75rem;
    color: var(--text-muted);
  }
  .edit-textarea {
    background: var(--surface-inset);
    border: 1px solid var(--border);
    border-radius: 6px;
    color: var(--text);
    padding: 8px 12px;
    resize: vertical;
    font-family: var(--font-kurdish);
    font-size: 1rem;
    line-height: 1.9;
    direction: rtl;
    text-align: start;
    width: 100%;
    box-sizing: border-box;
  }
  .edit-actions {
    display: flex;
    gap: 8px;
  }

  /* ── Verb bar ────────────────────────────────────────────────────────────────── */
  .verb-bar {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
    background: var(--surface-1);
    border-top: 1px solid var(--border);
    padding: 12px 0 4px;
    position: sticky;
    bottom: 0;
  }
  .verb-btn {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 8px 16px;
    border-radius: 8px;
    border: 1px solid transparent;
    font-size: 0.8rem;
    font-weight: 600;
    cursor: pointer;
    transition: all 0.15s;
    letter-spacing: 0.02em;
  }
  .verb-btn:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }
  .verb-key {
    background: color-mix(in srgb, currentColor 16%, transparent);
    border-radius: 4px;
    padding: 1px 5px;
    font-family: var(--font-mono);
    font-size: 0.7rem;
  }
  /* Semantic verb buttons use the theme-aware status palettes (channels defined
     in app.css :root/.light) so each tint + label clears ≥3:1 in both themes. */
  .verb-btn.accept {
    background: rgb(var(--emerald-500-rgb) / 0.12);
    border-color: rgb(var(--emerald-500-rgb) / 0.35);
    color: rgb(var(--emerald-400-rgb));
  }
  .verb-btn.accept:hover:not(:disabled) {
    background: rgb(var(--emerald-500-rgb) / 0.2);
  }
  .verb-btn.edit {
    background: rgb(var(--blue-500-rgb) / 0.12);
    border-color: rgb(var(--blue-500-rgb) / 0.35);
    color: rgb(var(--blue-400-rgb));
  }
  .verb-btn.edit:hover:not(:disabled) {
    background: rgb(var(--blue-500-rgb) / 0.2);
  }
  .verb-btn.reject {
    background: rgb(var(--red-500-rgb) / 0.12);
    border-color: rgb(var(--red-500-rgb) / 0.35);
    color: rgb(var(--red-400-rgb));
  }
  .verb-btn.reject:hover:not(:disabled) {
    background: rgb(var(--red-500-rgb) / 0.2);
  }
  .verb-btn.skip {
    background: var(--surface-2);
    border-color: var(--border);
    color: var(--text-muted);
  }
  .verb-btn.skip:hover:not(:disabled) {
    background: var(--surface-3);
    color: var(--text);
  }
  .verb-btn.flag {
    background: rgb(var(--amber-500-rgb) / 0.12);
    border-color: rgb(var(--amber-500-rgb) / 0.35);
    color: rgb(var(--amber-400-rgb));
  }
  .verb-btn.flag:hover:not(:disabled) {
    background: rgb(var(--amber-500-rgb) / 0.2);
  }
  .verb-btn.undo {
    background: var(--surface-2);
    border-color: var(--border);
    color: var(--text-subtle);
  }
  .verb-btn.undo:hover:not(:disabled) {
    background: var(--surface-3);
    color: var(--text);
  }

  /* ── Status bar ─────────────────────────────────────────────────────────────── */
  .status-bar {
    text-align: center;
    font-size: 0.78rem;
    color: var(--accent);
    padding: 4px 0;
    animation: fadeIn 0.2s ease;
  }
  @keyframes fadeIn {
    from {
      opacity: 0;
      transform: translateY(4px);
    }
    to {
      opacity: 1;
    }
  }
</style>
