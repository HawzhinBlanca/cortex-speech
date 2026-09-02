import { describe, expect, it } from 'vitest';
import {
  AUDIO_PHASES,
  audioAttemptBinding,
  audioTransition,
  createAudioMachine,
  isCurrentAudioAttempt,
  type AudioMachineEvent,
} from '../../src/lib/audioMachine';

describe('audio lifecycle state machine', () => {
  it('binds asynchronous work to both the exact clip and the exact attempt', () => {
    const state = {
      ...createAudioMachine(),
      phase: 'ready' as const,
      clipId: 'clip-a',
      sourceId: 'recording.wav',
      attemptId: 42,
    };
    expect(isCurrentAudioAttempt(state, { clipId: 'clip-a', attemptId: 42 })).toBe(true);
    expect(isCurrentAudioAttempt(state, { clipId: 'clip-a', attemptId: 41 })).toBe(false);
    expect(isCurrentAudioAttempt(state, { clipId: 'clip-b', attemptId: 42 })).toBe(false);
  });

  it('rolls the safe-integer attempt boundary to one instead of producing an unsafe identity', () => {
    const state = { ...createAudioMachine(), attemptId: Number.MAX_SAFE_INTEGER };
    const selected = audioTransition(state, {
      type: 'select',
      clipId: 'clip-a',
      sourceId: 'recording.wav',
    });
    expect(selected.attemptId).toBe(1);
    expect(Number.isSafeInteger(selected.attemptId)).toBe(true);
  });

  it('retries only an exact selected source and clears the prior terminal error', () => {
    const idle = createAudioMachine();
    expect(audioTransition(idle, { type: 'retry' })).toBe(idle);

    const missingSource = {
      ...idle,
      clipId: 'clip-a',
      phase: 'failed' as const,
      errorCode: 'DECODE_FAILED',
    };
    expect(audioTransition(missingSource, { type: 'retry' })).toBe(missingSource);

    const missingClip = {
      ...idle,
      sourceId: 'recording.wav',
      phase: 'failed' as const,
      errorCode: 'DECODE_FAILED',
    };
    expect(audioTransition(missingClip, { type: 'retry' })).toBe(missingClip);

    const failed = {
      ...missingSource,
      sourceId: 'recording.wav',
      attemptId: 7,
    };
    expect(audioTransition(failed, { type: 'retry' })).toEqual({
      ...failed,
      phase: 'resolving',
      attemptId: 8,
      errorCode: null,
    });
  });

  it('starts a fresh play attempt only from the four playable phases', () => {
    const selected = {
      ...createAudioMachine(),
      clipId: 'clip-a',
      sourceId: 'recording.wav',
      attemptId: 10,
    };
    for (const phase of ['ready', 'playing', 'paused', 'ended'] as const) {
      const state = { ...selected, phase, errorCode: 'OLD_ERROR' };
      expect(audioTransition(state, { type: 'playRequested' })).toEqual({
        ...state,
        attemptId: 11,
        errorCode: null,
      });
    }
    for (const phase of ['idle', 'resolving', 'loading', 'failed', 'blocked'] as const) {
      const state = { ...selected, phase };
      expect(audioTransition(state, { type: 'playRequested' })).toBe(state);
    }
    const idle = createAudioMachine();
    expect(audioTransition(idle, { type: 'playRequested' })).toBe(idle);
    const missingClip = {
      ...idle,
      phase: 'ready' as const,
      sourceId: 'recording.wav',
    };
    expect(audioTransition(missingClip, { type: 'playRequested' })).toBe(missingClip);
  });

  it('keeps ended terminal on pause and otherwise creates an exact paused attempt', () => {
    const ended = {
      ...createAudioMachine(),
      phase: 'ended' as const,
      clipId: 'clip-a',
      sourceId: 'recording.wav',
      attemptId: 3,
    };
    expect(audioTransition(ended, { type: 'pause' })).toEqual({
      ...ended,
      phase: 'ended',
      attemptId: 4,
    });
    expect(audioTransition({ ...ended, phase: 'playing' }, { type: 'pause' })).toEqual({
      ...ended,
      phase: 'paused',
      attemptId: 4,
    });
    const idle = createAudioMachine();
    expect(audioTransition(idle, { type: 'pause' })).toBe(idle);
  });

  it('ignores every late completion from the previous clip and attempt', () => {
    let state = audioTransition(createAudioMachine(), {
      type: 'select',
      clipId: 'clip-a',
      sourceId: 'recording-a.wav',
    });
    const stale = audioAttemptBinding(state)!;
    state = audioTransition(state, {
      type: 'select',
      clipId: 'clip-b',
      sourceId: 'recording-b.wav',
    });
    const selectedB = state;

    for (const event of [
      { type: 'resolved', binding: stale },
      { type: 'loaded', binding: stale },
      { type: 'playStarted', binding: stale },
      { type: 'ended', binding: stale },
      { type: 'failed', binding: stale, errorCode: 'OLD_FAILURE' },
      { type: 'blocked', binding: stale, errorCode: 'OLD_BLOCK' },
    ] satisfies AudioMachineEvent[]) {
      state = audioTransition(state, event);
      expect(state).toEqual(selectedB);
    }
  });

  it('reuses a loaded source for the next clip but creates a fresh attempt', () => {
    let state = audioTransition(createAudioMachine(), {
      type: 'select',
      clipId: 'clip-a',
      sourceId: 'one-recording.wav',
    });
    const first = audioAttemptBinding(state)!;
    state = audioTransition(state, { type: 'resolved', binding: first });
    state = audioTransition(state, { type: 'loaded', binding: first });
    expect(state.phase).toBe('ready');

    const oldAttempt = state.attemptId;
    state = audioTransition(state, {
      type: 'select',
      clipId: 'clip-b',
      sourceId: 'one-recording.wav',
    });
    expect(state.phase).toBe('ready');
    expect(state.attemptId).toBe(oldAttempt + 1);
    expect(state.loadedSourceId).toBe('one-recording.wav');
  });

  it('follows the declared lifecycle and keeps blocked distinct from failed', () => {
    let state = audioTransition(createAudioMachine(), {
      type: 'select',
      clipId: 'clip-a',
      sourceId: 'recording.wav',
    });
    expect(state.phase).toBe('resolving');
    let binding = audioAttemptBinding(state)!;
    state = audioTransition(state, { type: 'resolved', binding });
    expect(state.phase).toBe('loading');
    state = audioTransition(state, { type: 'loaded', binding });
    expect(state.phase).toBe('ready');
    state = audioTransition(state, { type: 'playRequested' });
    binding = audioAttemptBinding(state)!;
    state = audioTransition(state, { type: 'playStarted', binding });
    expect(state.phase).toBe('playing');
    state = audioTransition(state, {
      type: 'blocked',
      binding,
      errorCode: 'AUTOPLAY_POLICY',
    });
    expect(state.phase).toBe('blocked');
    expect(state.errorCode).toBe('AUTOPLAY_POLICY');
  });

  it('survives 10,000 randomized transitions without cross-clip state corruption', () => {
    let random = 0x5eed1234;
    const nextRandom = () => {
      random = (Math.imul(random, 1664525) + 1013904223) >>> 0;
      return random;
    };
    let state = createAudioMachine();
    let lastAttempt = state.attemptId;
    const visited = new Set([state.phase]);

    for (let step = 0; step < 10_000; step += 1) {
      const current = audioAttemptBinding(state);
      const stale = {
        clipId: `stale-${step}`,
        attemptId: Math.max(0, state.attemptId - 1),
      };
      const binding = nextRandom() % 3 === 0 || current === null ? stale : current;
      const choice = nextRandom() % 11;
      let event: AudioMachineEvent;
      switch (choice) {
        case 0:
          event = {
            type: 'select',
            clipId: `clip-${step}`,
            sourceId: `source-${nextRandom() % 7}.wav`,
          };
          break;
        case 1:
          event = { type: 'retry' };
          break;
        case 2:
          event = { type: 'playRequested' };
          break;
        case 3:
          event = { type: 'pause' };
          break;
        case 4:
          event = { type: 'resolved', binding };
          break;
        case 5:
          event = { type: 'loaded', binding };
          break;
        case 6:
          event = { type: 'playStarted', binding };
          break;
        case 7:
          event = { type: 'ended', binding };
          break;
        case 8:
          event = { type: 'failed', binding, errorCode: 'DECODE_FAILED' };
          break;
        case 9:
          event = { type: 'blocked', binding, errorCode: 'PLAYBACK_BLOCKED' };
          break;
        default:
          event = { type: 'reset' };
      }

      const before = state;
      const wasStale = 'binding' in event && !isCurrentAudioAttempt(before, event.binding);
      state = audioTransition(state, event);

      expect(AUDIO_PHASES).toContain(state.phase);
      expect(Number.isSafeInteger(state.attemptId)).toBe(true);
      expect(state.attemptId).toBeGreaterThanOrEqual(0);
      if (lastAttempt !== Number.MAX_SAFE_INTEGER && event.type !== 'reset') {
        expect(state.attemptId).toBeGreaterThanOrEqual(lastAttempt);
      }
      if (wasStale) expect(state).toBe(before);
      if (state.clipId === null) {
        expect(state.sourceId).toBeNull();
        expect(state.phase).toBe('idle');
      }
      visited.add(state.phase);
      lastAttempt = state.attemptId;
    }
    expect(visited).toEqual(new Set(AUDIO_PHASES));
  });
});
