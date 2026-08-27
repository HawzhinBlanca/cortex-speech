import { invoke as invokeDesktop } from '@tauri-apps/api/core';

type CommandService = typeof import('../commands');
type CommandResult<Name extends keyof CommandService> = Awaited<
  ReturnType<
    CommandService[Name] extends (...args: never[]) => unknown ? CommandService[Name] : never
  >
>;

/**
 * Closed inventory of renderer commands that have not yet moved into the generated Specta
 * contract. This is deliberately a transitional boundary: adding a handwritten IPC command
 * requires changing this audited list, while generated commands must be called through
 * `generated/ipc.ts` instead.
 *
 * Call sites are additionally architecture-tested to use a string literal and an explicit result
 * type. The runtime membership check keeps the boundary fail-closed even if untyped JavaScript or
 * a future build step bypasses TypeScript.
 */
export const LEGACY_IPC_COMMANDS = [
  'acknowledge_quarantine',
  'align_segment',
  'batch_normalize',
  'batch_transcribe',
  'bootstrap_legacy_champion',
  'build_scorecard',
  'check_agentic_readiness',
  'compute_signal_anomaly_scores',
  'couch_review_status',
  'create_gold_from_file',
  'db_backup',
  'db_restore',
  'db_vacuum',
  'discard_interrupted_import',
  'export_agreement_sample',
  'export_audio',
  'export_dataset',
  'export_finetune_pack',
  'export_gold_eval_set',
  'export_huggingface_dataset',
  'export_transcript',
  'get_active_learning_queue',
  'get_audio_health',
  'get_champion_engine_status',
  'get_escalation_queue',
  'get_escalation_rate_trend',
  'get_intelligence_report',
  'get_interrupted_import',
  'get_jobs',
  'get_media_asset_url',
  'get_quarantine_notice',
  'get_segment_consensus',
  'get_waveform',
  'import_audio_file',
  'import_directory',
  'import_model_checkpoint',
  'import_model_deployment',
  'import_verified_segments_as_gold',
  'list_agent_import_reports',
  'list_agent_stage_events',
  'list_db_snapshots',
  'list_eval_runs',
  'list_model_versions',
  'merge_dataset_json',
  'models_download_all',
  'models_status',
  'open_audio_file',
  'record_review_flag',
  'rediarize_segments',
  'register_media_asset',
  'register_review_media_asset',
  'relink_audio',
  'restore_db_from_snapshot',
  'resume_interrupted_import',
  'reviewer_throughput',
  'revoke_couch_reviewer',
  'run_gold_eval_asr',
  'run_jury_pipeline',
  'run_t2_for_segment',
  'run_wsl_refinement',
  'spot_check_report',
  'start_champion_engine',
  'start_couch_review',
  'stop_couch_review',
  'transcribe_segment',
  'undo_human_decision',
  'undo_review_flag',
  'validate_dataset_cmd',
] as const;

