import { describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { defaultSettings } from '../../src/lib/stores/settingsStore';

describe('model runtime defaults', () => {
  it('uses only the fine-tuned OmniASR-7B champion as the app default', () => {
    const settingsSource = readFileSync(
      resolve(process.cwd(), 'src-tauri', 'src', 'settings.rs'),
      'utf-8',
    );
    const batchProcessorSource = readFileSync(
      resolve(process.cwd(), 'src-tauri', 'src', 'bin', 'batch_processor.rs'),
      'utf-8',
    );

    expect(defaultSettings.asrModel).toBe('wsl-7b');
    expect(settingsSource).toContain('asr_model_size: AsrModelSize::WSL7B');
    expect(settingsSource).toMatch(
      /fn default_multi_engine_hypotheses\(\) -> bool \{\s*false\s*\}/,
    );
    // The legacy offline helper hard-stops for WSL7B and preserves an explicitly selected local model;
    // AsrLoadConfig's library default can never silently choose 300M for it.
    expect(batchProcessorSource).toContain('model_size: settings.asr_model_size.clone()');
    expect(batchProcessorSource).not.toContain('AsrModelSize::CTC1B');
  });
});
