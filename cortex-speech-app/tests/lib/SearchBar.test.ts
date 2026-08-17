import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/svelte';
import { get } from 'svelte/store';
import SearchBar from '../../src/lib/SearchBar.svelte';
import { filteredSegments, searchQuery, searchResults, filterVerified, sortOrder, segments } from '../../src/lib/stores/segmentStore';
import { ckb } from '../../src/lib/i18n/ckb';
import { getSegmentsPage } from '../../src/lib/commands';
import type { SpeechSegment } from '../../src/lib/types';

vi.mock('../../src/lib/commands', () => ({
  getSegmentsPage: vi.fn(() => Promise.resolve({ items: [], total: 0, nextCursor: null })),
  getDatasetCertificate: vi.fn(() => Promise.resolve({ threshold: 0.35 })),
  getDatasetStats: vi.fn(() => Promise.resolve({
    totalSegments: 0,
    verifiedCount: 0,
    pendingCount: 0,
    totalDurationSeconds: 0,
  })),
  getSegment: vi.fn(),
}));

const searchPlaceholder = ckb.searchPlaceholder;
const getSegmentsPageMock = vi.mocked(getSegmentsPage);

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

describe('SearchBar', () => {
  beforeEach(() => {
    getSegmentsPageMock.mockClear();
    delete window.__TAURI__;
    delete window.__TAURI_INTERNALS__;
    segments.set([]);
    searchQuery.set('');
    searchResults.set(null);
    filterVerified.set(null);
    sortOrder.set('newest');
  });

  it('updates the searchQuery store after debounce', async () => {
    vi.useFakeTimers();
    render(SearchBar);
    const input = screen.getByPlaceholderText(searchPlaceholder);
    await fireEvent.input(input, { target: { value: 'test query' } });
    await vi.advanceTimersByTimeAsync(250);
    expect(get(searchQuery)).toBe('test query');
    vi.useRealTimers();
  });

  it('debounces rapid input and uses the last value', async () => {
    vi.useFakeTimers();
    render(SearchBar);
    const input = screen.getByPlaceholderText(searchPlaceholder);
    await fireEvent.input(input, { target: { value: 'first' } });
    await vi.advanceTimersByTimeAsync(100);
    await fireEvent.input(input, { target: { value: 'second' } });
    await vi.advanceTimersByTimeAsync(100);
    await fireEvent.input(input, { target: { value: 'final' } });
    await vi.advanceTimersByTimeAsync(250);
    expect(get(searchQuery)).toBe('final');
    vi.useRealTimers();
  });

  it('clears the query and store when clear button is clicked', async () => {
    vi.useFakeTimers();
    render(SearchBar);
    const input = screen.getByPlaceholderText(searchPlaceholder);
    await fireEvent.input(input, { target: { value: 'something' } });
    await vi.advanceTimersByTimeAsync(250);
    expect(get(searchQuery)).toBe('something');

    await fireEvent.click(screen.getByLabelText(ckb.clearSearch));
    expect(get(searchQuery)).toBe('');
    expect(input).toHaveValue('');
    vi.useRealTimers();
  });

  it('shows the clear button only when query is non-empty', async () => {
    render(SearchBar);
    expect(screen.queryByLabelText(ckb.clearSearch)).not.toBeInTheDocument();

    const input = screen.getByPlaceholderText(searchPlaceholder);
    await fireEvent.input(input, { target: { value: 'x' } });
    expect(screen.getByLabelText(ckb.clearSearch)).toBeInTheDocument();
  });

  it('updates filterVerified when verified filter buttons are clicked', async () => {
    render(SearchBar);

    await fireEvent.click(screen.getByText(ckb.filterVerified));
    expect(get(filterVerified)).toBe(true);

    await fireEvent.click(screen.getByText(ckb.filterPending));
    expect(get(filterVerified)).toBe(false);

    await fireEvent.click(screen.getByText(ckb.all));
    expect(get(filterVerified)).toBeNull();
  });

  it('updates sortOrder when the select value changes', async () => {
    render(SearchBar);
    const select = screen.getByDisplayValue(ckb.sortNewest);
    await fireEvent.input(select, { target: { value: 'oldest' } });
    expect(get(sortOrder)).toBe('oldest');

    await fireEvent.input(select, { target: { value: 'duration' } });
    expect(get(sortOrder)).toBe('duration');

    await fireEvent.input(select, { target: { value: 'verified' } });
    expect(get(sortOrder)).toBe('verified');
  });

  it('uses local in-memory filtering in browser mode without backend search', async () => {
    vi.useFakeTimers();
    segments.set([
      makeSegment({ id: 'a', rawTranscript: 'hello sorani speech', speakerId: 'halwest' }),
      makeSegment({ id: 'b', rawTranscript: 'unrelated transcript', speakerId: 'other' }),
    ]);

    render(SearchBar);
    const input = screen.getByPlaceholderText(searchPlaceholder);
    await fireEvent.input(input, { target: { value: 'sorani' } });
    await vi.advanceTimersByTimeAsync(250);

    expect(get(searchQuery)).toBe('sorani');
    expect(get(searchResults)).toBeNull();
    expect(get(filteredSegments).map((segment) => segment.id)).toEqual(['a']);
    vi.useRealTimers();
  });

  it('reloads a bounded server-side search window in desktop mode', async () => {
    vi.useFakeTimers();
    window.__TAURI_INTERNALS__ = {};
    render(SearchBar);
    await fireEvent.input(screen.getByPlaceholderText(searchPlaceholder), {
      target: { value: 'sorani' },
    });
    await vi.advanceTimersByTimeAsync(250);
    await vi.runAllTimersAsync();
    expect(getSegmentsPageMock).toHaveBeenCalledWith(
      expect.objectContaining({ query: 'sorani', limit: 200, cursor: null }),
    );
    vi.useRealTimers();
  });
});
