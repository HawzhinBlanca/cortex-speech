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
  'couch_review_status',
  'export_agreement_sample',
  'reviewer_throughput',
  'revoke_couch_reviewer',
  'spot_check_report',
  'start_couch_review',
  'stop_couch_review',
] as const;

type CriticalLegacyIpcContract = {
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
