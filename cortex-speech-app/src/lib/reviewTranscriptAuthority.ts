import type { SpeechSegment } from './types';

type ReviewTranscriptFields = Pick<
  SpeechSegment,
  'rawTranscript' | 'annotatedTranscript' | 'verdictTranscript' | 'humanDecision' | 'verdict'
>;

function hasHumanTextVerdict(segment: ReviewTranscriptFields): boolean {
  const decision = (segment.humanDecision ?? '').toLowerCase();
  const verdict = (segment.verdict ?? '').toLowerCase();
  return (
    decision === 'accept' ||
    decision === 'edit' ||
    decision === 'human_accept' ||
    decision === 'human_edit' ||
    verdict === 'human_accept' ||
    verdict === 'human_edit'
  );
}

/** Verbatim Law: proven human truth, then human annotation, then immutable champion raw. */
export function reviewTranscript(segment: ReviewTranscriptFields): string {
  if (hasHumanTextVerdict(segment) && segment.verdictTranscript?.trim()) {
    return segment.verdictTranscript;
  }
  if (segment.annotatedTranscript?.trim()) return segment.annotatedTranscript;
  return segment.rawTranscript ?? '';
}
