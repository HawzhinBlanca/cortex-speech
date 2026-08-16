/**
 * Machine-stable escalation reason codes, as recorded by the jury into `evidence_json`.
 *
 * These answer "WHY WAS THIS DECIDED", which is a different question from "what is true now". The live
 * audio badge derives from the row's CURRENT snr/clipping values; these codes are the historical record
 * of the decision. A clip escalated for `low_snr` whose audio was later replaced should still say why it
 * was escalated while the badge reads clean — so this module never feeds the badge, and the badge never
 * feeds this. Conflating them would be the three-copy threshold bug in reverse.
 *
 * The vocabulary is pinned in Rust (`jury::reason`) and asserted there by literal value, because the
 * strings are persisted and old rows keep the old spelling.
 */

/** Every code the backend can currently write. Mirrors `jury::reason`. */
export const KNOWN_REASON_CODES = [
  'low_snr',
  'clipping',
  'single_recognizer',
  'model_disagreement',
  'uncalibrated_bucket',
  'policy_hold',
  'missing_audio',
  't2_no_majority',
  't1_unresolved',
] as const;

export type ReasonCode = (typeof KNOWN_REASON_CODES)[number];

export interface EscalationEvidence {
  /** Codes in the order the backend recorded them. Empty when the row carries none. */
  reasonCodes: string[];
  /** Which routing policy decided this. Absent on rows written before the policy was versioned. */
  policyVersion: string | null;
}

/**
 * Parse `evidence_json` into its reason codes.
 *
 * Returns `null` — NOT an empty result — when there is nothing to show: no evidence recorded (every row
 * decided before the codes existed, and every row never escalated), unparseable JSON, or no
 * `reasonCodes` key. The distinction matters at the call site: `null` means "this row has no decision
 * record", which must render NOTHING. An empty array would render as "escalated for no reason", which is
 * a claim the data does not support.
 *
 * Unknown codes are PRESERVED rather than filtered. If the backend gains a tenth code and this file has
 * not caught up, the reviewer should see the raw string, not a silently shorter list — a UI that drops
 * what it does not recognise hides exactly the new information someone just added.
 */
export function parseEscalationEvidence(
  evidenceJson: string | null | undefined,
): EscalationEvidence | null {
  if (!evidenceJson || !evidenceJson.trim()) return null;
  let parsed: unknown;
  try {
    parsed = JSON.parse(evidenceJson);
  } catch {
    // Malformed evidence is not a reason to break the review screen. Render nothing and move on.
    return null;
  }
  if (typeof parsed !== 'object' || parsed === null) return null;
  const raw = (parsed as Record<string, unknown>).reasonCodes;
  if (!Array.isArray(raw)) return null;
  const reasonCodes = raw.filter((c): c is string => typeof c === 'string' && c.length > 0);
  if (reasonCodes.length === 0) return null;
  const pv = (parsed as Record<string, unknown>).policyVersion;
  return { reasonCodes, policyVersion: typeof pv === 'string' ? pv : null };
}

/**
 * i18n key for a reason code, or `null` when the code is unknown to this build.
 *
 * A `null` tells the caller to render the raw code verbatim. That is deliberately visible: an
 * untranslated string in the UI is a prompt to add the translation, whereas a dropped code is invisible
 * and stays that way.
 */
export function reasonLabelKey(code: string): string | null {
  return (KNOWN_REASON_CODES as readonly string[]).includes(code) ? `reason.${code}` : null;
}

/**
 * Severity tint for a code, used only for colour.
 *
 * `policy_hold` is deliberately NEUTRAL: it is not a fault in the clip at all, it means the autonomy
 * dial forbade a machine commit. Colouring it like bad audio would tell the reviewer to distrust a clip
 * that nothing is wrong with.
 */
export function reasonTone(code: string): 'danger' | 'warning' | 'neutral' {
  switch (code) {
    case 'missing_audio':
      return 'danger';
    case 'low_snr':
    case 'clipping':
    case 'single_recognizer':
    case 'model_disagreement':
    case 't2_no_majority':
      return 'warning';
    // uncalibrated_bucket / t1_unresolved / policy_hold describe the EVIDENCE or the OPERATOR's
    // settings, not the clip. Neutral, so the reviewer reads them as context rather than as a defect.
    default:
      return 'neutral';
  }
}
