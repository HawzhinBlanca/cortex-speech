import { cleanup, render, screen, waitFor } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import StatsDashboard from '../../src/lib/StatsDashboard.svelte';
import { locale } from '../../src/lib/i18n';
import { segments } from '../../src/lib/stores/segmentStore';
import type { SpeechSegment } from '../../src/lib/types';

const invokeMock = vi.mocked(invoke);

function makeSegment(overrides: Partial<SpeechSegment>): SpeechSegment {
  return {
    id: 'seg-1',
    audioPath: 'C:\\audio\\sample.wav',
    rawTranscript: '',
    normalizedTranscript: null,
    annotatedTranscript: null,
    alignmentJson: null,
    durationMs: 0,
    speakerId: null,
    verified: false,
    ...overrides,
  };
}

describe('StatsDashboard browser runtime', () => {
  beforeEach(() => {
    invokeMock.mockReset();
    delete window.__TAURI__;
    delete window.__TAURI_INTERNALS__;
    locale.set('en');
  });

  afterEach(() => {
    cleanup();
    segments.set([]);
  });

  it('renders local in-memory stats without invoking Tauri', async () => {
    segments.set([
      makeSegment({
        id: 'seg-1',
        rawTranscript: 'one two',
        durationMs: 30_000,
        speakerId: 'speaker-a',
        verified: true,
      }),
      makeSegment({
        id: 'seg-2',
        rawTranscript: 'three four five',
        durationMs: 45_000,
        speakerId: 'speaker-a',
        verified: false,
      }),
    ]);

    render(StatsDashboard);

    expect(await screen.findByText('Dataset Statistics')).toBeInTheDocument();
    expect(screen.getByText('Total Segments')).toBeInTheDocument();
    expect(screen.getAllByText('1m 15s').length).toBeGreaterThan(0);
    await waitFor(() => expect(invokeMock).not.toHaveBeenCalled());
  });
});
