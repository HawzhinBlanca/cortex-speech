import type { TranslationKey } from './en';

export type AutonomyLevel = 'observe' | 'propose' | 'act_confirm' | 'act_auto';

export const autonomyValues = [
  'observe',
  'propose',
  'act_confirm',
  'act_auto',
] as const satisfies readonly AutonomyLevel[];

const autonomyLabelKeys: Readonly<Record<AutonomyLevel, TranslationKey>> = {
  observe: 'inbox.autonomy.observe',
  propose: 'inbox.autonomy.propose',
  act_confirm: 'inbox.autonomy.actConfirm',
  act_auto: 'inbox.autonomy.actAuto',
};

export function autonomyLabelKey(level: AutonomyLevel): TranslationKey {
  return autonomyLabelKeys[level];
}
