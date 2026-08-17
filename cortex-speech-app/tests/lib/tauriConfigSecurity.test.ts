import { describe, expect, it } from 'vitest';
import { existsSync, readFileSync, statSync } from 'node:fs';
import { resolve } from 'node:path';

// Only `models` is consumed (this test); dead fields removed in the 2026-07-15 audit, and the
// standalone src/lib/app.config.ts file itself removed (zero production importers) in round 2.
const APP_CONFIG = {
  models: {
    omniasrModel: 'models/omniasr-ctc-300m/model.int8.onnx',
    omniasrTokens: 'models/omniasr-ctc-300m/tokens.txt',
    vadPath: 'models/silero_vad_v4.onnx',
  },
};

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
    const finetunedPaths = ['models/finetuned-mms-ckb/model.onnx', 'models/finetuned-mms-ckb/vocab.json'];
    const basePaths = requiredResources.map((resource) => resource.path);
    const resources = config.bundle?.resources ?? [];

    // The DEFAULT bundle lists only the base models that scripts/fetch_models.py can download +
    // SHA-256-verify, so hosted CI (ci.yml) and hosted release (release.yml) can build from a fresh
    // checkout. The USER-PROVIDED fine-tuned model is NOT publicly fetchable, so it is intentionally
    // absent here and added only via the local-build override (asserted below). tauri.windows.conf.json
    // must stay in lock-step with the default, or a Windows build would demand a resource the default
    // omits.
    expect(resources).toEqual(basePaths);
    const windowsConfig = JSON.parse(
      readFileSync(resolve(process.cwd(), 'src-tauri', 'tauri.windows.conf.json'), 'utf-8'),
    ) as TauriConfig;
    expect(windowsConfig.bundle?.resources ?? []).toEqual(basePaths);
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

    // The fine-tuned model is bundled ONLY through the explicit local-build override
    // (tauri.finetuned.conf.json, passed via `tauri build --config`). Assert that override still
    // DECLARES the full set (base + fine-tuned) so the opt-in engine can never be silently dropped from
    // a real installer, and stays a strict superset of the default.
    const finetunedConfig = JSON.parse(
      readFileSync(resolve(process.cwd(), 'src-tauri', 'tauri.finetuned.conf.json'), 'utf-8'),
    ) as TauriConfig;
    const finetunedResources = finetunedConfig.bundle?.resources ?? [];
    expect(finetunedResources).toEqual([...basePaths, ...finetunedPaths]);

    // The base ONNX models (silero/omniasr/onnxruntime DLLs) are large and gitignored; hosted CI
    // runners deliberately don't carry them until `npm run fetch-models` runs — same design as the
    // e2e:real gate, which only runs where the models exist (see ci.yml). The bundle DECLARATION is
    // asserted unconditionally above (a dropped model fails `toEqual`). This loop additionally
    // size-checks each file WHEN PRESENT, so the truncated/corrupt-file guard still fires on any
    // model-bearing machine (local dev / the release runner) while a bare checkout skips the physical
    // probe instead of asserting a gitignored blob into existence.
    for (const resource of requiredResources) {
      const resourcePath = resolve(process.cwd(), 'src-tauri', resource.path);

      if (existsSync(resourcePath)) {
        expect(statSync(resourcePath).size).toBeGreaterThanOrEqual(resource.minBytes);
      }
    }

    for (const p of finetunedPaths) {
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
