export const AUDIO_PHASES = [
  'idle',
  'resolving',
  'loading',
  'ready',
  'playing',
  'paused',
  'ended',
  'failed',
  'blocked',
] as const;

export type AudioPhase = (typeof AUDIO_PHASES)[number];

export interface AudioAttemptBinding {
  clipId: string;
  attemptId: number;
}

export interface AudioMachineState {
  phase: AudioPhase;
  clipId: string | null;
  sourceId: string | null;
  loadedSourceId: string | null;
  attemptId: number;
  errorCode: string | null;
}

export type AudioMachineEvent =
  | { type: 'select'; clipId: string; sourceId: string }
  | { type: 'retry' }
  | { type: 'playRequested' }
  | { type: 'pause' }
  | { type: 'reset' }
  | { type: 'resolved'; binding: AudioAttemptBinding }
  | { type: 'loaded'; binding: AudioAttemptBinding }
  | { type: 'playStarted'; binding: AudioAttemptBinding }
  | { type: 'ended'; binding: AudioAttemptBinding }
  | { type: 'failed'; binding: AudioAttemptBinding; errorCode: string }
  | { type: 'blocked'; binding: AudioAttemptBinding; errorCode: string };

export function createAudioMachine(): AudioMachineState {
  return {
    phase: 'idle',
    clipId: null,
    sourceId: null,
    loadedSourceId: null,
    attemptId: 0,
    errorCode: null,
  };
}

export function audioAttemptBinding(state: AudioMachineState): AudioAttemptBinding | null {
  return state.clipId === null ? null : { clipId: state.clipId, attemptId: state.attemptId };
}

export function isCurrentAudioAttempt(
  state: AudioMachineState,
  binding: AudioAttemptBinding,
): boolean {
  return state.clipId === binding.clipId && state.attemptId === binding.attemptId;
}

function nextAttempt(state: AudioMachineState): number {
  if (state.attemptId >= Number.MAX_SAFE_INTEGER) return 1;
  return state.attemptId + 1;
}

/**
 * Pure audio lifecycle reducer. Every asynchronous completion carries the clip and attempt that
 * created it; a late resolver, play promise, media failure, ended event, or timer is a no-op once a
 * newer clip/action owns the player.
 */
export function audioTransition(
  state: AudioMachineState,
  event: AudioMachineEvent,
): AudioMachineState {
  switch (event.type) {
    case 'select':
      return {
        ...state,
        phase: state.loadedSourceId === event.sourceId ? 'ready' : 'resolving',
        clipId: event.clipId,
        sourceId: event.sourceId,
        attemptId: nextAttempt(state),
        errorCode: null,
      };
    case 'retry':
      if (state.clipId === null || state.sourceId === null) return state;
      return {
        ...state,
        phase: 'resolving',
        attemptId: nextAttempt(state),
        errorCode: null,
      };
    case 'playRequested':
      if (state.clipId === null || !['ready', 'playing', 'paused', 'ended'].includes(state.phase)) {
        return state;
      }
      return { ...state, attemptId: nextAttempt(state), errorCode: null };
    case 'pause':
      if (state.clipId === null) return state;
      return {
        ...state,
        phase: state.phase === 'ended' ? 'ended' : 'paused',
        attemptId: nextAttempt(state),
      };
    case 'reset':
      return { ...createAudioMachine(), attemptId: nextAttempt(state) };
    case 'resolved':
      return isCurrentAudioAttempt(state, event.binding)
        ? { ...state, phase: 'loading', errorCode: null }
        : state;
    case 'loaded':
      return isCurrentAudioAttempt(state, event.binding)
        ? { ...state, phase: 'ready', loadedSourceId: state.sourceId, errorCode: null }
        : state;
    case 'playStarted':
      return isCurrentAudioAttempt(state, event.binding)
        ? { ...state, phase: 'playing', errorCode: null }
        : state;
    case 'ended':
      return isCurrentAudioAttempt(state, event.binding) ? { ...state, phase: 'ended' } : state;
    case 'failed':
      return isCurrentAudioAttempt(state, event.binding)
        ? { ...state, phase: 'failed', errorCode: event.errorCode }
        : state;
    case 'blocked':
      return isCurrentAudioAttempt(state, event.binding)
        ? { ...state, phase: 'blocked', errorCode: event.errorCode }
        : state;
  }
}
