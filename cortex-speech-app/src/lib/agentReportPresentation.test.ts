import { describe, expect, it } from 'vitest';
import { agentStatusLabel, compactAgentReportModels } from './agentReportPresentation';
import { en } from './i18n/en';
import type { Translate } from './i18n';

const translate: Translate = (key, params) => {
  let value: string = en[key];
  for (const [name, replacement] of Object.entries(params ?? {})) {
    value = value.replace(`{${name}}`, replacement);
  }
  return value;
};

describe('agent report presentation', () => {
  it('labels every legitimate skipped stage without collapsing it to unknown', () => {
    expect(agentStatusLabel('skipped', translate)).toBe('Skipped');
  });

  it('uses the authoritative model total instead of the bounded preview length', () => {
    expect(
      compactAgentReportModels(['model-a', 'model-b', 'model-c', 'model-d'], 10_000, translate),
    ).toBe('model-a, model-b, model-c +9997');
  });

  it('stays honest when an authoritative total has no safe preview label', () => {
    expect(compactAgentReportModels(['token=secret SELECT'], 2, translate)).toBe(
      `${en['agentReport.unknown']} +1`,
    );
    expect(compactAgentReportModels([], 0, translate)).toBe(en['agentReport.none']);
  });
});
