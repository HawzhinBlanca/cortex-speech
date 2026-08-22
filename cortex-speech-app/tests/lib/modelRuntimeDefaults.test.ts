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
    expect(settingsSource).toContain('pub fn load_production(');
    expect(settingsSource).toContain('settings.enforce_production_canon();');
    // The obsolete headless writer is a tombstone: no settings-dependent branch can reach a local
    // engine or open the production database under stale configuration.
    expect(batchProcessorSource).toContain('HARD STOP');
    expect(batchProcessorSource).not.toMatch(/Database|AsrPool|AppSettings|insert_segments/);
  });
});
