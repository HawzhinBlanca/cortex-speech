import type { AppSettings } from './stores/settingsStore';
import { ADVISORY_CLOUD_MODEL } from './stores/settingsStore';

/** Backend settings shape returned by Tauri `get_settings` / `update_settings`. */
export interface BackendSettings {
  // Internal model/output paths are deliberately absent from the generated renderer snapshot.
  // They remain optional here only so legacy adapter tests and compatibility callers can preserve
  // an already-held value; the revision-guarded patch never transmits either field.
  model_dir?: string;
  output_dir?: string;
  asr_model_size: string;
  use_finetuned_asr?: boolean;
  vad_threshold: number;
  min_segment_duration_ms: number;
  max_segment_duration_ms: number;
  num_asr_threads: number;
  enable_gpu: boolean;
  language: string;
  export_format: string;
  auto_normalize: boolean;
  verbalize_numbers: boolean;
  auto_align: boolean;
  assign_speaker_from_filename: boolean;
  enable_diarization: boolean;
  enable_denoising: boolean;
  autoplay_segments?: boolean;
  max_speakers: number;
  max_wer_threshold: number;
  max_cer_threshold: number;
  enforce_quality_gates: boolean;
  theme: string;
  llm_mode: string;
  llm_endpoint: string;
  // Secret values never cross the generated settings contract. Key writes use set_api_key.
  llm_api_key?: string;
  llm_api_key_configured: boolean;
  cloud_llm_opt_in: boolean;
  llm_system_prompt: string;
  llm_model: string;
  external_asr_script_path: string;
  hf_train_ratio: number;
  hf_val_ratio: number;
  hf_test_ratio: number;
  hf_split_seed: number;
  hf_speaker_disjoint: boolean;
  hf_license: string;
  // Listening Jury
  jury_cloud_opt_in?: boolean;
  jury_model?: string;
  jury_provider?: string;
  source_reference_models?: string[];
  jury_self_consistency_n?: number;
  jury_autonomy_level?: string;
  jury_t1_threshold?: number;
}

function themeFromBackend(value: string): AppSettings['theme'] {
  switch (value) {
    case 'Light':
      return 'light';
    case 'System':
      return 'system';
    default:
      return 'dark';
  }
}

function themeToBackend(value: AppSettings['theme']): string {
  switch (value) {
    case 'light':
      return 'Light';
    case 'system':
      return 'System';
    default:
      return 'Dark';
  }
}

function exportFormatFromBackend(value: string): AppSettings['exportFormat'] {
  switch (value) {
    case 'Csv':
      return 'csv';
    case 'Jsonl':
      return 'jsonl';
    case 'Parquet':
      return 'parquet';
    default:
      return 'json';
  }
}

function exportFormatToBackend(value: AppSettings['exportFormat']): string {
  switch (value) {
    case 'csv':
      return 'Csv';
    case 'jsonl':
      return 'Jsonl';
    case 'parquet':
      return 'Parquet';
    default:
      return 'Json';
  }
}

function asrModelFromBackend(_value: string): AppSettings['asrModel'] {
  return 'wsl-7b';
}

function asrModelToBackend(_value: AppSettings['asrModel']): string {
  return 'WSL7B';
}

function llmModeFromBackend(value: string): AppSettings['llmMode'] {
  switch (value) {
    case 'Gemini':
      return 'Gemini';
    case 'None':
    case 'Off':
      return 'None';
    case 'Local':
    default:
      return 'Local';
  }
}

export function mapBackendToFrontend(raw: BackendSettings): AppSettings {
  return {
    theme: themeFromBackend(raw.theme),
    autoNormalize: raw.auto_normalize,
    verbalizeNumbers: raw.verbalize_numbers ?? true,
    autoAlign: raw.auto_align,
    exportFormat: exportFormatFromBackend(raw.export_format),
    asrModel: asrModelFromBackend(raw.asr_model_size),
    useFinetuned: false,
    vadThreshold: raw.vad_threshold,
    minSegmentSec: Math.round(raw.min_segment_duration_ms / 1000),
    maxSegmentSec: Math.round(raw.max_segment_duration_ms / 1000),
    numThreads: raw.num_asr_threads,
    enableGpu: raw.enable_gpu,
    language: raw.language,
    enableDiarization: raw.enable_diarization ?? true,
    enableDenoising: raw.enable_denoising ?? false,
    maxSpeakers: raw.max_speakers ?? 8,
    assignSpeakerFromFilename: raw.assign_speaker_from_filename ?? true,
    maxWerThreshold: raw.max_wer_threshold ?? 0.35,
    maxCerThreshold: raw.max_cer_threshold ?? 0.2,
    enforceQualityGates: raw.enforce_quality_gates ?? false,
    autoplaySegments: raw.autoplay_segments ?? false,
    hfTrainRatio: raw.hf_train_ratio ?? 0.8,
    hfValRatio: raw.hf_val_ratio ?? 0.1,
    hfTestRatio: raw.hf_test_ratio ?? 0.1,
    hfSplitSeed: raw.hf_split_seed ?? 42,
    hfSpeakerDisjoint: raw.hf_speaker_disjoint ?? true,
    hfLicense: raw.hf_license ?? 'mit',
    llmMode: llmModeFromBackend(raw.llm_mode),
    llmEndpoint: raw.llm_endpoint ?? 'http://127.0.0.1:11434/v1/chat/completions',
    llmApiKey: '',
    llmApiKeyConfigured: raw.llm_api_key_configured ?? false,
    cloudLlmOptIn: raw.cloud_llm_opt_in ?? false,
    llmSystemPrompt:
      raw.llm_system_prompt ??
      'You are an expert Kurdish linguist. Fix the phonetic transcription errors in the following text, preserving the exact meaning. Output ONLY the corrected text, no explanations.',
    llmModel:
      llmModeFromBackend(raw.llm_mode) === 'Gemini'
        ? ADVISORY_CLOUD_MODEL
        : (raw.llm_model ?? 'heretic-final:latest'),
    externalAsrScriptPath: raw.external_asr_script_path ?? '',
    // Listening Jury
    juryCloudOptIn: raw.jury_cloud_opt_in ?? false,
    juryModel: ADVISORY_CLOUD_MODEL,
    juryProvider: raw.jury_provider === 'openrouter' ? 'openrouter' : 'gemini',
    sourceReferenceModels: [ADVISORY_CLOUD_MODEL],
    jurySelfConsistencyN: raw.jury_self_consistency_n ?? 3,
    juryAutonomyLevel: (raw.jury_autonomy_level as AppSettings['juryAutonomyLevel']) ?? 'propose',
    juryT1Threshold: raw.jury_t1_threshold ?? 0.75,
  };
}

