import { describe, it, expect } from 'vitest';
import { defaultSettings } from '../../src/lib/stores/settingsStore';
import {
  mapBackendToFrontend,
  mapFrontendToBackend,
  type BackendSettings,
} from '../../src/lib/settingsAdapter';

const sampleBackend: BackendSettings = {
  model_dir: 'models',
  output_dir: 'exports',
  asr_provider: 'SherpaOnnxCtc',
  asr_model_size: 'CTC300M',
  vad_threshold: 0.6,
  min_segment_duration_ms: 4000,
  max_segment_duration_ms: 20000,
  num_asr_threads: 8,
  enable_gpu: false,
  language: 'ckb',
  export_format: 'Jsonl',
  auto_normalize: true,
  auto_align: false,
  assign_speaker_from_filename: true,
  enable_diarization: true,
  enable_denoising: false,
  max_speakers: 8,
  max_wer_threshold: 0.35,
  max_cer_threshold: 0.20,
  enforce_quality_gates: false,
  theme: 'Dark',
  verbalize_numbers: true,
  llm_mode: 'Off',
  llm_endpoint: '',
  llm_api_key: '',
  llm_api_key_configured: false,
  cloud_llm_opt_in: false,
  llm_system_prompt: '',
  llm_model: '',
  external_asr_script_path: '',
  hf_train_ratio: 0.8,
  hf_val_ratio: 0.1,
  hf_test_ratio: 0.1,
  hf_split_seed: 42,
  hf_speaker_disjoint: true,
  hf_license: 'mit',
};

describe('settingsAdapter', () => {
  it('maps backend settings to frontend shape', () => {
    const ui = mapBackendToFrontend(sampleBackend);
    expect(ui.vadThreshold).toBe(0.6);
    expect(ui.minSegmentSec).toBe(4);
    expect(ui.maxSegmentSec).toBe(20);
    expect(ui.numThreads).toBe(8);
    expect(ui.exportFormat).toBe('jsonl');
    expect(ui.asrModel).toBe('ctc-300m');
    expect(ui.theme).toBe('dark');
    expect(ui.llmMode).toBe('None');
    expect(ui.sourceReferenceModels).toEqual(['gemini-2.5-pro', 'gemini-2.5-flash']);
  });

  it('round-trips frontend settings through backend', () => {
    const ui = {
      ...defaultSettings,
      vadThreshold: 0.35,
      minSegmentSec: 5,
      maxSegmentSec: 25,
      numThreads: 12,
      exportFormat: 'csv' as const,
      asrModel: 'ctc-1b' as const,
      theme: 'light' as const,
      sourceReferenceModels: ['gemini-2.5-pro', 'gemini-2.5-flash'],
    };
    const backend = mapFrontendToBackend(ui, sampleBackend);
    expect(backend.vad_threshold).toBe(0.35);
    expect(backend.min_segment_duration_ms).toBe(5000);
    expect(backend.max_segment_duration_ms).toBe(25000);
    expect(backend.num_asr_threads).toBe(12);
    expect(backend.export_format).toBe('Csv');
    expect(backend.asr_model_size).toBe('CTC1B');
    expect(backend.theme).toBe('Light');
    expect(backend.source_reference_models).toEqual(['gemini-2.5-pro', 'gemini-2.5-flash']);
    expect(backend.model_dir).toBe('models');
    expect(mapBackendToFrontend(backend)).toEqual(ui);
  });

  it('normalizes unknown backend LLM modes to local-only mode', () => {
    expect(mapBackendToFrontend({ ...sampleBackend, llm_mode: 'UnexpectedMode' }).llmMode).toBe('Local');
  });

  // The 'Autoplay Segments' toggle used to be silently dropped on save (mapFrontendToBackend never
  // wrote it) and force-reset to false on load (mapBackendToFrontend hardcoded false), so it never
  // survived a restart. Pin the full round-trip in both directions.
  it('persists the autoplaySegments toggle across save and reload', () => {
    const ui = { ...defaultSettings, autoplaySegments: true };
    const backend = mapFrontendToBackend(ui, sampleBackend);
    expect(backend.autoplay_segments).toBe(true);
    expect(mapBackendToFrontend(backend).autoplaySegments).toBe(true);
    // A stored value is honoured, not overridden.
    expect(mapBackendToFrontend({ ...sampleBackend, autoplay_segments: true }).autoplaySegments).toBe(true);
    expect(mapBackendToFrontend({ ...sampleBackend, autoplay_segments: false }).autoplaySegments).toBe(false);
  });
// juryProvider (T2 judge transport) must survive save -> reload, default safely to 'gemini' on old
  // settings files, and reject junk values (never route cloud audio on a typo'd provider).
  it('round-trips juryProvider and defaults unknown/missing values to gemini', () => {
    const ui = { ...defaultSettings, juryProvider: 'openrouter' as const };
    const backend = mapFrontendToBackend(ui, sampleBackend);
    expect(backend.jury_provider).toBe('openrouter');
    expect(mapBackendToFrontend(backend).juryProvider).toBe('openrouter');
    // Missing (pre-upgrade settings.json) and junk values both resolve to the safe default.
    expect(mapBackendToFrontend(sampleBackend).juryProvider).toBe('gemini');
    expect(mapBackendToFrontend({ ...sampleBackend, jury_provider: 'qwen' }).juryProvider).toBe('gemini');
  });
});
