import type { AppSettings } from './stores/settingsStore';

/** Backend settings shape returned by Tauri `get_settings` / `update_settings`. */
export interface BackendSettings {
  model_dir: string;
  output_dir: string;
  asr_provider: string;
  asr_model_size: string;
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
  max_speakers: number;
  max_wer_threshold: number;
  max_cer_threshold: number;
  enforce_quality_gates: boolean;
  theme: string;
  llm_mode: string;
  llm_endpoint: string;
  llm_api_key: string;
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

function asrModelFromBackend(value: string): AppSettings['asrModel'] {
  if (value === 'CTC1B') return 'ctc-1b';
  if (value === 'WSL7B') return 'wsl-7b';
  return 'ctc-300m';
}

function asrModelToBackend(value: AppSettings['asrModel']): string {
  if (value === 'ctc-1b') return 'CTC1B';
  if (value === 'wsl-7b') return 'WSL7B';
  return 'CTC300M';
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
    autoplaySegments: false,
    hfTrainRatio: raw.hf_train_ratio ?? 0.8,
    hfValRatio: raw.hf_val_ratio ?? 0.1,
    hfTestRatio: raw.hf_test_ratio ?? 0.1,
    hfSplitSeed: raw.hf_split_seed ?? 42,
    hfSpeakerDisjoint: raw.hf_speaker_disjoint ?? true,
    hfLicense: raw.hf_license ?? 'mit',
    llmMode: llmModeFromBackend(raw.llm_mode),
    llmEndpoint: raw.llm_endpoint ?? 'http://127.0.0.1:11434/v1/chat/completions',
    llmApiKey: raw.llm_api_key ?? '',
    llmApiKeyConfigured: raw.llm_api_key_configured ?? false,
    cloudLlmOptIn: raw.cloud_llm_opt_in ?? false,
    llmSystemPrompt:
      raw.llm_system_prompt ??
      'You are an expert Kurdish linguist. Fix the phonetic transcription errors in the following text, preserving the exact meaning. Output ONLY the corrected text, no explanations.',
    llmModel: raw.llm_model ?? 'omniASR_LLM_7B_v2',
    externalAsrScriptPath: raw.external_asr_script_path ?? '',
    // Listening Jury
    juryCloudOptIn: raw.jury_cloud_opt_in ?? false,
    juryModel: raw.jury_model ?? 'gemini-2.5-pro',
    sourceReferenceModels: raw.source_reference_models ?? ['gemini-2.5-pro', 'gemini-2.5-flash'],
    jurySelfConsistencyN: raw.jury_self_consistency_n ?? 3,
    juryAutonomyLevel: (raw.jury_autonomy_level as AppSettings['juryAutonomyLevel']) ?? 'propose',
    juryT1Threshold: raw.jury_t1_threshold ?? 0.75,
  };
}

export function mapFrontendToBackend(ui: AppSettings, existing: BackendSettings): BackendSettings {
  return {
    ...existing,
    theme: themeToBackend(ui.theme),
    auto_normalize: ui.autoNormalize,
    verbalize_numbers: ui.verbalizeNumbers,
    auto_align: ui.autoAlign,
    export_format: exportFormatToBackend(ui.exportFormat),
    asr_model_size: asrModelToBackend(ui.asrModel),
    vad_threshold: ui.vadThreshold,
    min_segment_duration_ms: ui.minSegmentSec * 1000,
    max_segment_duration_ms: ui.maxSegmentSec * 1000,
    num_asr_threads: ui.numThreads,
    enable_gpu: ui.enableGpu,
    language: ui.language,
    assign_speaker_from_filename: ui.assignSpeakerFromFilename,
    enable_diarization: ui.enableDiarization,
    enable_denoising: ui.enableDenoising,
    max_speakers: ui.maxSpeakers,
    max_wer_threshold: ui.maxWerThreshold,
    max_cer_threshold: ui.maxCerThreshold,
    enforce_quality_gates: ui.enforceQualityGates,
    llm_mode: ui.llmMode,
    llm_endpoint: ui.llmEndpoint,
    llm_api_key: ui.llmApiKey,
    llm_api_key_configured: ui.llmApiKeyConfigured || ui.llmApiKey.length > 0,
    cloud_llm_opt_in: ui.cloudLlmOptIn,
    llm_system_prompt: ui.llmSystemPrompt,
    llm_model: ui.llmModel,
    external_asr_script_path: ui.externalAsrScriptPath,
    hf_train_ratio: ui.hfTrainRatio,
    hf_val_ratio: ui.hfValRatio,
    hf_test_ratio: ui.hfTestRatio,
    hf_split_seed: ui.hfSplitSeed,
    hf_speaker_disjoint: ui.hfSpeakerDisjoint,
    hf_license: ui.hfLicense,
    // Listening Jury
    jury_cloud_opt_in: ui.juryCloudOptIn,
    jury_model: ui.juryModel,
    source_reference_models: ui.sourceReferenceModels,
    jury_self_consistency_n: ui.jurySelfConsistencyN,
    jury_autonomy_level: ui.juryAutonomyLevel,
    jury_t1_threshold: ui.juryT1Threshold,
  };
}
