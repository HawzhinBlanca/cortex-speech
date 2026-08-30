<script lang="ts">
  import { t } from './i18n';
  import { safeInboxEvidence } from './reviewInboxDecisions.svelte';
  import { confidenceBand } from './reviewLabels';
  import type { SpeechSegment } from './types';

  interface Props {
    current: SpeechSegment;
  }

  let { current }: Props = $props();
  const poorAudio = $derived(
    (current.snrDb != null && current.snrDb < 5) ||
      (current.clippingRatio != null && current.clippingRatio > 0.1),
  );
  const band = $derived(confidenceBand(current.agreementScore, $t, poorAudio));
</script>

<div class="card-meta">
  <span class="meta-pill"><bdi>{current.id.slice(0, 16)}</bdi></span>
  <span class="meta-pill">
    <bdi>
      {$t('inbox.durationSeconds', {
        seconds: String(Math.round(current.durationMs / 1000)),
      })}
    </bdi>
  </span>
  {#if current.speakerId}<span class="meta-pill"><bdi>{current.speakerId}</bdi></span>{/if}
</div>

<section class="text-section">
  <h3 class="section-label">{$t('inbox.hypotheses')}</h3>
  <div class="hypothesis" dir="rtl" lang="ckb">
    <span class="hyp-label-inline">{$t('rawAsr')}:</span>
    <span class="hyp-text">{current.rawTranscript}</span>
  </div>
  {#if current.normalizedTranscript && current.normalizedTranscript !== current.rawTranscript}
    <div class="hypothesis" dir="rtl" lang="ckb">
      <span class="hyp-label-inline">{$t('normalized')}:</span>
      <span class="hyp-text">{current.normalizedTranscript}</span>
    </div>
  {/if}
</section>

{#if current.verdictTranscript}
  <section class="text-section">
    <h3 class="section-label">{$t('inbox.juryProposes')}</h3>
    <div class="verdict-text" dir="rtl" lang="ckb">{current.verdictTranscript}</div>
  </section>
{/if}

{#if current.rationale || current.evidenceJson}
  <section class="text-section">
    <h3 class="section-label">{$t('inbox.rationale')}</h3>
    <details class="rationale-details" open>
      <summary>{$t('inbox.evidenceReasoning')}</summary>
      {#if current.rationale}<p class="rationale-text">{current.rationale}</p>{/if}
      {#if current.evidenceJson}
        <pre class="evidence-pre">{safeInboxEvidence(current.evidenceJson)}</pre>
      {/if}
    </details>
  </section>
{/if}

<div class="confidence-strip" style="border-left-color:{band.color}">
  <span>{band.label}</span>
</div>

<style>
  .card-meta {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 10px;
  }
  .meta-pill {
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--surface-2);
    padding: 2px 8px;
    color: var(--text-muted);
    font-family: var(--font-mono);
    font-size: 0.7rem;
  }
  .text-section {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .section-label {
    margin: 0;
    color: var(--text-muted);
    font-size: 0.72rem;
    font-weight: 600;
    letter-spacing: 0.04em;
    text-transform: uppercase;
  }
  .hypothesis {
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--surface-2);
    padding: 10px 14px;
    color: var(--text);
    font-family: var(--font-kurdish);
    font-size: 1.05rem;
    line-height: 1.9;
    text-align: start;
  }
  .hyp-label-inline {
    display: inline-block;
    margin-inline-end: 8px;
    color: var(--text-muted);
    font-family: var(--font-mono);
    font-size: 0.65rem;
  }
  .hyp-text {
    direction: rtl;
    unicode-bidi: isolate;
  }
  .verdict-text {
    border: 1px solid color-mix(in srgb, var(--accent) 35%, transparent);
    border-radius: 8px;
    background: var(--accent-soft);
    padding: 12px 16px;
    color: var(--text);
    font-family: var(--font-kurdish);
    font-size: 1.1rem;
    line-height: 1.9;
    text-align: start;
  }
  .rationale-details {
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--surface-inset);
    padding: 8px 12px;
    font-size: 0.8rem;
  }
  .rationale-details summary {
    cursor: pointer;
    color: var(--text-muted);
  }
  .rationale-text {
    margin: 6px 0 0;
    color: var(--text-muted);
    line-height: 1.6;
  }
  .evidence-pre {
    overflow-x: auto;
    border-radius: 4px;
    background: var(--surface-inset);
    padding: 8px;
    color: var(--text-muted);
    font-size: 0.7rem;
    white-space: pre-wrap;
    word-break: break-all;
  }
  .confidence-strip {
    display: flex;
    align-items: center;
    gap: 8px;
    border: 1px solid var(--border);
    border-left-width: 3px;
    border-radius: 6px;
    background: var(--surface-2);
    padding: 8px 14px;
    color: var(--text);
    font-size: 0.8rem;
  }
</style>
