export interface SpeechSegment {
  id: string;
  createdAt?: string;
  audioPath: string;
  rawTranscript: string;
  normalizedTranscript: string | null;
  annotatedTranscript: string | null;
  alignmentJson: string | null;
  durationMs: number;
  speakerId: string | null;
  verified: boolean;
  // Backend sends these as Rust Option<f64> with no skip_serializing_if, so an
  // unset value arrives as JSON `null` (not `undefined`). Type them honestly so the
  // type-checker forces a null-aware guard before any `.toFixed()`.
  confidence?: number | null;
  ctcScore?: number | null;
  // Also Rust Option<f64> → may arrive as JSON null (see confidence/ctcScore above).
  clippingRatio?: number | null;
  rmsDb?: number | null;
  snrDb?: number | null;
  split?: string | null;
  oodScore?: number | null;
  // Jury fields (Migration v11)
  verdict?: string | null;
  verdictTranscript?: string | null;
  rationale?: string | null;
  evidenceJson?: string | null;
  agentConfidence?: number | null;
  escalated?: boolean;
  humanDecision?: string | null;
  correctedAt?: string | null;
  isGold?: boolean;
  /** 'ctc_forced' | 'energy_heuristic' | null (never aligned) — Migration v12 */
  alignmentQuality?: string | null;
}

export interface GoldSegment {
  id: string;
  audioPath: string;
  reference: string;
  isHoldout: boolean;
  createdAt?: string;
}

export interface EvalRun {
  id: string;
  modelId: string;
  runAt: string;
  numSegs: number;
  wer: number;
  cer: number;
  metaJson?: string | null;
}

export interface EvalSegmentResult {
  goldId: string;
  audioPath: string;
  reference: string;
  hypothesis: string;
  wer: number;
  cer: number;
}

export interface EvalRunResult {
  run: EvalRun;
  segments: EvalSegmentResult[];
}

export interface EscalationTrendPoint {
  date: string;
  escalationRate: number;
  total: number;
  escalated: number;
}

export interface LabelQualityLift {
  n: number;
  rawMicroCer: number;
  juryMicroCer: number;
  cerLift: number;
  liftCiLow: number;
  liftCiHigh: number;
}

export interface FewShotExample {
  id: string;
  segmentId: string;
  wrongTranscript: string;
  humanFix: string;
  createdAt?: string;
}

export interface T0GateReport {
  total: number;
  autoAccepted: number;
  escalated: number;
}

export interface WordTimestamp {
  word: string;
  start: number;
  end: number;
  confidence: number;
}

export interface DatasetStats {
  totalSegments: number;
  totalDurationSeconds: number;
  avgDurationSeconds: number;
  verifiedCount: number;
  pendingCount: number;
  verificationRate: number;
  uniqueSpeakers: number;
  totalChars: number;
  avgCharsPerSegment: number;
  durationHistogram: DurationHistogram;
  topSpeakers: SpeakerStat[];
}

export interface DurationHistogram {
  under5s: number;
  under10s: number;
  under15s: number;
  under30s: number;
  over30s: number;
}

export interface SpeakerStat {
  speakerId: string;
  segmentCount: number;
  totalDurationSeconds: number;
}

export interface ImportProgress {
  current: number;
  total: number;
  status: string;
}

export interface ValidationResult {
  valid: boolean;
  totalFiles: number;
  missingFiles: number;
  emptyTranscripts: number;
  duplicateContent: number;
  errors: string[];
}

export interface TextDiff {
  raw: string;
  annotated: string;
  changes: DiffChange[];
}

export interface DiffChange {
  type: 'equal' | 'insert' | 'delete' | 'replace';
  value: string;
}

export interface PipelineProgressEvent {
  current: number;
  total: number;
  file: string;
  status: string;
}

export interface PipelineErrorEvent {
  file: string;
  error: string;
}

export interface PipelineCompleteEvent {
  total: number;
  succeeded: number;
  failed: number;
}

export interface AppSettings {
  theme: string;
  autoNormalize: boolean;
  autoAlign: boolean;
  exportFormat: string;
  asrModel: string;
  vadThreshold: number;
  minSegmentSec: number;
  maxSegmentSec: number;
  numThreads: number;
  enableGpu: boolean;
  language: string;
  autoplaySegments: boolean;
  sourceReferenceModels: string[];
}
