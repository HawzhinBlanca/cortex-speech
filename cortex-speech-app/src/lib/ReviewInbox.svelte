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
  // Guard against double-submission from rapid key presses.
  let isSubmitting = false;

  $: current = queue[currentIndex] ?? null;
  $: pendingCount = queue.filter(s => !s.humanDecision).length;

  // ── Confidence bands ─────────────────────────────────────────────────────────
  function confidenceBand(conf: number | null | undefined): { label: string; icon: string; color: string } {
    if (conf == null) return { label: 'Unknown confidence', icon: '❓', color: '#6b7280' };
    if (conf >= 0.90) return { label: `AI is very confident (${Math.round(conf * 100)}%) — quick glance 👀`, icon: '✅', color: '#22c55e' };
    if (conf >= 0.75) return { label: `AI is fairly sure (${Math.round(conf * 100)}%) — quick listen 👂`, icon: '🟡', color: '#eab308' };
    if (conf >= 0.55) return { label: `AI is unsure (${Math.round(conf * 100)}%) — listen carefully ⚠`, icon: '⚠️', color: '#f97316' };
    return { label: `AI has low confidence (${Math.round(conf * 100)}%) — careful review needed 🔴`, icon: '🔴', color: '#ef4444' };
  }

  // ── Queue loading ─────────────────────────────────────────────────────────────
  async function loadQueue() {
    isLoading = true;
    try {
      queue = await api.getEscalationQueue(200);
      currentIndex = 0;
    } catch (e) {
      statusMsg = `Failed to load queue: ${e}`;
    } finally {
      isLoading = false;
    }
  }

  let isRunningJury = false;

  async function triggerJuryPipeline() {
    if (isRunningJury) return;
    isRunningJury = true;
    statusMsg = '⏳ Running Jury Pipeline...';
    try {
      const allSegs = await api.getSegments();
      const targetIds = allSegs.filter(s => !s.verified).map(s => s.id);
      if (targetIds.length === 0) {
        statusMsg = 'ℹ️ No unverified segments to run jury on.';
        isRunningJury = false;
        return;
      }
      const report = await api.runJuryPipeline(targetIds);
      statusMsg = `⚡ Jury finished! T0 accepted: ${report.t0AutoAccepted}, T1 committed: ${report.t1Committed}, T2 committed: ${report.t2Committed}, Escalated: ${report.humanInbox}`;
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
    isSubmitting = true;
    try {
      history.push({ id: current.id, decision: 'accept', prev: { ...current } });
      await api.recordHumanDecision(current.id, 'accept', null);
      queue[currentIndex] = { ...current, humanDecision: 'accept' };
      statusMsg = '✅ Accepted';
      advance();
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
    isSubmitting = true;
    try {
      history.push({ id: current.id, decision: 'edit', prev: { ...current } });
      await api.recordHumanDecision(current.id, 'edit', editText.trim());
      queue[currentIndex] = { ...current, humanDecision: 'edit', verdictTranscript: editText.trim() };
      isEditing = false;
      statusMsg = '✏️ Edited';
      advance();
    } finally {
      isSubmitting = false;
    }
  }

  async function reject() {
    if (!current || isSubmitting) return;
    isSubmitting = true;
    try {
      history.push({ id: current.id, decision: 'reject', prev: { ...current } });
      await api.recordHumanDecision(current.id, 'reject', null);
      queue[currentIndex] = { ...current, humanDecision: 'reject' };
      statusMsg = '❌ Rejected';
      advance();
    } finally {
      isSubmitting = false;
    }
  }

  function skip() {
    if (!current) return;
    statusMsg = '⏭ Skipped';
    advance();
  }

  async function flag() {
    if (!current || isSubmitting) return;
    isSubmitting = true;
    try {
      await api.writeSegmentVerdict(current.id, 'escalated', null, 'Flagged for second-pass adjudication', null, null, true);
      queue[currentIndex] = { ...current, escalated: true };
      statusMsg = '🚩 Flagged for second pass';
      advance();
    } finally {
      isSubmitting = false;
    }
  }

  async function undo() {
    const last = history.pop();
    if (!last) return;
    // P3-3: Clear the human decision entirely (set to NULL) instead of
    // overwriting it with a fake 'accept' — that was corrupting agent_examples.
    await api.clearHumanDecision(last.id);
    const idx = queue.findIndex(s => s.id === last.id);
    if (idx >= 0) {
      queue[idx] = { ...last.prev };
      // Navigate directly to the undone segment, not blindly to currentIndex-1.
      // If the user accepted seg#5 then scrolled to seg#10, undo should show seg#5.
      currentIndex = idx;
    }
    statusMsg = '↩ Undone';
  }


  function advance() {
    if (currentIndex < queue.length - 1) {
      currentIndex++;
    }
  }

  // ── Keyboard handler ─────────────────────────────────────────────────────────
  function handleKey(e: KeyboardEvent) {
    if (isEditing) {
      if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) { e.preventDefault(); commitEdit(); }
      if (e.key === 'Escape') { e.preventDefault(); isEditing = false; }
      return;
    }
    switch (e.key) {
      case 'a': e.preventDefault(); accept(); break;
      case 'e': e.preventDefault(); startEdit(); break;
      case 'x': e.preventDefault(); reject(); break;
      case ' ': e.preventDefault(); skip(); break;
      case 'f': e.preventDefault(); flag(); break;
      case 'Backspace': e.preventDefault(); undo(); break;
      case 'Escape': e.preventDefault(); onClose(); break;
      default:
        if (e.key >= '1' && e.key <= '9') {
          const idx = parseInt(e.key) - 1;
          if (idx < queue.length) { currentIndex = idx; }
        }
    }
  }

  onMount(() => {
    loadQueue();
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
      class="run-jury-btn"
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

    <!-- Autonomy Dial -->
    <div class="autonomy-dial" role="group" aria-label="Autonomy level">
      {#each [['observe','👁 Observe'],['propose','💡 Propose'],['act_confirm','✅ Act+Confirm'],['act_auto','🤖 Act Auto']] as [val, label]}
        <button
          class="dial-btn"
          class:active={autonomyLevel === val}
          onclick={() => (autonomyLevel = val as typeof autonomyLevel)}
          title={val}
        >{label}</button>
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
        <button class="btn-primary" onclick={triggerJuryPipeline} disabled={isRunningJury}>
          {isRunningJury ? 'Running Jury...' : '⚡ Run Jury Pipeline'}
        </button>
        <button class="btn-secondary" onclick={loadQueue}>Refresh</button>
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

          <!-- Waveform placeholder (LTR always) -->
          <div class="waveform-zone" dir="ltr" aria-label="Audio waveform">
            <div class="waveform-stub">
              🔊 <bdi>{current.audioPath?.split(/[\\/]/).pop() ?? 'audio'}</bdi>
            </div>
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
                  <pre class="evidence-pre">{JSON.stringify(JSON.parse(current.evidenceJson ?? '[]'), null, 2)}</pre>
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
              <label class="edit-label" for="edit-textarea">Edit transcript (Ctrl+Enter to save, Esc to cancel):</label>
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
                <button class="btn-primary" onclick={commitEdit}>Save edit (Ctrl+↵)</button>
                <button class="btn-secondary" onclick={() => (isEditing = false)}>Cancel (Esc)</button>
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
            <button class="verb-btn flag" onclick={flag} title="Flag for second pass (f)" id="inbox-flag">
              <span class="verb-key">F</span> Flag
            </button>
            <button class="verb-btn undo" onclick={undo} title="Undo (⌫)" id="inbox-undo" disabled={history.length === 0}>
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
    background: #0e1117;
    color: #e2e8f0;
    font-family: 'Inter', system-ui, sans-serif;
    border-radius: 12px;
    overflow: hidden;
  }

  /* ── Header ─────────────────────────────────────────────────────────────────── */
  .inbox-header {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 12px 16px;
    background: #161b27;
    border-bottom: 1px solid #2d3748;
    flex-shrink: 0;
  }
  .inbox-title {
    display: flex;
    align-items: center;
    gap: 8px;
    flex: 1;
  }
  .inbox-title h2 { margin: 0; font-size: 0.95rem; font-weight: 600; color: #a78bfa; }
  .inbox-icon { font-size: 1.1rem; }
  .inbox-badge {
    background: #7c3aed;
    color: #fff;
    font-size: 0.7rem;
    font-weight: 700;
    padding: 1px 7px;
    border-radius: 999px;
  }
  .close-btn {
    background: none; border: none; color: #6b7280;
    cursor: pointer; font-size: 1rem; padding: 4px;
  }
  .close-btn:hover { color: #e2e8f0; }

  .run-jury-btn {
    background: linear-gradient(135deg, #7c3aed, #4f46e5);
    color: #fff; border: 1px solid #6d28d9;
    font-size: 0.7rem; font-weight: 600; padding: 4px 10px;
    border-radius: 6px; cursor: pointer; transition: all 0.2s;
    display: inline-flex; align-items: center; gap: 6px;
    white-space: nowrap;
    box-shadow: 0 2px 4px rgba(0, 0, 0, 0.2);
  }
  .run-jury-btn:hover:not(:disabled) {
    background: linear-gradient(135deg, #8b5cf6, #6366f1);
    transform: translateY(-1px);
    box-shadow: 0 0 10px rgba(124, 58, 237, 0.4);
  }
  .run-jury-btn:disabled {
    opacity: 0.6; cursor: not-allowed;
  }


  /* ── Autonomy Dial ───────────────────────────────────────────────────────────── */
  .autonomy-dial { display: flex; gap: 4px; }
  .dial-btn {
    background: #1e2536; border: 1px solid #2d3748;
    color: #94a3b8; font-size: 0.65rem; padding: 3px 8px;
    border-radius: 6px; cursor: pointer; transition: all 0.15s;
    white-space: nowrap;
  }
  .dial-btn:hover { border-color: #7c3aed; color: #e2e8f0; }
  .dial-btn.active { background: #7c3aed; border-color: #7c3aed; color: #fff; }

  /* ── Loading / Empty ─────────────────────────────────────────────────────────── */
  .inbox-loading, .inbox-empty {
    display: flex; flex-direction: column; align-items: center; justify-content: center;
    flex: 1; gap: 12px; color: #94a3b8; font-size: 0.9rem;
  }
  .empty-icon { font-size: 3rem; }
  .inbox-empty h3 { margin: 0; color: #e2e8f0; }
  .inbox-empty p { margin: 0; text-align: center; max-width: 300px; }
  .spinner {
    display: inline-block; width: 18px; height: 18px;
    border: 2px solid #7c3aed; border-top-color: transparent;
    border-radius: 50%; animation: spin 0.7s linear infinite;
  }
  @keyframes spin { to { transform: rotate(360deg); } }

  /* ── Body ───────────────────────────────────────────────────────────────────── */
  .inbox-body {
    display: flex; flex: 1; overflow: hidden;
  }

  /* ── Queue Rail ─────────────────────────────────────────────────────────────── */
  .queue-rail {
    width: 140px; flex-shrink: 0; background: #0d1117;
    border-right: 1px solid #2d3748; display: flex; flex-direction: column;
    overflow: hidden;
  }
  .rail-header {
    padding: 8px 10px; font-size: 0.65rem; font-weight: 600;
    color: #6b7280; text-transform: uppercase; letter-spacing: 0.05em;
    border-bottom: 1px solid #1e2536;
  }
  .rail-list {
    flex: 1; overflow-y: auto; list-style: none; margin: 0; padding: 4px 0;
  }
  .rail-row { margin: 0; padding: 0; }
  .rail-item {
    display: flex; align-items: center; gap: 6px;
    padding: 6px 10px; cursor: pointer; transition: background 0.1s;
    font-size: 0.72rem; color: #94a3b8; border-radius: 4px; margin: 1px 4px;
    user-select: none;
    width: calc(100% - 8px); border: 0; background: transparent; text-align: left;
  }
  .rail-item:hover { background: #1e2536; }
  .rail-item.active { background: #2d1f5e; color: #c4b5fd; }
  .rail-item.done { opacity: 0.45; }
  .rail-icon { font-size: 0.8rem; }
  .rail-id { flex: 1; font-family: monospace; font-size: 0.65rem; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .rail-done { color: #22c55e; font-size: 0.7rem; }

  /* ── Focus Card ─────────────────────────────────────────────────────────────── */
  .focus-card {
    flex: 1; overflow-y: auto; padding: 20px 24px;
    display: flex; flex-direction: column; gap: 16px;
  }

  /* ── Meta ────────────────────────────────────────────────────────────────────── */
  .card-meta {
    display: flex; gap: 10px; align-items: center; flex-wrap: wrap;
  }
  .meta-id, .meta-dur, .meta-speaker {
    background: #1e2536; border: 1px solid #2d3748;
    padding: 2px 8px; border-radius: 4px;
    font-size: 0.7rem; font-family: monospace; color: #94a3b8;
  }

  /* ── Waveform zone ───────────────────────────────────────────────────────────── */
  .waveform-zone {
    background: #161b27; border: 1px solid #2d3748;
    border-radius: 8px; padding: 12px 16px;
    font-size: 0.8rem; color: #6b7280;
  }
  .waveform-stub { font-size: 0.8rem; }

  /* ── Sections ────────────────────────────────────────────────────────────────── */
  .hyp-section, .verdict-section, .rationale-section { display: flex; flex-direction: column; gap: 8px; }
  .section-label {
    margin: 0; font-size: 0.72rem; font-weight: 600;
    color: #6b7280; text-transform: uppercase; letter-spacing: 0.04em;
  }

  .hyp-raw, .hyp-norm {
    background: #161b27; border: 1px solid #2d3748; border-radius: 8px;
    padding: 10px 14px; font-family: 'Vazirmatn', 'Noto Naskh Arabic', system-ui, sans-serif;
    font-size: 1.05rem; line-height: 1.9; color: #cbd5e1;
    text-align: start;
  }
  .hyp-label-inline {
    font-size: 0.65rem; color: #6b7280; font-family: monospace;
    display: inline-block; margin-inline-end: 8px;
  }
  .hyp-text { direction: rtl; unicode-bidi: embed; }

  .verdict-text {
    background: #1a1f2e; border: 1px solid #4c1d95;
    border-radius: 8px; padding: 12px 16px;
    font-family: 'Vazirmatn', 'Noto Naskh Arabic', system-ui, sans-serif;
    font-size: 1.1rem; line-height: 1.9; color: #c4b5fd;
    text-align: start;
  }

  /* ── Rationale ────────────────────────────────────────────────────────────────── */
  .rationale-details {
    background: #0d1117; border: 1px solid #2d3748;
    border-radius: 6px; padding: 8px 12px;
    font-size: 0.8rem;
  }
  .rationale-details summary { cursor: pointer; color: #94a3b8; }
  .rationale-text { color: #cbd5e1; margin: 6px 0 0; line-height: 1.6; }
  .evidence-pre {
    background: #161b27; border-radius: 4px; padding: 8px;
    font-size: 0.7rem; color: #94a3b8; overflow-x: auto;
    white-space: pre-wrap; word-break: break-all;
  }

  /* ── Confidence strip ─────────────────────────────────────────────────────────── */
  .confidence-strip {
    display: flex; align-items: center; gap: 8px;
    background: #161b27; border: 1px solid #2d3748;
    border-left-width: 3px; border-radius: 6px;
    padding: 8px 14px; font-size: 0.8rem; color: #94a3b8;
  }
  .conf-icon { font-size: 1rem; }
  .conf-label { flex: 1; }

  /* ── Edit area ───────────────────────────────────────────────────────────────── */
  .edit-area {
    display: flex; flex-direction: column; gap: 8px;
    background: #161b27; border: 1px solid #7c3aed;
    border-radius: 8px; padding: 12px 16px;
  }
  .edit-label { font-size: 0.75rem; color: #94a3b8; }
  .edit-textarea {
    background: #0d1117; border: 1px solid #2d3748; border-radius: 6px;
    color: #e2e8f0; padding: 8px 12px; resize: vertical;
    font-family: 'Vazirmatn', 'Noto Naskh Arabic', system-ui, sans-serif;
    font-size: 1rem; line-height: 1.9;
    direction: rtl; text-align: start;
    width: 100%; box-sizing: border-box;
  }
  .edit-actions { display: flex; gap: 8px; }

  /* ── Verb bar ────────────────────────────────────────────────────────────────── */
  .verb-bar {
    display: flex; gap: 8px; flex-wrap: wrap;
    background: #0d1117; border-top: 1px solid #2d3748;
    padding: 12px 0 4px; position: sticky; bottom: 0;
  }
  .verb-btn {
    display: flex; align-items: center; gap: 6px;
    padding: 8px 16px; border-radius: 8px; border: 1px solid;
    font-size: 0.8rem; font-weight: 600; cursor: pointer;
    transition: all 0.15s; letter-spacing: 0.02em;
  }
  .verb-btn:disabled { opacity: 0.4; cursor: not-allowed; }
  .verb-key {
    background: rgba(255,255,255,0.1); border-radius: 4px;
    padding: 1px 5px; font-family: monospace; font-size: 0.7rem;
  }
  .verb-btn.accept { background: #14532d; border-color: #16a34a; color: #86efac; }
  .verb-btn.accept:hover:not(:disabled) { background: #166534; }
  .verb-btn.edit { background: #1e3a5f; border-color: #2563eb; color: #93c5fd; }
  .verb-btn.edit:hover:not(:disabled) { background: #1e40af; }
  .verb-btn.reject { background: #4c1d1d; border-color: #dc2626; color: #fca5a5; }
  .verb-btn.reject:hover:not(:disabled) { background: #7f1d1d; }
  .verb-btn.skip { background: #1e2536; border-color: #475569; color: #94a3b8; }
  .verb-btn.skip:hover:not(:disabled) { background: #2d3748; }
  .verb-btn.flag { background: #451a03; border-color: #ea580c; color: #fdba74; }
  .verb-btn.flag:hover:not(:disabled) { background: #7c2d12; }
  .verb-btn.undo { background: #1e1e1e; border-color: #374151; color: #6b7280; }
  .verb-btn.undo:hover:not(:disabled) { background: #2d3748; color: #e2e8f0; }

  /* ── Status bar ─────────────────────────────────────────────────────────────── */
  .status-bar {
    text-align: center; font-size: 0.78rem; color: #a78bfa;
    padding: 4px 0; animation: fadeIn 0.2s ease;
  }
  @keyframes fadeIn { from { opacity: 0; transform: translateY(4px); } to { opacity: 1; } }

  /* ── Shared button styles ─────────────────────────────────────────────────────── */
  .btn-primary {
    background: #7c3aed; color: #fff; border: none;
    padding: 8px 18px; border-radius: 8px; font-size: 0.82rem;
    font-weight: 600; cursor: pointer; transition: background 0.15s;
  }
  .btn-primary:hover { background: #6d28d9; }
  .btn-secondary {
    background: #1e2536; color: #94a3b8; border: 1px solid #2d3748;
    padding: 8px 18px; border-radius: 8px; font-size: 0.82rem;
    cursor: pointer; transition: background 0.15s;
  }
  .btn-secondary:hover { background: #2d3748; }
</style>
