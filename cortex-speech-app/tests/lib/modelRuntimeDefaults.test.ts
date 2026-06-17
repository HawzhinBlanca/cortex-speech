import { describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { defaultSettings } from '../../src/lib/stores/settingsStore';

describe('model runtime defaults', () => {
  it('keeps bundled CTC300M as the app and helper default', () => {
    const settingsSource = readFileSync(resolve(process.cwd(), 'src-tauri', 'src', 'settings.rs'), 'utf-8');
    const batchProcessorSource = readFileSync(
      resolve(process.cwd(), 'src-tauri', 'src', 'bin', 'batch_processor.rs'),
      'utf-8',
    );
    const downloadModelSource = readFileSync(
      resolve(process.cwd(), 'src-tauri', 'src', 'bin', 'download_model.rs'),
      'utf-8',
    );

    expect(defaultSettings.asrModel).toBe('ctc-300m');
    expect(settingsSource).toContain('asr_model_size: AsrModelSize::CTC300M');
    expect(batchProcessorSource).toContain('..AsrLoadConfig::default()');
    expect(batchProcessorSource).not.toContain('AsrModelSize::CTC1B');
    expect(downloadModelSource).toContain('download_omniasr(AsrModelSize::CTC300M');
    expect(downloadModelSource).not.toContain('download_omniasr(AsrModelSize::CTC1B');
  });
});
