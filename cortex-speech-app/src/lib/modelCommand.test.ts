import { invoke } from '@tauri-apps/api/core';
import { describe, expect, expectTypeOf, it, vi } from 'vitest';
import {
  bootstrapLegacyChampion,
  getChampionEngineStatus,
  importModelCheckpoint,
  importModelDeployment,
  listModelVersions,
  modelsDownloadAll,
  modelsStatus,
  startChampionEngine,
} from './commands';
import type {
  EngineStatusV1,
  ModelDownloadSummaryV1,
  ModelStatusEntryV1,
  ModelVersionSummaryV1,
} from './generated/ipc';

const invokeMock = vi.mocked(invoke);

describe('generated model-management IPC contracts', () => {
  it('preserves generated result shapes and exact command names', async () => {
    invokeMock.mockReset();
    const status: ModelStatusEntryV1[] = [
      {
        name: 'Silero VAD v4',
        filename: 'silero_vad_v4.onnx',
        downloaded: true,
        exists: true,
        sizeBytes: 2_000_000,
        minSizeBytes: 1_000_000,
        version: '4.0',
        source: 'bundled',
        downloadable: true,
      },
    ];
    const download: ModelDownloadSummaryV1 = { downloaded: 1, failed: 0, total: 1, skipped: 0 };
    const engine: EngineStatusV1 = {
      ready: true,
      port: 8799,
      identityMatches: true,
      expectedModelVersionId: 'champion-1',
      expectedDeploymentSha256: 'a'.repeat(64),
      loadedModelVersionId: 'champion-1',
      loadedDeploymentSha256: 'a'.repeat(64),
      reason: null,
    };
    const versions: ModelVersionSummaryV1[] = [
      {
        id: 'champion-1',
        family: 'omniasr-7b',
        modelCardName: 'Pinned champion',
        checkpointSha256: 'b'.repeat(64),
        source: 'owner-finetune',
        license: 'Apache-2.0',
        status: 'champion',
      },
    ];
    invokeMock
      .mockResolvedValueOnce(status)
      .mockResolvedValueOnce(download)
      .mockResolvedValueOnce(engine)
      .mockResolvedValueOnce(undefined)
      .mockResolvedValueOnce(versions);

    const statusResult = modelsStatus();
    expectTypeOf(statusResult).toEqualTypeOf<Promise<ModelStatusEntryV1[]>>();
    await expect(statusResult).resolves.toEqual(status);
    await expect(modelsDownloadAll()).resolves.toEqual(download);
    await expect(getChampionEngineStatus()).resolves.toEqual(engine);
    await expect(startChampionEngine()).resolves.toBeUndefined();
    await expect(listModelVersions()).resolves.toEqual(versions);

    expect(invokeMock.mock.calls).toEqual([
      ['models_status'],
      ['models_download_all'],
      ['get_champion_engine_status'],
      ['start_champion_engine'],
      ['list_model_versions'],
    ]);
  });

  it('propagates structured model failures without stringifying private details', async () => {
    invokeMock.mockReset();
    const refusal = {
      schema: 1,
      code: 'CHAMPION_REGISTRY_UNAVAILABLE',
      message: 'Champion identity could not be read. Open Health for recovery options.',
      retryable: false,
      suggestedAction: 'openHealth',
      operationId: null,
      details: {},
    };
    invokeMock.mockRejectedValueOnce(refusal);

    await expect(getChampionEngineStatus()).rejects.toEqual(refusal);
  });

  it('binds every registry-write argument through the generated contract', async () => {
    invokeMock.mockReset();
    const deploymentSha = 'c'.repeat(64);
    const candidate: ModelVersionSummaryV1 = {
      id: 'candidate-2',
      family: 'omniasr-7b',
      modelCardName: null,
      checkpointSha256: 'd'.repeat(64),
      source: 'owner-finetune',
      license: 'Apache-2.0',
      status: 'candidate',
    };
    const champion: ModelVersionSummaryV1 = {
      ...candidate,
      id: 'champion-legacy',
      status: 'champion',
    };
    invokeMock
      .mockResolvedValueOnce('candidate-2')
      .mockResolvedValueOnce(candidate)
      .mockResolvedValueOnce(champion);

    await expect(
      importModelCheckpoint({
        id: 'candidate-2',
        checkpointPath: 'D:/private/candidate.onnx',
        source: 'owner-finetune',
        license: 'Apache-2.0',
      }),
    ).resolves.toBe('candidate-2');
    await expect(
      importModelDeployment({
        manifestPath: 'D:/private/deployment.json',
        expectedDeploymentSha256: deploymentSha,
        expectedModelId: 'candidate-2',
        source: 'owner-finetune',
        license: 'Apache-2.0',
      }),
    ).resolves.toEqual(candidate);
    await expect(
      bootstrapLegacyChampion({
        manifestPath: '/srv/cortex/deployment.json',
        expectedDeploymentSha256: deploymentSha,
        expectedModelId: 'champion-legacy',
        license: 'Apache-2.0',
      }),
    ).resolves.toEqual(champion);

    expect(invokeMock.mock.calls).toEqual([
      [
        'import_model_checkpoint',
        {
          id: 'candidate-2',
          checkpointPath: 'D:/private/candidate.onnx',
          source: 'owner-finetune',
          license: 'Apache-2.0',
          modelCardName: null,
        },
      ],
      [
        'import_model_deployment',
        {
          manifestPath: 'D:/private/deployment.json',
          expectedDeploymentSha256: deploymentSha,
          expectedModelId: 'candidate-2',
          source: 'owner-finetune',
          license: 'Apache-2.0',
        },
      ],
      [
        'bootstrap_legacy_champion',
        {
          manifestPath: '/srv/cortex/deployment.json',
          expectedDeploymentSha256: deploymentSha,
          expectedModelId: 'champion-legacy',
          license: 'Apache-2.0',
        },
      ],
    ]);
  });
});
