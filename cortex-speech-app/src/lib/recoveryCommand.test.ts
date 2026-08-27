import { invoke } from '@tauri-apps/api/core';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import {
  acknowledgeQuarantine,
  dbBackup,
  dbRestore,
  dbVacuum,
  getQuarantineNotice,
  listDbSnapshots,
  restoreDbFromSnapshot,
} from './commands';

const invokeMock = vi.mocked(invoke);

describe('generated recovery command boundary', () => {
  beforeEach(() => invokeMock.mockReset());

  it('preserves exact arguments and typed public results for every recovery command', async () => {
    const backup = { integrityOk: true, segmentCount: 42 };
    const notice = {
      quarantinedFileCount: 2,
      snapshotCount: 1,
      newestSnapshotSegments: 42,
    };
    const snapshots = [
      {
        name: 'snapshot_1787000000',
        timestamp: 1787000000,
        dbSizeBytes: 8192,
        segmentCount: 42,
      },
    ];
    invokeMock
      .mockResolvedValueOnce(backup)
      .mockResolvedValueOnce(notice)
      .mockResolvedValueOnce(snapshots)
      .mockResolvedValueOnce(undefined)
      .mockResolvedValueOnce(2)
      .mockResolvedValueOnce(undefined)
      .mockResolvedValueOnce(undefined);

    await expect(dbBackup('D:/proof/library.db')).resolves.toEqual(backup);
    await expect(getQuarantineNotice()).resolves.toEqual(notice);
    await expect(listDbSnapshots()).resolves.toEqual(snapshots);
    await expect(restoreDbFromSnapshot('snapshot_1787000000')).resolves.toBeUndefined();
    await expect(acknowledgeQuarantine()).resolves.toBe(2);
    await expect(dbRestore('D:/proof/library.db')).resolves.toBeUndefined();
    await expect(dbVacuum()).resolves.toBeUndefined();

    expect(invokeMock.mock.calls).toEqual([
      ['db_backup', { dest: 'D:/proof/library.db' }],
      ['get_quarantine_notice'],
      ['list_db_snapshots'],
      ['restore_db_from_snapshot', { name: 'snapshot_1787000000' }],
      ['acknowledge_quarantine'],
      ['db_restore', { src: 'D:/proof/library.db' }],
      ['db_vacuum'],
    ]);
  });

  it('preserves structured backend refusals without stringifying private data', async () => {
    const refusal = {
      schema: 1,
      code: 'SNAPSHOT_RESTORE_FAILED',
      message: 'The selected snapshot could not be safely restored.',
      retryable: false,
      suggestedAction: 'openHealth',
      operationId: null,
      details: {},
    } as const;
    invokeMock.mockRejectedValueOnce(refusal);

    await expect(restoreDbFromSnapshot('snapshot_1787000000')).rejects.toEqual(refusal);
  });

  it('keeps transport errors distinguishable from typed backend refusals', async () => {
    const transport = new Error('desktop transport unavailable');
    invokeMock.mockRejectedValueOnce(transport);

    await expect(dbVacuum()).rejects.toBe(transport);
  });
});
