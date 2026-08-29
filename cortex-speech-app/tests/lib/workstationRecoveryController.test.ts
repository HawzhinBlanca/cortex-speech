import { get } from 'svelte/store';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const apiMocks = vi.hoisted(() => ({
  getInterruptedImport: vi.fn(),
  resumeInterruptedImport: vi.fn(),
  getImportRunStatus: vi.fn(),
  discardInterruptedImport: vi.fn(),
  acknowledgeQuarantine: vi.fn(),
  getActiveBatchRun: vi.fn(),
}));

vi.mock('../../src/lib/commands', () => apiMocks);

import { createWorkstationRecoveryController } from '../../src/lib/workstationRecoveryController.svelte';
import { locale } from '../../src/lib/i18n';
import { notifications } from '../../src/lib/stores/notificationStore';
import { isProcessing } from '../../src/lib/stores/uiStore';

beforeEach(() => {
  locale.set('en');
  notifications.clear();
  isProcessing.set(false);
  apiMocks.getInterruptedImport.mockReset().mockResolvedValue({
    id: 'job-owner',
    totalFiles: 2,
    completedCount: 1,
    createdAt: '2026-08-28T00:00:00Z',
  });
  apiMocks.resumeInterruptedImport.mockReset().mockRejectedValue({
    schema: 1,
    code: 'IMPORT_RUN_REJECTED',
    message: 'resume refused',
    retryable: true,
    suggestedAction: 'retry',
  });
  apiMocks.getImportRunStatus.mockReset().mockImplementation(async (runId: string) => ({
    runId,
    status: 'rejected',
  }));
  apiMocks.discardInterruptedImport.mockReset();
  apiMocks.acknowledgeQuarantine.mockReset();
  apiMocks.getActiveBatchRun.mockReset().mockResolvedValue(null);
});

afterEach(() => {
  notifications.clear();
  vi.restoreAllMocks();
});

describe('workstation import recovery visibility', () => {
  it('shows a definite rejected resume instead of mistaking two cleared authorities for ambiguity', async () => {
    const controller = createWorkstationRecoveryController({
      requireDesktopRuntime: () => true,
      loadSegments: vi.fn(async () => {}),
      loadLatestAgentHistory: vi.fn(async () => {}),
      clearAgentEvidence: vi.fn(),
      setSegmentsLoading: vi.fn(),
    });

    await controller.importRecovery.reconcile();
    expect(controller.interruptedImport?.id).toBe('job-owner');
    await controller.importRecovery.resume();

    expect(
      get(notifications).some(
        (notice) => notice.type === 'error' && notice.message.includes('resume'),
      ),
    ).toBe(true);
    expect(controller.importCoordinator.shouldSuppressResumeFailure()).toBe(false);
    controller.importCoordinator.destroy();
  });
});
