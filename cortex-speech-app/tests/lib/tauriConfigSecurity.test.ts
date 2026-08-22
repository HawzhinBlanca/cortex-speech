import { describe, expect, it } from 'vitest';
import { existsSync, readFileSync, statSync } from 'node:fs';
import { resolve } from 'node:path';

// Only `models` is consumed (this test); dead fields removed in the 2026-07-15 audit, and the
// standalone src/lib/app.config.ts file itself removed (zero production importers) in round 2.
const APP_CONFIG = {
  models: {
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

  it('bundles runtime support but no optional ASR engine', () => {
    const configPath = resolve(process.cwd(), 'src-tauri', 'tauri.conf.json');
    const config = JSON.parse(readFileSync(configPath, 'utf-8')) as TauriConfig;

    const requiredResources = [
      { path: 'models/silero_vad_v4.onnx', minBytes: 1_000_000 },
      { path: 'models/onnxruntime.dll/onnxruntime.dll', minBytes: 10_000_000 },
      { path: 'models/onnxruntime.dll/onnxruntime_providers_shared.dll', minBytes: 1_000 },
    ];
    // The champion's WSL server/client scripts ship BESIDE the exe so an installed build satisfies
    // `SERVER_SCRIPT_RELATIVE_TO_EXE[0]` in engine_runtime.rs without any environment variable — the
    // outage that fallback exists for. Unlike the models these are repo-tracked, so they never break
    // the fresh-checkout build this list is otherwise careful about; that is asserted below.
    const scriptPaths = ['../scripts/cortex_7b_server.py', '../scripts/cortex_7b_client.py'];
    const basePaths = requiredResources.map((resource) => resource.path);
    const resources = config.bundle?.resources ?? [];

    // Standard builds carry VAD/runtime support and the WSL7B client/server scripts, never a smaller
    // ASR model. tauri.windows.conf.json must stay byte-for-byte aligned at the resource-list level.
    expect(resources).toEqual([...basePaths, ...scriptPaths]);
    const windowsConfig = JSON.parse(
      readFileSync(resolve(process.cwd(), 'src-tauri', 'tauri.windows.conf.json'), 'utf-8'),
    ) as TauriConfig;
    expect(windowsConfig.bundle?.resources ?? []).toEqual([...basePaths, ...scriptPaths]);
    expect(resources).not.toContain('models/*');
    expect(resources).not.toContain('models/**');
    expect(resources).toContain(APP_CONFIG.models.vadPath);
    const serializedResources = JSON.stringify(resources).toLowerCase();
    for (const forbidden of ['omniasr-ctc-300m', 'omniasr-ctc-1b', 'finetuned-mms', 'scribe', 'elevenlabs']) {
      expect(serializedResources).not.toContain(forbidden);
    }
    // MMS remains available only through a deliberately named diagnostic override; standard CI,
    // release and verify commands never pass it. The override itself must not smuggle in 300M/1B or
    // any cloud model.
    const diagnosticPath = resolve(process.cwd(), 'src-tauri', 'tauri.finetuned.conf.json');
    const diagnosticSource = readFileSync(diagnosticPath, 'utf-8');
    const diagnosticConfig = JSON.parse(diagnosticSource) as TauriConfig;
    const mmsPaths = ['models/finetuned-mms-ckb/model.onnx', 'models/finetuned-mms-ckb/vocab.json'];
    expect(diagnosticSource).toContain('EXPLICIT DIAGNOSTIC-ONLY');
    expect(diagnosticConfig.bundle?.resources ?? []).toEqual([...basePaths, ...mmsPaths, ...scriptPaths]);
    for (const forbidden of ['omniasr-ctc-300m', 'omniasr-ctc-1b', 'scribe', 'elevenlabs']) {
      expect(diagnosticSource.toLowerCase()).not.toContain(forbidden);
    }

    // STRICTER than before, not looser: every NON-model bundled resource must actually exist in the
    // checkout. The models are gitignored and only size-checked when present, but a script declared
    // in the bundle and missing from git would fail the hosted build with nothing to download —
    // exactly the failure this list's design note warns about.
    for (const script of scriptPaths) {
      expect(existsSync(resolve(process.cwd(), 'src-tauri', script))).toBe(true);
    }

    // Runtime support assets are large and gitignored; hosted CI
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

  });
});

type TauriConfig = {
  app?: { security?: { assetProtocol?: { enable?: boolean; scope?: string[] } } };
  bundle?: { resources?: string[] };
  plugins?: { fs?: unknown };
};
