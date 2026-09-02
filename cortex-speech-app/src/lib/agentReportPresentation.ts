import type { AgentImportReport } from './commands';
import type { Translate, TranslationKey } from './i18n';

type SourceReference = NonNullable<AgentImportReport>['summary']['sourceReferences'][number];

function boundedBasename(value: unknown): string {
  const parts = String(value ?? '')
    .slice(-1024)
    .split(/[\\/]/)
    .filter(Boolean);
  const basename = parts.length ? parts[parts.length - 1] : '';
  return Array.from(basename)
    .filter((character) => {
      const point = character.codePointAt(0) ?? 0;
      const isBidiControl =
        point === 0x061c ||
        point === 0x200e ||
        point === 0x200f ||
        (point >= 0x202a && point <= 0x202e) ||
        (point >= 0x2066 && point <= 0x2069);
      return point > 31 && point !== 127 && !isBidiControl;
    })
    .join('')
    .slice(0, 160)
    .trim();
}

export function formatAgentReportDate(value: string, translate: Translate): string {
  const date = new Date(value);
  return Number.isNaN(date.getTime())
    ? translate('agentReport.unknownDate')
    : date.toLocaleString();
}

export function formatAgentReportPercent(ready: number, total: number): string {
  if (total <= 0) return '0%';
  return `${Math.round((ready / total) * 100)}%`;
}

export function publicAgentReportFileLabel(value: unknown, translate: Translate): string {
  return boundedBasename(value) || translate('events.unknownFile');
}

export function publicAgentReportIdentifier(value: unknown): string {
  const basename = boundedBasename(value);
  return /^[A-Za-z0-9][A-Za-z0-9_.:@+-]{0,159}$/.test(basename) ? basename : '';
}

export function compactAgentReportModels(
  models: string[],
  authoritativeTotal: number,
  translate: Translate,
): string {
  const labels = models.map(publicAgentReportIdentifier).filter(Boolean);
  const total =
    Number.isSafeInteger(authoritativeTotal) && authoritativeTotal >= 0
      ? authoritativeTotal
      : labels.length;
  if (!total) return translate('agentReport.none');
  const visible = labels.slice(0, Math.min(3, total));
  if (!visible.length) {
    const unknown = translate('agentReport.unknown');
    return unknown + (total > 1 ? ` +${total - 1}` : '');
  }
  return visible.join(', ') + (total > visible.length ? ` +${total - visible.length}` : '');
}

export function formatSourceReferenceIdentity(
  reference: SourceReference,
  translate: Translate,
): string {
  const hash =
    reference.audioContentHash && /^[a-f0-9]{64}$/i.test(reference.audioContentHash)
      ? reference.audioContentHash.slice(0, 12)
      : '';
  const size =
    typeof reference.audioSizeBytes === 'number' && Number.isFinite(reference.audioSizeBytes)
      ? translate('agentReport.byteCount', { count: String(reference.audioSizeBytes) })
      : '';
  return [hash ? translate('agentReport.hashValue', { hash }) : '', size]
    .filter(Boolean)
    .join(' | ');
}

export function topAgentReportCounts(
  counts: Record<string, number> | undefined,
  limit: number,
): Array<[string, number]> {
  return Object.entries(counts ?? {})
    .filter(
      ([key, count]) =>
        Boolean(publicAgentReportIdentifier(key)) && Number.isSafeInteger(count) && count >= 0,
    )
    .sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]))
    .slice(0, limit);
}

export function agentStageTone(status: string): string {
  // Disabled optional work is neutral: valid configuration, but neither proven coverage nor a fault.
  if (status === 'not_required') return 'text-cortex-300 bg-cortex-800/40 border-cortex-700/40';
  if (status === 'ready' || status === 'completed')
    return 'text-emerald-300 bg-emerald-950/30 border-emerald-800/40';
  if (status === 'running' || status === 'degraded' || status === 'needs_review')
    return 'text-amber-300 bg-amber-950/30 border-amber-800/40';
  if (status === 'blocked') return 'text-red-300 bg-red-950/30 border-red-800/40';
  return 'text-cortex-300 bg-cortex-900/50 border-cortex-800/40';
}

const statusKeys: Readonly<Record<string, TranslationKey>> = {
  not_required: 'agentReport.status.notRequired',
  ready: 'agentReport.status.ready',
  completed: 'agentReport.status.completed',
  skipped: 'agentReport.status.skipped',
  failed: 'agentReport.status.failed',
  running: 'agentReport.status.running',
  degraded: 'agentReport.status.degraded',
  needs_review: 'agentReport.status.needsReview',
  blocked: 'agentReport.status.blocked',
  unprocessed: 'agentReport.status.unprocessed',
};

const checkLabelKeys: Readonly<Record<string, TranslationKey>> = {
  source_reference: 'agentReport.check.sourceReference',
  primary_asr: 'agentReport.check.primaryAsr',
  hypothesis_coverage: 'agentReport.check.hypothesisCoverage',
  readiness_snapshot: 'agentReport.check.readinessSnapshot',
};

const stageLabelKeys: Readonly<Record<string, TranslationKey>> = {
  source_reference: 'agentReport.stage.sourceReference',
  audio_chunking: 'agentReport.stage.audioChunking',
  multi_model_hypotheses: 'agentReport.stage.multiModelHypotheses',
  jury_adjudication: 'agentReport.stage.juryAdjudication',
  dataset_promotion: 'agentReport.stage.datasetPromotion',
  agent_report: 'agentReport.stage.agentReport',
};

function translatedLabel(
  labels: Readonly<Record<string, TranslationKey>>,
  value: string,
  translate: Translate,
): string {
  const key = labels[value];
  return key ? translate(key) : translate('agentReport.unknown');
}

export const agentStatusLabel = (status: string, translate: Translate): string =>
  translatedLabel(statusKeys, status, translate);

export const agentCheckLabel = (check: string, translate: Translate): string =>
  translatedLabel(checkLabelKeys, check, translate);

export const agentStageLabel = (stage: string, translate: Translate): string =>
  translatedLabel(stageLabelKeys, stage, translate);
