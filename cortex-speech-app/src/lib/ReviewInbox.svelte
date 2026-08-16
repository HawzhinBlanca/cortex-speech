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
  import { physicalKey } from './keyboard';
  import { t } from './i18n';
  import { parseSourceMeta, chunkPlaybackRange } from './alignment';
  import type { SpeechSegment } from './types';
  import type { AppSettings } from './stores/settingsStore';
  import AudioPlayer from './AudioPlayer.svelte';

  // ── Props ───────────────────────────────────────────────────────────────────
  export let onClose: () => void = () => {};

  // ── State ───────────────────────────────────────────────────────────────────
  let queue: SpeechSegment[] = [];
  let currentIndex = 0;
  let isLoading = false;
  // A transport/database failure is not an empty queue. Keep the failure as first-class state so
  // the reviewer is never shown the celebratory "Inbox zero" claim when nothing was actually read.
  let loadError: string | null = null;
  let isEditing = false;
  let editText = '';
  let editTextarea: HTMLTextAreaElement | null = null;
  let statusMsg = '';
  let history: { id: string; decision: string; prev: SpeechSegment }[] = [];
  // Round-23 #12: the autonomy dial reflects and WRITES the real backend `jury_autonomy_level` setting
  // (read by the T0 gate's apply_autonomy), not a dead local variable.
  let autonomyLevel: 'observe' | 'propose' | 'act_confirm' | 'act_auto' = 'propose';
  // Persisted app settings (mirrors Settings). Seeded on mount, written on a dial change so it can be
  // persisted via update_settings, and read to surface the cloud-T2 (jury) consent state in the header.
  let settings: AppSettings | null = null;
  // Keyboard play/pause state for the current clip (Space); reset on queue navigation so a new
  // clip never inherits the previous clip's playing flag.
  let inboxPlaying = false;
  // True-10 audit MAJOR: edit state must also reset on rail navigation — isEditing/editText used to
  // survive a rail click, so "E on segment A → click B → Save" recorded A's transcript as B's human
  // 'edit' decision (a permanent wrong gold label). editingForId is the second lock: commitEdit
  // refuses to persist when the segment changed since startEdit.
  let editingForId: string | null = null;
  $: if (currentIndex >= 0) {
    inboxPlaying = false;
    isEditing = false;
    editText = '';
    editingForId = null;
  }
  // True-10 audit: keep the active rail row visible — past ~15 items the aria-current row scrolled
  // below the fold of a 200-item queue with no way to see where you are.
  $: if (currentIndex >= 0) void scrollRailToCurrent();
  async function scrollRailToCurrent() {
    await tick();
    document
      .querySelector('.rail-list [aria-current="true"]')
      ?.scrollIntoView({ block: 'nearest' });
  }
  // Guard against double-submission from rapid key presses.
  let isSubmitting = false;

  async function setAutonomy(val: 'observe' | 'propose' | 'act_confirm' | 'act_auto') {
    const previous = autonomyLevel;
    autonomyLevel = val; // optimistic
    if (!settings) {
      // Settings not loaded yet — keep the optimistic UI but don't claim it persisted.
      return;
    }
    try {
      const next = { ...settings, juryAutonomyLevel: val };
      await api.updateSettings(next);
      settings = next;
      statusMsg = $t('inbox.status.autonomySet', {
        level: $t(`inbox.autonomy.${autonomyKey(val)}`),
      });
    } catch (e) {
      autonomyLevel = previous; // revert so the dial never lies about the persisted state
      statusMsg = $t('inbox.status.autonomyFailed', { err: String(e) });
    }
  }

  // Map a backend level to its i18n key suffix (observe/propose/actConfirm/actAuto).
  function autonomyKey(val: string): string {
    return val === 'act_confirm' ? 'actConfirm' : val === 'act_auto' ? 'actAuto' : val;
  }

  $: current = queue[currentIndex] ?? null;
  $: pendingCount = queue.filter((s) => !s.humanDecision).length;
  // Bound the player to this segment's window so Play hears only this clip, not the whole file.
  $: inboxRange = current
    ? chunkPlaybackRange(parseSourceMeta(current.alignmentJson))
    : { startTime: 0, endTime: 0 };

  // ── Confidence bands ─────────────────────────────────────────────────────────
  type Translate = (key: string, params?: Record<string, string>) => string;
  // `tr` ($t) is passed in from the template so the band labels stay reactive to a locale change.
  /// Poor audio, by the same thresholds `has_hard_distrust_veto` uses in the jury (snr < 5 dB or
  /// clipping > 0.1). Kept identical on purpose: two definitions of "bad audio" that drift apart would
  /// show a green chip on a clip the gate refused to trust.
  function hasPoorAudio(seg: { snrDb?: number | null; clippingRatio?: number | null }): boolean {
    return (
      (seg.snrDb != null && seg.snrDb < 5) || (seg.clippingRatio != null && seg.clippingRatio > 0.1)
    );
  }

  function confidenceBand(
    conf: number | null | undefined,
    tr: Translate,
    poorAudio = false,
  ): { label: string; icon: string; color: string } {
    const pct = (c: number) => ({ pct: String(Math.round(c * 100)) });
    if (conf == null)
      return { label: tr('inbox.band.unknown'), icon: '❓', color: 'var(--text-subtle)' };
    // External review 2026-08-06 #2: `agreementScore` is model AGREEMENT, and agreement is not
    // trustworthiness. Every recognizer can confidently agree on the same garbage when the audio is
    // bad — which is exactly why the jury vetoes those clips. Rendering that as a green "97%" told the
    // reviewer the opposite of what the gate concluded, so acoustic quality is stated instead of
    // being averaged into one number that means neither thing.
    if (poorAudio) {
      return { label: tr('inbox.band.poorAudio', pct(conf)), icon: '🔊', color: 'var(--warning)' };
    }
    if (conf >= 0.9)
      return {
        label: tr('inbox.band.veryConfident', pct(conf)),
        icon: '✅',
        color: 'var(--success)',
      };
    if (conf >= 0.75)
      return { label: tr('inbox.band.fairlySure', pct(conf)), icon: '🟡', color: 'var(--warning)' };
    if (conf >= 0.55)
      return {
        label: tr('inbox.band.unsure', pct(conf)),
        icon: '⚠️',
        color: 'rgb(var(--orange-400-rgb))',
      };
    return { label: tr('inbox.band.low', pct(conf)), icon: '🔴', color: 'var(--danger)' };
  }

  // ── Queue loading ─────────────────────────────────────────────────────────────
  async function loadQueue() {
    isLoading = true;
    loadError = null;
    try {
      queue = await api.getEscalationQueue(200);
      currentIndex = 0;
      // Drop the undo stack: it references the PREVIOUS queue's segments. A stale undo after a
      // reload would fire a backend clear against a segment no longer in view.
      history = [];
    } catch (e) {
      loadError = $t('inbox.status.loadFailed', { err: String(e) });
      statusMsg = loadError;
    } finally {
      isLoading = false;
    }
  }

  let isRunningJury = false;

  async function triggerJuryPipeline() {
    if (isRunningJury) return;
    isRunningJury = true;
    statusMsg = $t('inbox.status.running');
    try {
      const targetIds = await api.getSegmentIdsForView({ verified: false });
      if (targetIds.length === 0) {
        statusMsg = $t('inbox.status.noUnverified');
        isRunningJury = false;
        return;
      }
      const report = await api.runJuryPipeline(targetIds);
      if (!report) throw new Error('Jury pipeline returned no result');
      statusMsg = $t('inbox.status.juryFinished', {
        t0: String(report.t0AutoAccepted ?? 0),
        t1: String(report.t1Committed ?? 0),
        t2: String(report.t2Committed ?? 0),
        esc: String(report.humanInbox ?? 0),
      });
      await loadQueue();
    } catch (e) {
      statusMsg = $t('inbox.status.juryFailed', { err: String(e) });
    } finally {
      isRunningJury = false;
    }
  }

  // ── Actions ──────────────────────────────────────────────────────────────────
  async function accept() {
    // Already-decided guard: advance() does NOT move past the LAST queue item, so without this a
    // second keypress on the final clip would record a DUPLICATE human decision (a biometric label).
    if (!current || isSubmitting || current.humanDecision) return;
    // Snapshot the target before the await — currentIndex/current can change mid-flight if the user
    // clicks another rail item (the rail is not disabled during submit), which would otherwise stamp
    // this decision onto the wrong segment's queue slot.
    const cur = current;
    const idx = currentIndex;
    isSubmitting = true;
    try {
      // Reassignment (not .push) — this component is legacy-mode, so only assignment invalidates
      // `disabled={history.length === 0}`; mutation left the Undo button permanently disabled.
      history = [...history, { id: cur.id, decision: 'accept', prev: { ...cur } }];
      await api.recordHumanDecision(cur.id, 'accept', null);
      queue[idx] = { ...cur, humanDecision: 'accept' };
      statusMsg = $t('inbox.status.accepted');
      advance();
    } catch (e) {
      // The decision did not persist: drop the phantom undo entry pushed above and
      // tell the reviewer, rather than silently swallowing it (unhandled rejection).
      history = history.slice(0, -1);
      statusMsg = $t('inbox.status.acceptFailed', { err: String(e) });
    } finally {
      isSubmitting = false;
    }
  }

  async function startEdit() {
    if (!current) return;
    editText = current.verdictTranscript ?? current.rawTranscript ?? '';
    isEditing = true;
    editingForId = current.id;
    await tick();
    editTextarea?.focus();
    editTextarea?.select();
  }

  async function commitEdit() {
    if (!current || !editText.trim() || isSubmitting || current.humanDecision) return;
    // Never write text opened for one segment onto another: if the queue navigated since startEdit
    // (any path the reactive reset above might not cover), drop the stale edit instead of persisting
    // a wrong gold label.
    if (editingForId !== current.id) {
      isEditing = false;
      editText = '';
      editingForId = null;
      return;
    }
    const cur = current;
    const idx = currentIndex;
    const text = editText.trim();
    isSubmitting = true;
    try {
      history = [...history, { id: cur.id, decision: 'edit', prev: { ...cur } }];
      await api.recordHumanDecision(cur.id, 'edit', text);
      queue[idx] = {
        ...cur,
        humanDecision: 'edit',
        verdictTranscript: text,
      };
      isEditing = false;
      statusMsg = $t('inbox.status.edited');
      advance();
    } catch (e) {
      history = history.slice(0, -1);
      statusMsg = $t('inbox.status.editFailed', { err: String(e) });
    } finally {
      isSubmitting = false;
    }
  }

  async function reject() {
    if (!current || isSubmitting || current.humanDecision) return;
    const cur = current;
    const idx = currentIndex;
    isSubmitting = true;
    try {
      history = [...history, { id: cur.id, decision: 'reject', prev: { ...cur } }];
      await api.recordHumanDecision(cur.id, 'reject', null);
      queue[idx] = { ...cur, humanDecision: 'reject' };
      statusMsg = $t('inbox.status.rejected');
      advance();
    } catch (e) {
      history = history.slice(0, -1);
      statusMsg = $t('inbox.status.rejectFailed', { err: String(e) });
    } finally {
      isSubmitting = false;
    }
  }

  function skip() {
    if (!current) return;
    statusMsg = $t('inbox.status.skipped');
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
    if (!current || isSubmitting || current.humanDecision) return;
    const cur = current;
    const idx = currentIndex;
    isSubmitting = true;
    try {
      // Record an undo entry BEFORE the await (same as accept/reject) so an accidental `f` — a common
      // fat-finger next to `e`/`x` — is recoverable via Backspace/Undo instead of permanently escalating
      // the segment. The 'flag' tag routes undo() to clear the escalation rather than a human decision.
      history = [...history, { id: cur.id, decision: 'flag', prev: { ...cur } }];
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
      statusMsg = $t('inbox.status.flagged');
      advance();
    } catch (e) {
      // Persist failed: drop the phantom undo entry pushed above and surface the failure.
      history = history.slice(0, -1);
      statusMsg = $t('inbox.status.flagFailed', { err: String(e) });
    } finally {
      isSubmitting = false;
    }
  }

  async function undo() {
    // Guard against racing an in-flight decision. Every persisting action (accept/reject/commitEdit/flag)
    // sets isSubmitting and pushes its history entry BEFORE its await; a Backspace during that await would
    // pop that just-pushed entry and fire the inverse op (clearHumanDecision) against the SAME id while its
    // record is still in flight — and if the in-flight action then rejects, its catch does another
    // history.slice(0,-1), dropping a PREVIOUS segment's entry (permanent undo loss). The four mutators all
    // guard isSubmitting; undo must too.
    if (isSubmitting) return;
    const last = history[history.length - 1];
    if (!last) return;
    history = history.slice(0, -1); // reassignment: keeps the Undo button's disabled binding live
    try {
      // A flag set `escalated` (not a human_decision), so it needs the inverse op. Everything else is a
      // human decision, cleared to NULL (P3-3: not overwritten with a fake 'accept', which corrupted
      // agent_examples).
      if (last.decision === 'flag') {
        await api.clearEscalation(last.id);
      } else {
        await api.clearHumanDecision(last.id);
      }
      const idx = queue.findIndex((s) => s.id === last.id);
      if (idx >= 0) {
        queue[idx] = { ...last.prev };
        // Navigate directly to the undone segment, not blindly to currentIndex-1.
        // If the user accepted seg#5 then scrolled to seg#10, undo should show seg#5.
        currentIndex = idx;
      }
      statusMsg = $t('inbox.status.undone');
    } catch (e) {
      // The decision was NOT cleared — put the history entry back so the undo can be
      // retried, and tell the user instead of failing silently (which previously also
      // dropped the entry, making the undo permanently unretryable).
      history = [...history, last];
      statusMsg = $t('inbox.status.undoFailed', { err: String(e) });
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
    // Never let a modifier chord (Ctrl+A select-all, Ctrl+F, Ctrl+K palette) fire a bare-key decision,
    // and never act while focus is in ANY editable element overlaid on the inbox (e.g. the command
    // palette input) — each mis-fire silently stamps a human adjudication on the current clip.
    if (e.ctrlKey || e.metaKey || e.altKey) return;
    const target = e.target as HTMLElement | null;
    if (
      target &&
      (target.tagName === 'INPUT' || target.tagName === 'TEXTAREA' || target.isContentEditable)
    ) {
      return;
    }
    // Match on the PHYSICAL key (layout-independent): with the owner's Central Kurdish layout
    // active, e.key is 'ا'/'ب'/… and every letter shortcut below went dead until the OS layout was
    // toggled back — once per edited clip. physicalKey maps KeyA→'a', Digit1→'1' and falls back to
    // e.key for Space/Backspace/Escape/arrows.
    const key = physicalKey(e);
    switch (key) {
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
        // True-10 audit: Space now means play/pause in BOTH review surfaces (it was SKIP here while
        // play/pause in ReviewMode — a reflexive Space silently skipped the current item, and the
        // inbox had no keyboard play at all while adjudicating biometric audio by ear).
        e.preventDefault();
        inboxPlaying = !inboxPlaying;
        break;
      case 's':
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
      case 'ArrowRight':
        // Non-destructive revisit navigation (true-10 audit: the inbox forced a mouse rail-click for
        // any move that wasn't a decision — or a destructive Backspace-undo to go back).
        e.preventDefault();
        if (currentIndex < queue.length - 1) currentIndex += 1;
        break;
      case 'ArrowLeft':
        e.preventDefault();
        if (currentIndex > 0) currentIndex -= 1;
        break;
      default:
        if (key >= '1' && key <= '9') {
          const idx = parseInt(key) - 1;
          if (idx < queue.length) {
            currentIndex = idx;
          }
        }
    }
  }

  onMount(() => {
    loadQueue();
    // Round-23 #12: reflect the REAL backend autonomy level on the dial, and hold the loaded settings
    // so the dial can persist changes and the cloud-T2 consent state can be surfaced.
    api
      .getSettings()
      .then((s) => {
        settings = s;
        autonomyLevel = s.juryAutonomyLevel ?? 'propose';
      })
      .catch(() => {
        /* leave the optimistic default; the dial just won't persist until settings load */
      });
    window.addEventListener('keydown', handleKey);
  });

  onDestroy(() => {
    window.removeEventListener('keydown', handleKey);
  });
</script>

<!-- ── Root container ──────────────────────────────────────────────────────── -->
<div class="inbox-root" role="dialog" aria-modal="true" aria-labelledby="review-inbox-title">
  <!-- Header -->
  <div class="inbox-header">
    <div class="inbox-title">
      <span class="inbox-icon">📬</span>
      <h2 id="review-inbox-title">{$t('reviewInbox')}</h2>
      {#if pendingCount > 0}
        <span class="inbox-badge">{pendingCount}</span>
      {/if}
    </div>

    <!-- Run Jury Button -->
    <button
      class="btn btn-primary btn-sm"
      onclick={triggerJuryPipeline}
      disabled={isRunningJury}
      title={$t('inbox.runJuryTitle')}
    >
      {#if isRunningJury}
        <span class="spinner inline-block" style="width:10px;height:10px;"></span>
        {$t('inbox.runningJury')}
      {:else}
        ⚡ {$t('inbox.runJury')}
      {/if}
    </button>
    {#if settings && !settings.juryCloudOptIn}
      <!-- Consent affordance: the jury's T2 tier can send audio to Gemini, but cloud T2 is opt-in.
           The backend hard-refuses T2 egress when the opt-in is off (the run stays local) — surface
           that state here so it's not silent, mirroring the gated Scribe buttons. -->
      <span
        class="local-only-badge"
        data-testid="jury-local-only"
        title={$t('inbox.localOnlyTitle')}>🔒 {$t('inbox.localOnly')}</span
      >
    {/if}

    <!-- Autonomy Dial -->
    <div class="autonomy-dial" role="group" aria-label={$t('inbox.autonomyLevel')}>
      {#each [['observe', '👁', 'inbox.autonomy.observe'], ['propose', '💡', 'inbox.autonomy.propose'], ['act_confirm', '✅', 'inbox.autonomy.actConfirm'], ['act_auto', '🤖', 'inbox.autonomy.actAuto']] as [val, emoji, key]}
        <button
          type="button"
          class="dial-btn"
          class:active={autonomyLevel === val}
          aria-pressed={autonomyLevel === val}
          onclick={() => setAutonomy(val as typeof autonomyLevel)}
          title={$t(key)}>{emoji} {$t(key)}</button
        >
      {/each}
    </div>

    <button class="close-btn" onclick={onClose} aria-label={$t('inbox.close')}>✕</button>
  </div>

  {#if isLoading}
    <div class="inbox-loading">
      <span class="spinner"></span>
      {$t('inbox.loadingQueue')}
    </div>
  {:else if loadError}
    <div class="inbox-empty" role="alert" data-testid="review-inbox-load-error">
      <h3>{$t('inbox.loadErrorTitle')}</h3>
      <p>{loadError}</p>
      <button class="btn btn-primary" onclick={loadQueue}>{$t('inbox.retry')}</button>
    </div>
  {:else if queue.length === 0}
    <div class="inbox-empty">
      <div class="empty-icon">🎉</div>
      <h3>{$t('inbox.zero')}</h3>
      <p>{$t('inbox.zeroHint')}</p>
      <div class="empty-actions">
        <button class="btn btn-primary" onclick={triggerJuryPipeline} disabled={isRunningJury}>
          {isRunningJury ? $t('inbox.runningJury') : '⚡ ' + $t('inbox.runJuryPipeline')}
        </button>
        <button class="btn btn-secondary" onclick={loadQueue}>{$t('inbox.refresh')}</button>
      </div>
    </div>
  {:else}
    <div class="inbox-body">
      <!-- Queue Rail -->
      <nav class="queue-rail" aria-label={$t('inbox.segmentQueue')}>
        <div class="rail-header">{$t('inbox.queue', { n: String(queue.length) })}</div>
        <ul class="rail-list">
          {#each queue as seg, i}
            {@const band = confidenceBand(seg.agreementScore, $t, hasPoorAudio(seg))}
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
        {@const band = confidenceBand(current.agreementScore, $t, hasPoorAudio(current))}
        <article class="focus-card" aria-label={$t('inbox.segmentQueue')}>
          <!-- Segment ID + meta -->
          <div class="card-meta">
            <span class="meta-id"><bdi>{current.id.slice(0, 16)}</bdi></span>
            <span class="meta-dur"><bdi>{Math.round(current.durationMs / 1000)}s</bdi></span>
            {#if current.speakerId}
              <span class="meta-speaker"><bdi>{current.speakerId}</bdi></span>
            {/if}
          </div>

          <!-- Audio playback (LTR always). Round-23 #13: a reviewer must be able to HEAR the clip before
               adjudicating a biometric Kurdish transcript — the old static filename stub offered no way
               to listen, yet Accept stamps a human-verified label. Bounded to THIS segment's window
               (inboxRange) so Play hears only this clip, not the whole file, and keyed on the segment id
               so the player re-resolves cleanly (no cross-segment audio bleed) as the queue is navigated. -->
          <div class="waveform-zone" dir="ltr" aria-label="Audio playback">
            {#if current.audioPath}
              {#key current.id}
                <!-- True-10 audit: honor the autoplay setting (was hardcoded off) — advancing the
                     queue auto-plays the clip so adjudication needs zero play clicks. -->
                <AudioPlayer
                  audioPath={current.audioPath}
                  clipKey={current.id}
                  startTime={inboxRange.startTime}
                  endTime={inboxRange.endTime}
                  autoplay={settings?.autoplaySegments ?? false}
                  bind:playing={inboxPlaying}
                />
              {/key}
              <div class="waveform-stub">
                🔊 <bdi>{current.audioPath?.split(/[\\/]/).pop() ?? 'audio'}</bdi>
              </div>
            {:else}
              <div class="waveform-stub">🔊 <bdi>no audio</bdi></div>
            {/if}
          </div>

          <!-- Hypotheses section (RTL for Kurdish text) -->
          <section class="hyp-section">
            <h3 class="section-label">{$t('inbox.hypotheses')}</h3>
            <div class="hyp-raw" dir="rtl" lang="ckb">
              <span class="hyp-label-inline">{$t('rawAsr')}:</span>
              <span class="hyp-text">{current.rawTranscript}</span>
            </div>
            {#if current.normalizedTranscript && current.normalizedTranscript !== current.rawTranscript}
              <div class="hyp-norm" dir="rtl" lang="ckb">
                <span class="hyp-label-inline">{$t('normalized')}:</span>
                <span class="hyp-text">{current.normalizedTranscript}</span>
              </div>
            {/if}
          </section>

          <!-- Jury verdict (RTL) -->
          {#if current.verdictTranscript}
            <section class="verdict-section">
              <h3 class="section-label">🤖 {$t('inbox.juryProposes')}</h3>
              <div class="verdict-text" dir="rtl" lang="ckb">{current.verdictTranscript}</div>
            </section>
          {/if}

          <!-- Evidence & reasoning -->
          {#if current.rationale || current.evidenceJson}
            <section class="rationale-section">
              <h3 class="section-label">📋 {$t('inbox.rationale')}</h3>
              <details class="rationale-details" open>
                <summary>{$t('inbox.evidenceReasoning')}</summary>
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
              <label class="edit-label" for="edit-textarea">{$t('inbox.editLabel')}</label>
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
                <button class="btn btn-primary" onclick={commitEdit}>{$t('inbox.saveEdit')}</button>
                <button class="btn btn-secondary" onclick={() => (isEditing = false)}
                  >{$t('inbox.cancelEdit')}</button
                >
              </div>
            </div>
          {/if}

          <!-- Verb bar (Prodigy-style) -->
          <div class="verb-bar" role="group" aria-label={$t('inbox.reviewActions')}>
            <button
              class="verb-btn accept"
              onclick={accept}
              title={$t('inbox.acceptTitle')}
              id="inbox-accept"
            >
              <span class="verb-key">A</span>
              {$t('inbox.accept')}
            </button>
            <button
              class="verb-btn edit"
              onclick={startEdit}
              title={$t('inbox.editTitle')}
              id="inbox-edit"
            >
              <span class="verb-key">E</span>
              {$t('inbox.edit')}
            </button>
            <button
              class="verb-btn reject"
              onclick={reject}
              title={$t('inbox.rejectTitle')}
              id="inbox-reject"
            >
              <span class="verb-key">X</span>
              {$t('inbox.reject')}
            </button>
            <button
              class="verb-btn skip"
              onclick={skip}
              title={$t('inbox.skipTitle')}
              id="inbox-skip"
            >
              <span class="verb-key">⎵</span>
              {$t('inbox.skip')}
            </button>
            <button
              class="verb-btn flag"
              onclick={flag}
              title={$t('inbox.flagTitle')}
              id="inbox-flag"
            >
              <span class="verb-key">F</span>
              {$t('inbox.flag')}
            </button>
            <button
              class="verb-btn undo"
              onclick={undo}
              title={$t('inbox.undoTitle')}
              id="inbox-undo"
              disabled={history.length === 0}
            >
              <span class="verb-key">⌫</span>
              {$t('undo')}
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
    min-width: 0;
    background: var(--app-bg);
    color: var(--text);
    font-family: var(--font-sans);
    border-radius: 12px;
    overflow: hidden;
  }

  /* ── Header ─────────────────────────────────────────────────────────────────── */
  .inbox-header {
    display: flex;
    flex-wrap: wrap;
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
    flex: 1 1 auto;
    min-width: 0;
  }
  .inbox-title h2 {
    margin: 0;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
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
    flex-wrap: wrap;
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
  .local-only-badge {
    margin-inline-start: 6px;
    font-size: 0.68rem;
    opacity: 0.75;
    white-space: nowrap;
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
  .empty-actions {
    display: flex;
    flex-wrap: wrap;
    justify-content: center;
    gap: 10px;
    width: 100%;
    margin-top: 10px;
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
    min-width: 0;
    min-height: 0;
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
    min-width: 0;
    min-height: 0;
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
  .waveform-zone {
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
    /* isolate, not embed: the transcript sits inline after an LTR-ish "Raw ASR:" label in an RTL
       block. `embed` still lets a transcript that STARTS with a Latin token or digit reorder across
       the label boundary (colon/label jump to the wrong side). `isolate` gives the transcript its
       own bidi context so it can never reflow the label — same isolation the <bdi> model-name spans use. */
    unicode-bidi: isolate;
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

  /* WCAG reflow: at a 320 CSS-pixel viewport the App overlay leaves roughly 272px after padding.
     Stack the rail above the card and let header controls form deliberate rows; nothing is clipped
     behind the root's overflow boundary, while the queue remains a one-axis scroll region. */
  @media (max-width: 480px) {
    .inbox-root {
      border-radius: 8px;
    }
    .inbox-header {
      align-items: center;
      gap: 8px;
      padding: 10px;
    }
    .inbox-title {
      order: 1;
      flex: 1 1 calc(100% - 2.5rem);
    }
    .close-btn {
      order: 2;
      flex: 0 0 auto;
    }
    .inbox-header > .btn {
      order: 3;
      flex: 1 1 auto;
      min-width: 0;
      white-space: normal;
    }
    .local-only-badge {
      order: 3;
      flex: 0 1 auto;
      min-width: 0;
      margin-inline-start: 0;
      white-space: normal;
    }
    .autonomy-dial {
      order: 4;
      display: grid;
      flex: 1 1 100%;
      grid-template-columns: repeat(2, minmax(0, 1fr));
      width: 100%;
    }
    .dial-btn {
      min-width: 0;
      padding: 5px 6px;
      white-space: normal;
    }
    .inbox-body {
      flex-direction: column;
    }
    .queue-rail {
      width: 100%;
      max-height: 6.5rem;
      border-right: 0;
      border-bottom: 1px solid var(--border);
    }
    .rail-list {
      display: flex;
      flex: 0 0 auto;
      overflow-x: auto;
      overflow-y: hidden;
      padding: 4px;
    }
    .rail-row {
      flex: 0 0 7rem;
      min-width: 0;
    }
    .rail-item {
      width: calc(100% - 4px);
      margin: 1px 2px;
    }
    .focus-card {
      width: 100%;
      padding: 12px;
    }
    .inbox-loading,
    .inbox-empty {
      min-width: 0;
      padding: 16px;
    }
    .empty-actions {
      flex-direction: column;
    }
    .empty-actions .btn {
      width: 100%;
    }
  }
</style>
