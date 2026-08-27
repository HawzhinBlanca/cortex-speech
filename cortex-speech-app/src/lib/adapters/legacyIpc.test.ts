import { invoke } from '@tauri-apps/api/core';
import { describe, expect, expectTypeOf, it, vi } from 'vitest';
import { invokeCritical, invokeLegacy, type LegacyIpcCommand } from './legacyIpc';

const invokeMock = vi.mocked(invoke);

describe('handwritten IPC containment', () => {
  it('refuses an unregistered runtime command before it reaches Tauri', async () => {
    invokeMock.mockReset();

    await expect(
      invokeLegacy<unknown>('not_a_registered_command' as LegacyIpcCommand),
    ).rejects.toThrow('Refusing unregistered');
    expect(invokeMock).not.toHaveBeenCalled();
  });

  it('preserves exact critical arguments while inferring the registered result type', async () => {
    invokeMock.mockReset();
    invokeMock.mockResolvedValueOnce(undefined);

    const result = invokeCritical('export_dataset', {
      path: 'D:/proof/library.jsonl',
      format: 'jsonl',
    });
    expectTypeOf(result).toEqualTypeOf<Promise<void>>();
    await expect(result).resolves.toBeUndefined();
    expect(invokeMock).toHaveBeenCalledWith('export_dataset', {
      path: 'D:/proof/library.jsonl',
      format: 'jsonl',
    });
  });
});

// Compile-time proof: generated commands cannot return to the handwritten boundary, critical
// command arguments cannot be omitted, and arbitrary runtime command strings cannot enter it.
const compileTimeContractProof = (): void => {
  // @ts-expect-error generated playback is intentionally absent from the handwritten inventory
  void invokeLegacy<unknown>('begin_desktop_playback_session_v1');
  // @ts-expect-error the complete desktop-history domain is generated, not handwritten
  void invokeCritical('undo');
  // @ts-expect-error generated history queries cannot regress into the legacy inventory
  void invokeLegacy<unknown>('can_redo');
  // @ts-expect-error transcript utilities are generated, not handwritten
  void invokeLegacy<unknown>('compute_diff');
  // @ts-expect-error generated normalization cannot regress into the legacy inventory
  void invokeLegacy<unknown>('normalize_text');
  // @ts-expect-error health and build identity are generated, not handwritten
  void invokeLegacy<unknown>('app_health');
  // @ts-expect-error inference diagnostics use the generated public DTO
  void invokeLegacy<unknown>('get_inference_stats');
  // @ts-expect-error telemetry diagnostics cannot regain the raw handwritten bridge
  void invokeLegacy<unknown>('get_recent_spans');
  // @ts-expect-error the one-shot crash notice is generated and renderer-safe
  void invokeLegacy<unknown>('take_last_crash');
  // @ts-expect-error duplicate-audio diagnostics use the generated typed contract
  void invokeCritical('get_fingerprint_count');
  // @ts-expect-error cancellation is a generated domain, never a handwritten escape hatch
  void invokeCritical('cancel_operation');
  // @ts-expect-error the dedicated refinement cancel signal is generated too
  void invokeLegacy<unknown>('cancel_wsl_refinement');
  // @ts-expect-error API-key status and mutation use one generated closed provider domain
  void invokeCritical('get_configured_providers');
  // @ts-expect-error secrets cannot regain the handwritten IPC surface
  void invokeCritical('set_api_key', { provider: 'gemini', key: 'secret' });
  // @ts-expect-error session persistence is generated and renderer-safe
  void invokeCritical('save_session', {
    searchQuery: '',
    sortOrder: 'newest',
    filterVerified: null,
  });
  // @ts-expect-error session restore uses the generated SessionState shape
  void invokeCritical('restore_session');
  // @ts-expect-error dataset analytics are generated and use public DTOs
  void invokeLegacy<unknown>('get_dataset_stats');
  // @ts-expect-error training readiness cannot return to handwritten IPC
  void invokeLegacy<unknown>('get_training_grade_breakdown');
  // @ts-expect-error certificate parameters use the generated command signature
  void invokeLegacy<unknown>('get_dataset_certificate');
  // @ts-expect-error opaque media grants cannot return to the path-bearing handwritten bridge
  void invokeCritical('register_media_asset', { audioPath: 'C:/private/source.wav' });
  // @ts-expect-error review media uses the same generated path-scrubbed contract
  void invokeLegacy<unknown>('register_review_media_asset');
  // @ts-expect-error media resolution returns an opaque protocol URL through generated IPC
  void invokeCritical('get_media_asset_url', { id: 'grant' });
  // @ts-expect-error the library segment/page contract is generated and typed
  void invokeLegacy<unknown>('get_segment');
  // @ts-expect-error contextual batch ids cannot regress into handwritten IPC
  void invokeLegacy<unknown>('get_segment_ids_for_view');
  // @ts-expect-error anomaly hydration uses the bounded generated contract
  void invokeLegacy<unknown>('get_signal_anomaly_segments');
  // @ts-expect-error segment metadata compare-and-set is generated, never handwritten
  void invokeLegacy<unknown>('update_segment_fields');
  // @ts-expect-error segment deletion is generated and shared by single/batch callers
  void invokeLegacy<unknown>('delete_segment');
  // @ts-expect-error the retired batch deletion bridge cannot return
  void invokeLegacy<unknown>('delete_segments_batch');
  // @ts-expect-error backup and recovery use generated, scrubbed contracts
  void invokeCritical('db_backup', { dest: 'D:/proof/library.db' });
  // @ts-expect-error destructive restore cannot regress into the handwritten boundary
  void invokeCritical('db_restore', { src: 'D:/proof/library.db' });
  // @ts-expect-error the legacy bridge accepts a closed command union, not runtime strings
  void invokeLegacy<unknown>('runtime_' + 'command');
};
void compileTimeContractProof;
