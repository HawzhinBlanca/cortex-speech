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
    invokeMock.mockResolvedValueOnce({ integrityOk: true, segmentCount: 12 });

    const result = invokeCritical('db_backup', { dest: 'D:/proof/library.db' });
    expectTypeOf(result).toEqualTypeOf<Promise<{ integrityOk: boolean; segmentCount: number }>>();
    await expect(result).resolves.toEqual({
      integrityOk: true,
      segmentCount: 12,
    });
    expect(invokeMock).toHaveBeenCalledWith('db_backup', { dest: 'D:/proof/library.db' });
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
  // @ts-expect-error the library segment/page contract is generated and typed
  void invokeLegacy<unknown>('get_segment');
  // @ts-expect-error contextual batch ids cannot regress into handwritten IPC
  void invokeLegacy<unknown>('get_segment_ids_for_view');
  // @ts-expect-error anomaly hydration uses the bounded generated contract
  void invokeLegacy<unknown>('get_signal_anomaly_segments');
  // @ts-expect-error destructive restore requires its exact source argument
  void invokeCritical('db_restore');
  // @ts-expect-error the legacy bridge accepts a closed command union, not runtime strings
  void invokeLegacy<unknown>('runtime_' + 'command');
};
void compileTimeContractProof;
