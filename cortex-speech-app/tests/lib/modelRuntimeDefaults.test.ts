import { describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { defaultSettings } from '../../src/lib/stores/settingsStore';

describe('model runtime defaults', () => {
  it('forces the OmniASR-7B Champion as the app default, keeps bundled CTC300M for download/batch', () => {
    const settingsSource = readFileSync(resolve(process.cwd(), 'src-tauri', 'src', 'settings.rs'), 'utf-8');
    const batchProcessorSource = readFileSync(
      resolve(process.cwd(), 'src-tauri', 'src', 'bin', 'batch_processor.rs'),
      'utf-8',
    );
    const downloadModelSource = readFileSync(
      resolve(process.cwd(), 'src-tauri', 'src', 'bin', 'download_model.rs'),
      'utf-8',
    );

    // The owner's main model is the fine-tuned OmniASR-7B Champion (WSL GPU path), so it is the
    // user-facing default in BOTH the Rust AppSettings and the frontend store (kept in sync).
    expect(defaultSettings.asrModel).toBe('wsl-7b');
    expect(settingsSource).toContain('asr_model_size: AsrModelSize::WSL7B');
    // ...but the BUNDLED/downloadable runtime model stays the base CTC-300M ONNX (the 7B is an external
    // WSL/GPU engine, not a bundled ONNX), so the offline batch path + the model downloader still
    // default to CTC300M rather than chasing a non-bundled 1B/7B.
    expect(batchProcessorSource).toContain('..AsrLoadConfig::default()');
    expect(batchProcessorSource).not.toContain('AsrModelSize::CTC1B');
    expect(downloadModelSource).toContain('download_omniasr(AsrModelSize::CTC300M');
    expect(downloadModelSource).not.toContain('download_omniasr(AsrModelSize::CTC1B');
  });
});
