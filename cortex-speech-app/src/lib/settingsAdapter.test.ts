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

  it('maps every closed theme, export, and LLM wire variant', () => {
    expect(mapBackendToFrontend(backend({ theme: 'System' })).theme).toBe('system');
    expect(mapBackendToFrontend(backend({ theme: 'Light' })).theme).toBe('light');
    expect(mapBackendToFrontend(backend({ export_format: 'Csv' })).exportFormat).toBe('csv');
    expect(mapBackendToFrontend(backend({ export_format: 'Jsonl' })).exportFormat).toBe('jsonl');
    expect(mapBackendToFrontend(backend({ export_format: 'Parquet' })).exportFormat).toBe(
      'parquet',
    );
    expect(mapBackendToFrontend(backend({ llm_mode: 'Gemini' })).llmMode).toBe('Gemini');
    expect(mapBackendToFrontend(backend({ llm_mode: 'Off' })).llmMode).toBe('None');
    expect(mapBackendToFrontend(backend({ llm_mode: 'Local' })).llmMode).toBe('Local');

    for (const [theme, expected] of [
      ['system', 'System'],
      ['light', 'Light'],
      ['dark', 'Dark'],
    ] as const) {
      expect(mapFrontendToBackend({ ...defaultSettings, theme }, backend()).theme).toBe(expected);
    }
    for (const [exportFormat, expected] of [
      ['csv', 'Csv'],
      ['jsonl', 'Jsonl'],
      ['parquet', 'Parquet'],
      ['json', 'Json'],
    ] as const) {
      expect(
        mapFrontendToBackend({ ...defaultSettings, exportFormat }, backend()).export_format,
      ).toBe(expected);
    }
  });

  it('applies every safe compatibility default when an older backend omits optional fields', () => {
    const legacy = backend({
      verbalize_numbers: undefined as unknown as boolean,
      enable_diarization: undefined as unknown as boolean,
      enable_denoising: undefined as unknown as boolean,
      autoplay_segments: undefined,
      max_speakers: undefined as unknown as number,
      assign_speaker_from_filename: undefined as unknown as boolean,
      max_wer_threshold: undefined as unknown as number,
      max_cer_threshold: undefined as unknown as number,
      enforce_quality_gates: undefined as unknown as boolean,
      hf_train_ratio: undefined as unknown as number,
      hf_val_ratio: undefined as unknown as number,
      hf_test_ratio: undefined as unknown as number,
      hf_split_seed: undefined as unknown as number,
      hf_speaker_disjoint: undefined as unknown as boolean,
      hf_license: undefined as unknown as string,
      llm_endpoint: undefined as unknown as string,
      llm_api_key_configured: undefined as unknown as boolean,
      cloud_llm_opt_in: undefined as unknown as boolean,
      llm_system_prompt: undefined as unknown as string,
      llm_model: undefined as unknown as string,
      external_asr_script_path: undefined as unknown as string,
      jury_cloud_opt_in: undefined,
      jury_self_consistency_n: undefined,
      jury_autonomy_level: undefined,
      jury_t1_threshold: undefined,
    });

    expect(mapBackendToFrontend(legacy)).toMatchObject({
      verbalizeNumbers: true,
      enableDiarization: true,
      enableDenoising: false,
      autoplaySegments: false,
      maxSpeakers: 8,
      assignSpeakerFromFilename: true,
      maxWerThreshold: 0.35,
      maxCerThreshold: 0.2,
      enforceQualityGates: false,
      hfTrainRatio: 0.8,
      hfValRatio: 0.1,
      hfTestRatio: 0.1,
      hfSplitSeed: 42,
      hfSpeakerDisjoint: true,
      hfLicense: 'mit',
      llmEndpoint: 'http://127.0.0.1:11434/v1/chat/completions',
      llmApiKeyConfigured: false,
      cloudLlmOptIn: false,
      llmModel: 'heretic-final:latest',
      externalAsrScriptPath: '',
      juryCloudOptIn: false,
      jurySelfConsistencyN: 3,
      juryAutonomyLevel: 'propose',
      juryT1Threshold: 0.75,
    });
  });

  it('falls back to durable numeric values instead of serializing NaN or infinity', () => {
    const existing = backend({
      vad_threshold: 0.41,
      min_segment_duration_ms: 2_500,
      max_segment_duration_ms: 12_500,
      num_asr_threads: 7,
      max_speakers: 6,
      max_wer_threshold: 0.31,
      max_cer_threshold: 0.19,
      hf_train_ratio: 0.7,
      hf_val_ratio: 0.2,
      hf_test_ratio: 0.1,
      hf_split_seed: 9,
      jury_self_consistency_n: undefined,
      jury_t1_threshold: undefined,
    });
    const invalid = {
      ...defaultSettings,
      vadThreshold: Number.NaN,
      minSegmentSec: Number.NaN,
      maxSegmentSec: Number.POSITIVE_INFINITY,
      numThreads: Number.NaN,
      maxSpeakers: Number.NaN,
      maxWerThreshold: Number.NaN,
      maxCerThreshold: Number.NaN,
      hfTrainRatio: Number.NaN,
      hfValRatio: Number.NaN,
      hfTestRatio: Number.NaN,
      hfSplitSeed: Number.NaN,
      jurySelfConsistencyN: Number.NaN,
      juryT1Threshold: Number.NaN,
      llmApiKeyConfigured: false,
      llmApiKey: 'explicit-key',
    };

    expect(mapFrontendToBackend(invalid, existing)).toMatchObject({
      vad_threshold: 0.41,
      min_segment_duration_ms: 2_500,
      max_segment_duration_ms: 12_500,
      num_asr_threads: 7,
      max_speakers: 6,
      max_wer_threshold: 0.31,
      max_cer_threshold: 0.19,
      hf_train_ratio: 0.7,
      hf_val_ratio: 0.2,
      hf_test_ratio: 0.1,
      hf_split_seed: 9,
      jury_self_consistency_n: 3,
      jury_t1_threshold: 0.75,
      llm_api_key_configured: true,
    });
  });
});
