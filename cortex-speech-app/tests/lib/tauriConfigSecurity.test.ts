import { describe, expect, it } from 'vitest';
import { existsSync, readFileSync, statSync } from 'node:fs';
import { resolve } from 'node:path';
import { APP_CONFIG } from '../../src/lib/app.config';

describe('Tauri config security boundaries', () => {
  it('limits asset protocol reads to the media cache', () => {
    const configPath = resolve(process.cwd(), 'src-tauri', 'tauri.conf.json');
    const config = JSON.parse(readFileSync(configPath, 'utf-8')) as TauriConfig;

    const assetProtocol = config.app?.security?.assetProtocol;

    expect(assetProtocol?.enable).toBe(true);
    // Scope must point at the app's REAL data dir ($DATA/cortex-speech, per lib.rs get_app_data_dir)
    // and stay confined to media-cache. The old '$APPDATA/media-cache/**' resolved to
    // $APPDATA=app_data_dir (Roaming/<bundle-identifier>), a different folder, so every clip was
    // blocked ("Failed to load audio file", media errCode 4).
    expect(assetProtocol?.scope).toEqual(['$DATA/cortex-speech/media-cache/**']);
    // Still locked down: never a broad data-dir grant.
    for (const s of assetProtocol?.scope ?? []) {
      expect(s).toContain('media-cache');
      expect(s).not.toBe('$DATA/**');
      expect(s).not.toBe('$APPDATA/**');
    }
  });

  it('does not retain stale filesystem plugin configuration', () => {
    const configPath = resolve(process.cwd(), 'src-tauri', 'tauri.conf.json');
    const config = JSON.parse(readFileSync(configPath, 'utf-8')) as TauriConfig;

    expect(config.plugins?.fs).toBeUndefined();
  });

  it('does not expose direct filesystem plugin permissions to the webview', () => {
    const capabilityPath = resolve(process.cwd(), 'src-tauri', 'capabilities', 'default.json');
    const capability = JSON.parse(readFileSync(capabilityPath, 'utf-8')) as {
      permissions?: Array<string | { identifier?: string; allow?: unknown[] }>;
    };

    const permissions = capability.permissions ?? [];
    const permissionNames = permissions.map((permission) =>
      typeof permission === 'string' ? permission : permission.identifier ?? '',
    );

    expect(permissionNames.some((permission) => permission.startsWith('fs:'))).toBe(false);
    expect(JSON.stringify(permissions)).not.toContain('$APPDATA/**');
  });

  it('bundles required runtime model resources explicitly', () => {
    const configPath = resolve(process.cwd(), 'src-tauri', 'tauri.conf.json');
    const config = JSON.parse(readFileSync(configPath, 'utf-8')) as TauriConfig;
    const rustModelsPath = resolve(process.cwd(), 'src-tauri', 'src', 'models.rs');
    const rustModelsSource = readFileSync(rustModelsPath, 'utf-8');

    const requiredResources = [
      { path: 'models/silero_vad_v4.onnx', minBytes: 1_000_000 },
      { path: 'models/omniasr-ctc-300m/model.int8.onnx', minBytes: 50_000_000 },
      { path: 'models/omniasr-ctc-300m/tokens.txt', minBytes: 100 },
      { path: 'models/onnxruntime.dll/onnxruntime.dll', minBytes: 10_000_000 },
      { path: 'models/onnxruntime.dll/onnxruntime_providers_shared.dll', minBytes: 1_000 },
    ];
    // The embedded fine-tuned Kurdish model is USER-PROVIDED (not fetched from a public upstream
    // like the base models), so it must be DECLARED in the bundle but its (large, gitignored) file
    // is only size-checked when present.
    const finetunedPaths = ['models/finetuned-mms-ckb/model.onnx', 'models/finetuned-mms-ckb/vocab.json'];
    const resources = config.bundle?.resources ?? [];
    const requiredPaths = [...requiredResources.map((resource) => resource.path), ...finetunedPaths];

    expect(resources).toEqual(requiredPaths);
    expect(resources).not.toContain('models/*');
    expect(resources).not.toContain('models/**');
    expect(resources).toContain(APP_CONFIG.models.vadPath);
    expect(resources).toContain(APP_CONFIG.models.omniasrModel);
    expect(resources).toContain(APP_CONFIG.models.omniasrTokens);
    expect(rustModelsSource).toContain(
      `OMNIASR_CTC_300M_MODEL: &str = "${stripModelsPrefix(APP_CONFIG.models.omniasrModel)}"`,
    );
    expect(rustModelsSource).toContain(
      `OMNIASR_CTC_300M_TOKENS: &str = "${stripModelsPrefix(APP_CONFIG.models.omniasrTokens)}"`,
    );

    for (const resource of requiredResources) {
      const resourcePath = resolve(process.cwd(), 'src-tauri', resource.path);

      expect(existsSync(resourcePath)).toBe(true);
      expect(statSync(resourcePath).size).toBeGreaterThanOrEqual(resource.minBytes);
    }

    for (const p of finetunedPaths) {
      expect(resources).toContain(p);
      const fp = resolve(process.cwd(), 'src-tauri', p);
      if (existsSync(fp)) {
        expect(statSync(fp).size).toBeGreaterThan(0);
      }
    }
  });
});

function stripModelsPrefix(path: string): string {
  return path.replace(/^models\//, '');
}

type TauriConfig = {
  app?: { security?: { assetProtocol?: { enable?: boolean; scope?: string[] } } };
  bundle?: { resources?: string[] };
  plugins?: { fs?: unknown };
};
