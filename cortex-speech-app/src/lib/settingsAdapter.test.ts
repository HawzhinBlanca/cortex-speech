import { describe, expect, it } from 'vitest';
import {
  mapBackendToFrontend,
  mapFrontendToBackend,
  type BackendSettings,
} from './settingsAdapter';
import { ADVISORY_CLOUD_MODEL, defaultSettings, type AppSettings } from './stores/settingsStore';

function backend(overrides: Partial<BackendSettings> = {}): BackendSettings {
  return {
    model_dir: 'models',
    output_dir: 'exports',
    asr_model_size: 'WSL7B',
    use_finetuned_asr: false,
    vad_threshold: 0.5,
    min_segment_duration_ms: 3_000,
    max_segment_duration_ms: 15_000,
    num_asr_threads: 4,
    enable_gpu: true,
    language: 'ckb',
    export_format: 'Json',
    auto_normalize: true,
    verbalize_numbers: true,
    auto_align: false,
    assign_speaker_from_filename: true,
    enable_diarization: true,
    enable_denoising: false,
    autoplay_segments: false,
    max_speakers: 8,
    max_wer_threshold: 0.35,
    max_cer_threshold: 0.2,
    enforce_quality_gates: false,
    theme: 'Dark',
    llm_mode: 'None',
    llm_endpoint: 'http://127.0.0.1:11434/v1/chat/completions',
    llm_api_key: '',
    llm_api_key_configured: false,
    cloud_llm_opt_in: false,
    llm_system_prompt: 'prompt',
    llm_model: 'owner-local-model:latest',
    external_asr_script_path: '',
    hf_train_ratio: 0.8,
    hf_val_ratio: 0.1,
    hf_test_ratio: 0.1,
    hf_split_seed: 42,
    hf_speaker_disjoint: true,
    hf_license: 'mit',
    jury_cloud_opt_in: false,
    jury_model: ADVISORY_CLOUD_MODEL,
    jury_provider: 'gemini',
    source_reference_models: [ADVISORY_CLOUD_MODEL],
    jury_self_consistency_n: 3,
    jury_autonomy_level: 'propose',
    jury_t1_threshold: 0.75,
    ...overrides,
  };
}

describe('settings cloud-model canon', () => {
  it('never exposes stale persisted Flash or arbitrary model ids to the UI', () => {
    const ui = mapBackendToFrontend(
      backend({
        llm_mode: 'Gemini',
        llm_model: 'gemini-2.5-flash',
        jury_model: 'other/cloud-judge',
        source_reference_models: ['gemini-2.5-flash', 'other/model'],
      }),
    );

    expect(ui.llmModel).toBe(ADVISORY_CLOUD_MODEL);
    expect(ui.juryModel).toBe(ADVISORY_CLOUD_MODEL);
    expect(ui.sourceReferenceModels).toEqual([ADVISORY_CLOUD_MODEL]);
  });

  it('clamps an untrusted renderer payload before it crosses the IPC boundary', () => {
    const unsafe = {
      ...defaultSettings,
      llmMode: 'Gemini',
      llmModel: 'gemini-2.5-flash',
      juryModel: 'other/cloud-judge',
      sourceReferenceModels: ['gemini-2.5-flash'],
    } as unknown as AppSettings;

    const persisted = mapFrontendToBackend(unsafe, backend());
    expect(persisted.llm_model).toBe(ADVISORY_CLOUD_MODEL);
    expect(persisted.jury_model).toBe(ADVISORY_CLOUD_MODEL);
    expect(persisted.source_reference_models).toEqual([ADVISORY_CLOUD_MODEL]);
  });

  it('preserves an explicit local/offline LLM model id', () => {
    const local: AppSettings = {
      ...defaultSettings,
      llmMode: 'Local',
      llmModel: 'owner-local-model:latest',
    };

    expect(mapFrontendToBackend(local, backend()).llm_model).toBe('owner-local-model:latest');
    expect(mapBackendToFrontend(backend()).llmModel).toBe('owner-local-model:latest');
  });
});
