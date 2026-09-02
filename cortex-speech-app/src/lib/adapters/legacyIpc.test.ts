import { invoke } from '@tauri-apps/api/core';
import { describe, expect, it, vi } from 'vitest';
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
});

// Compile-time proof: generated commands cannot return to the handwritten boundary, critical
// command arguments cannot be omitted, and arbitrary runtime command strings cannot enter it.
const compileTimeContractProof = (): void => {
  // @ts-expect-error owner audio selection is generated and typed
  void invokeLegacy<unknown>('open_audio_file');
  // @ts-expect-error both import starts use generated run-identity contracts
  void invokeCritical('import_directory', { runId: 'run-1' });
  // @ts-expect-error single-file import is no longer handwritten
  void invokeLegacy<unknown>('import_audio_file');
  // @ts-expect-error champion transcription uses the generated result and structured refusal
  void invokeLegacy<unknown>('transcribe_segment');
  // @ts-expect-error alignment timestamps are generated DTOs
  void invokeLegacy<unknown>('align_segment');
  // @ts-expect-error consensus evidence uses the generated provenance DTO
  void invokeLegacy<unknown>('get_segment_consensus');
  // @ts-expect-error waveform loading uses the generated typed command
  void invokeLegacy<unknown>('get_waveform');
  // @ts-expect-error dataset export now has a generated structured error boundary
  void invokeCritical('export_dataset', { path: 'D:/proof/library.jsonl', format: 'jsonl' });
  // @ts-expect-error transcript export now has a generated structured error boundary
  void invokeLegacy<unknown>('export_transcript');
  // @ts-expect-error audio-health results use the generated wire contract
  void invokeCritical('get_audio_health');
  // @ts-expect-error relink outcomes and refusals are generated
  void invokeLegacy<unknown>('relink_audio');
  // @ts-expect-error dataset validation uses its generated report domain
  void invokeCritical('validate_dataset_cmd');
  // @ts-expect-error reviewed-audio export uses generated options, results, and errors
  void invokeLegacy<unknown>('export_audio');
  // @ts-expect-error dataset merge has a generated closed result shape
  void invokeCritical('merge_dataset_json', { jsonContent: '{}' });
  // @ts-expect-error Hugging Face export uses generated structured errors
  void invokeLegacy<unknown>('export_huggingface_dataset');
  // @ts-expect-error gold promotion is generated and path-safe
  void invokeCritical('create_gold_from_file', { audioPath: 'D:/owner/source.wav' });
  // @ts-expect-error bulk gold promotion no longer uses handwritten IPC
  void invokeLegacy<unknown>('import_verified_segments_as_gold');
  // @ts-expect-error gold eval export uses its generated summary
  void invokeCritical('export_gold_eval_set', { outDir: 'D:/proof/gold' });
  // @ts-expect-error fine-tune export uses its complete generated provenance summary
  void invokeLegacy<unknown>('export_finetune_pack');
  // @ts-expect-error scorecards use exact generated eval and score DTOs
  void invokeLegacy<unknown>('build_scorecard');
  // @ts-expect-error signal anomaly mutation uses a generated structured refusal
  void invokeLegacy<unknown>('compute_signal_anomaly_scores');
  // @ts-expect-error active-learning queue arguments and segment results are generated
  void invokeLegacy<unknown>('get_active_learning_queue');
  // @ts-expect-error escalation queue results use the generated segment contract
  void invokeLegacy<unknown>('get_escalation_queue');
  // @ts-expect-error escalation trend evidence uses its generated DTO
  void invokeLegacy<unknown>('get_escalation_rate_trend');
  // @ts-expect-error intelligence evidence uses a closed generated report
  void invokeLegacy<unknown>('get_intelligence_report');
  // @ts-expect-error stored eval-run evidence uses the generated public DTO
  void invokeLegacy<unknown>('list_eval_runs');
  // @ts-expect-error champion gold evaluation uses generated results and hard-stop errors
  void invokeLegacy<unknown>('run_gold_eval_asr');
  // @ts-expect-error the jury report is an exact generated current/retired-mode union
  void invokeLegacy<unknown>('run_jury_pipeline');
  // @ts-expect-error T2 uses generated verdict/evidence and scrubbed error contracts
  void invokeLegacy<unknown>('run_t2_for_segment');
  // @ts-expect-error WSL refinement admission uses a generated started result
  void invokeLegacy<unknown>('run_wsl_refinement');
  // @ts-expect-error rediarization uses generated admission, result, and structured errors
  void invokeLegacy<unknown>('rediarize_segments');
  // @ts-expect-error generated playback is intentionally absent from the handwritten inventory
  void invokeLegacy<unknown>('begin_desktop_playback_session_v1');
  // @ts-expect-error generic review flags are revision-bound generated IPC, never handwritten
  void invokeCritical('record_review_flag', {
    request: {
      operationId: 'operation-1',
      segmentId: 'segment-1',
      baseRevision: 7,
      rationale: 'needs another listen',
    },
  });
  // @ts-expect-error the complete desktop-history domain is generated, not handwritten
  void invokeCritical('undo');
  // @ts-expect-error generated history queries cannot regress into the legacy inventory
  void invokeLegacy<unknown>('can_redo');
  // @ts-expect-error transcript utilities are generated, not handwritten
  void invokeLegacy<unknown>('compute_diff');
  // @ts-expect-error generated normalization cannot regress into the legacy inventory
  void invokeLegacy<unknown>('normalize_text');
  // @ts-expect-error durable batch starts use generated typed admission contracts
  void invokeLegacy<unknown>('batch_transcribe');
  // @ts-expect-error durable normalization uses the same generated admission contract
  void invokeLegacy<unknown>('batch_normalize');
  // @ts-expect-error terminal batch acknowledgement is generated and exact-id typed
  void invokeLegacy<unknown>('acknowledge_batch_run');
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
  // @ts-expect-error interrupted-import discovery is generated and path-scrubbed
  void invokeCritical('get_interrupted_import');
  // @ts-expect-error interrupted-import resume cannot regress into handwritten IPC
  void invokeLegacy<unknown>('resume_interrupted_import');
  // @ts-expect-error interrupted-import discard uses the generated typed identity
  void invokeCritical('discard_interrupted_import', { jobId: 'job-1' });
  // @ts-expect-error the legacy bridge accepts a closed command union, not runtime strings
  void invokeLegacy<unknown>('runtime_' + 'command');
};
void compileTimeContractProof;