type CriticalLegacyIpcContract = {
  open_audio_file: { args: undefined; result: CommandResult<'openAudioFile'> };
  import_directory: { args: undefined; result: CommandResult<'importDirectory'> };
  get_interrupted_import: { args: undefined; result: CommandResult<'getInterruptedImport'> };
  resume_interrupted_import: {
    args: undefined;
    result: CommandResult<'resumeInterruptedImport'>;
  };
  discard_interrupted_import: {
    args: { jobId: string };
    result: CommandResult<'discardInterruptedImport'>;
  };
  import_audio_file: { args: { path: string }; result: CommandResult<'importAudioFile'> };
  export_dataset: {
    args: { path: string; format: string };
    result: CommandResult<'exportDataset'>;
  };
  export_transcript: {
    args: { path: string; format: 'txt' | 'srt' | 'vtt' };
    result: CommandResult<'exportTranscript'>;
  };
  register_media_asset: {
    args: { audioPath: string };
    result: CommandResult<'registerMediaAsset'>;
  };
  register_review_media_asset: {
    args: { audioPath: string };
    result: CommandResult<'registerReviewMediaAsset'>;
  };
  get_media_asset_url: { args: { id: string }; result: CommandResult<'getMediaAssetUrl'> };
  start_couch_review: {
    args: { reviewers: string[] };
    result: CommandResult<'startCouchReview'>;
  };
  stop_couch_review: { args: undefined; result: CommandResult<'stopCouchReview'> };
  couch_review_status: { args: undefined; result: CommandResult<'couchReviewStatus'> };
  spot_check_report: { args: undefined; result: CommandResult<'spotCheckReport'> };
  reviewer_throughput: { args: undefined; result: CommandResult<'reviewerThroughput'> };
  revoke_couch_reviewer: {
    args: { reviewer: string };
    result: CommandResult<'revokeCouchReviewer'>;
  };
  export_agreement_sample: {
    args: undefined;
    result: CommandResult<'exportAgreementSample'>;
  };
  import_model_checkpoint: {
    args: {
      id: string;
      checkpointPath: string;
      source: string;
      license: string;
      modelCardName: string | null;
    };
    result: CommandResult<'importModelCheckpoint'>;
  };
  import_model_deployment: {
    args: {
      manifestPath: string;
      expectedDeploymentSha256: string;
      expectedModelId: string;
      source: string;
      license: string;
    };
    result: CommandResult<'importModelDeployment'>;
  };
  bootstrap_legacy_champion: {
    args: {
      manifestPath: string;
      expectedDeploymentSha256: string;
      expectedModelId: string;
      license: string;
    };
    result: CommandResult<'bootstrapLegacyChampion'>;
  };
  get_quarantine_notice: { args: undefined; result: CommandResult<'getQuarantineNotice'> };
  list_db_snapshots: { args: undefined; result: CommandResult<'listDbSnapshots'> };
  restore_db_from_snapshot: {
    args: { name: string };
    result: CommandResult<'restoreDbFromSnapshot'>;
  };
  get_audio_health: { args: undefined; result: CommandResult<'getAudioHealth'> };
  relink_audio: { args: { searchDir: string }; result: CommandResult<'relinkAudio'> };
  validate_dataset_cmd: { args: undefined; result: CommandResult<'validateDataset'> };
  export_audio: {
    args: {
      segmentIds: string[];
      options: {
        output_dir: string;
        format: 'Wav';
        sample_rate: number;
        include_metadata: boolean;
      };
    };
    result: CommandResult<'exportAudio'>;
  };
  merge_dataset_json: {
    args: { jsonContent: string };
    result: CommandResult<'mergeDatasetJson'>;
  };
  export_huggingface_dataset: {
    args: { path: string };
    result: CommandResult<'exportHuggingfaceDataset'>;
  };
  db_backup: { args: { dest: string }; result: CommandResult<'dbBackup'> };
  acknowledge_quarantine: { args: undefined; result: CommandResult<'acknowledgeQuarantine'> };
  db_restore: { args: { src: string }; result: CommandResult<'dbRestore'> };
  db_vacuum: { args: undefined; result: CommandResult<'dbVacuum'> };
  create_gold_from_file: {
    args: { audioPath: string };
    result: CommandResult<'createGoldFromFile'>;
  };
  import_verified_segments_as_gold: {
    args: undefined;
    result: CommandResult<'importVerifiedSegmentsAsGold'>;
  };
  export_gold_eval_set: {
    args: { outDir: string };
    result: CommandResult<'exportGoldEvalSet'>;
  };
  export_finetune_pack: {
    args: { outDir: string };
    result: CommandResult<'exportFinetunePack'>;
  };
  undo_human_decision: {
    args: { effectEventId: number; operationId: string };
    result: CommandResult<'undoHumanDecision'>;
  };
  record_review_flag: {
    args: { segmentId: string; rationale: string; operationId: string };
    result: CommandResult<'recordReviewFlag'>;
  };
  undo_review_flag: {
    args: { effectEventId: number; operationId: string };
    result: CommandResult<'undoReviewFlag'>;
  };
};

export type CriticalLegacyIpcCommand = keyof CriticalLegacyIpcContract;
export type LegacyIpcCommand = Exclude<
  (typeof LEGACY_IPC_COMMANDS)[number],
  CriticalLegacyIpcCommand
>;

const legacyIpcCommandSet: ReadonlySet<string> = new Set(LEGACY_IPC_COMMANDS);

/**
 * The sole bridge for handwritten IPC still awaiting Rust-generated bindings.
 *
 * `Result` remains explicit at every caller because these legacy Rust commands do not yet expose
 * Specta DTOs. This is honest compile-time containment, not a claim that handwritten result types
 * are generated or runtime-validated.
 */
export function invokeLegacy<Result>(
  command: LegacyIpcCommand,
  args?: Record<string, unknown>,
): Promise<Result> {
  if (!legacyIpcCommandSet.has(command)) {
    return Promise.reject(new Error(`Refusing unregistered legacy IPC command: ${command}`));
  }
  return invokeRegistered<Result>(command, args);
}

type CriticalArgs<Command extends CriticalLegacyIpcCommand> =
  CriticalLegacyIpcContract[Command]['args'] extends undefined
    ? []
    : [args: CriticalLegacyIpcContract[Command]['args']];

/** Command-specific argument and result types for human-truth, payment, settings and recovery IPC. */
export function invokeCritical<Command extends CriticalLegacyIpcCommand>(
  command: Command,
  ...args: CriticalArgs<Command>
): Promise<CriticalLegacyIpcContract[Command]['result']> {
  return invokeRegistered<CriticalLegacyIpcContract[Command]['result']>(
    command,
    args[0] as Record<string, unknown> | undefined,
  );
}

function invokeRegistered<Result>(
  command: (typeof LEGACY_IPC_COMMANDS)[number],
  args?: Record<string, unknown>,
): Promise<Result> {
  if (!legacyIpcCommandSet.has(command)) {
    return Promise.reject(new Error(`Refusing unregistered handwritten IPC command: ${command}`));
  }
  const invokeArgs: [] | [Record<string, unknown>] = args === undefined ? [] : [args];
  return invokeDesktop<Result>(command, ...invokeArgs);
}