export function mapFrontendToBackend(ui: AppSettings, existing: BackendSettings): BackendSettings {
  // Round-23 #11: clearing a `<input type="number">` sets its bound value to `null` (or NaN). The Rust
  // settings struct declares these as `u32`/`f64`, and serde rejects a present `null` — so a single
  // emptied field made `update_settings` fail to deserialize and discarded the WHOLE save (every other
  // edit lost). Coerce each numeric field to a finite number, falling back to the last-saved value.
  const num = (v: number, fallback: number): number =>
    typeof v === 'number' && Number.isFinite(v) ? v : fallback;
  return {
    ...existing,
    theme: themeToBackend(ui.theme),
    auto_normalize: ui.autoNormalize,
    verbalize_numbers: ui.verbalizeNumbers,
    auto_align: ui.autoAlign,
    export_format: exportFormatToBackend(ui.exportFormat),
    asr_model_size: asrModelToBackend(ui.asrModel),
    use_finetuned_asr: false,
    vad_threshold: num(ui.vadThreshold, existing.vad_threshold),
    min_segment_duration_ms: Number.isFinite(ui.minSegmentSec)
      ? ui.minSegmentSec * 1000
      : existing.min_segment_duration_ms,
    max_segment_duration_ms: Number.isFinite(ui.maxSegmentSec)
      ? ui.maxSegmentSec * 1000
      : existing.max_segment_duration_ms,
    num_asr_threads: num(ui.numThreads, existing.num_asr_threads),
    enable_gpu: ui.enableGpu,
    language: ui.language,
    assign_speaker_from_filename: ui.assignSpeakerFromFilename,
    enable_diarization: ui.enableDiarization,
    enable_denoising: ui.enableDenoising,
    autoplay_segments: ui.autoplaySegments,
    max_speakers: num(ui.maxSpeakers, existing.max_speakers),
    max_wer_threshold: num(ui.maxWerThreshold, existing.max_wer_threshold),
    max_cer_threshold: num(ui.maxCerThreshold, existing.max_cer_threshold),
    enforce_quality_gates: ui.enforceQualityGates,
    llm_mode: ui.llmMode,
    llm_endpoint: ui.llmEndpoint,
    llm_api_key: ui.llmApiKey,
    llm_api_key_configured: ui.llmApiKeyConfigured || ui.llmApiKey.length > 0,
    cloud_llm_opt_in: ui.cloudLlmOptIn,
    llm_system_prompt: ui.llmSystemPrompt,
    llm_model: ui.llmMode === 'Gemini' ? ADVISORY_CLOUD_MODEL : ui.llmModel,
    external_asr_script_path: ui.externalAsrScriptPath,
    hf_train_ratio: num(ui.hfTrainRatio, existing.hf_train_ratio),
    hf_val_ratio: num(ui.hfValRatio, existing.hf_val_ratio),
    hf_test_ratio: num(ui.hfTestRatio, existing.hf_test_ratio),
    hf_split_seed: num(ui.hfSplitSeed, existing.hf_split_seed),
    hf_speaker_disjoint: ui.hfSpeakerDisjoint,
    hf_license: ui.hfLicense,
    // Listening Jury
    jury_cloud_opt_in: ui.juryCloudOptIn,
    jury_model: ADVISORY_CLOUD_MODEL,
    jury_provider: ui.juryProvider,
    source_reference_models: [ADVISORY_CLOUD_MODEL],
    jury_self_consistency_n: num(ui.jurySelfConsistencyN, existing.jury_self_consistency_n ?? 3),
    jury_autonomy_level: ui.juryAutonomyLevel,
    jury_t1_threshold: num(ui.juryT1Threshold, existing.jury_t1_threshold ?? 0.75),
  };
}
