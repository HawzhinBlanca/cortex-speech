import { test, expect } from './fixtures';
import {
  durableReviewBackendSnapshot,
  enableDurableReviewRestartStory,
} from './helpers/tauri-mock';

const UUID_PATTERN = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
const CORRECTED_TRUTH = 'durable corrected truth';

async function enterReview(page: import('@playwright/test').Page) {
  await expect(page.getByTestId('review-nudge-start')).toBeVisible({ timeout: 15_000 });
  await page.getByTestId('review-nudge-start').click();
  await expect(page.getByTestId('review-action-bar')).toBeVisible();
  return page.locator('.review-transcript-input');
}

async function enterReviewInbox(page: import('@playwright/test').Page) {
  await page.getByTestId('header-overflow-btn').click();
  await page.getByTestId('review-inbox-btn').click();
  const dialog = page.getByRole('dialog');
  await expect(dialog).toBeVisible({ timeout: 15_000 });
  return dialog;
}

async function listenToCurrentClip(page: import('@playwright/test').Page) {
  const controls = page.getByTestId('audio-player-controls');
  const speed = controls.getByRole('button', { name: 'Playback Speed', exact: true });
  await expect(speed).toBeVisible({ timeout: 15_000 });
  for (let index = 0; index < 3; index += 1) await speed.click();
  await expect(speed).toHaveText('2x');

  await controls.getByRole('button', { name: 'Play', exact: true }).click();
  await expect(controls.getByRole('button', { name: 'Pause', exact: true })).toBeVisible();
  await expect(controls.getByRole('button', { name: 'Play', exact: true })).toBeVisible({
    timeout: 15_000,
  });
}

test('owner review truth survives restart, hydrates exact Undo, and stays restored after restart', async ({
  page,
}) => {
  await enableDurableReviewRestartStory(page);
  await page.goto('/');

  const editor = await enterReview(page);
  await expect(editor).toHaveValue('hello world');
  await listenToCurrentClip(page);
  await editor.fill(CORRECTED_TRUTH);

  const save = page.getByRole('button', { name: 'Save & next', exact: true });
  await expect(save).toBeEnabled();
  await save.click();
  await expect(editor).toHaveValue('second durable clip', { timeout: 15_000 });

  const committed = await durableReviewBackendSnapshot(page);
  const committedPrimary = committed.segments.find((row) => row.segment.id === 'e2e-segment-1');
  expect(committedPrimary).toMatchObject({
    revision: 1,
    segment: {
      verified: true,
      humanDecision: 'edit',
      verdictTranscript: CORRECTED_TRUTH,
      annotatedTranscript: CORRECTED_TRUTH,
    },
  });
  expect(committed.latestCommit).toMatchObject({
    segmentId: 'e2e-segment-1',
    decision: 'edit',
  });
  expect(committed.latestCommit?.operationId).toMatch(UUID_PATTERN);
  expect(committed.latestCommit?.payloadHash).toMatch(/^[0-9a-f]{64}$/);
  expect(committed.committedOperationCount).toBe(1);
  expect(committed.undoOperationCount).toBe(0);
  if (committed.undoAvailability.status !== 'available') {
    throw new Error('the process-backed review store did not publish exact Undo authority');
  }
  expect(committed.undoAvailability.target).toMatchObject({
    kind: 'decision',
    effectEventId: committed.latestCommit?.effectEventId,
    segmentId: 'e2e-segment-1',
    decision: 'edit',
    sourceOperationId: committed.latestCommit?.operationId,
    sourcePayloadHash: committed.latestCommit?.payloadHash,
    databaseGeneration: 1,
  });

  // Reload destroys every Svelte controller/module variable. The fake native store is a Node-side
  // closure owned by the Playwright fixture, so only an authoritative IPC hydration can re-enable
  // this exact target in the new renderer document.
  await page.reload();
  const reloadedEditor = await enterReview(page);
  await expect(reloadedEditor).toHaveValue('second durable clip');
  const undo = page.getByRole('button', {
    name: 'Undo last review action',
    exact: true,
  });
  await expect(undo).toBeEnabled();

  const hydrated = await durableReviewBackendSnapshot(page);
  expect(hydrated.availabilityReads).toBeGreaterThan(committed.availabilityReads);
  expect(hydrated.latestCommit).toEqual(committed.latestCommit);
  expect(hydrated.committedOperationCount).toBe(1);
  expect(hydrated.undoOperationCount).toBe(0);

  await undo.click();
  await expect(reloadedEditor).toHaveValue('hello world', { timeout: 15_000 });

  const restored = await durableReviewBackendSnapshot(page);
  const restoredPrimary = restored.segments.find((row) => row.segment.id === 'e2e-segment-1');
  expect(restoredPrimary).toMatchObject({
    revision: 2,
    segment: {
      rawTranscript: 'hello world',
      normalizedTranscript: 'hello world',
      annotatedTranscript: 'hello world',
      verified: false,
      humanDecision: null,
      verdict: null,
      verdictTranscript: null,
      correctedAt: null,
    },
  });
  expect(restored.latestUndo?.operationId).toMatch(UUID_PATTERN);
  expect(restored.latestUndo?.operationId).not.toBe(committed.latestCommit?.operationId);
  expect(restored.latestUndo?.target).toEqual(committed.undoAvailability.target);
  expect(restored.undoAvailability).toEqual({
    status: 'blocked',
    reason: 'latestDecisionUndone',
  });
  expect(restored.committedOperationCount).toBe(1);
  expect(restored.undoOperationCount).toBe(1);

  // A second renderer restart must hydrate the restored database truth, not the old correction and
  // not a renderer cache. It also must not replay either durable operation during startup.
  await page.reload();
  const twiceReloadedEditor = await enterReview(page);
  await expect(twiceReloadedEditor).toHaveValue('hello world');
  const twiceReloaded = await durableReviewBackendSnapshot(page);
  expect(twiceReloaded.segments).toEqual(restored.segments);
  expect(twiceReloaded.undoAvailability).toEqual(restored.undoAvailability);
  expect(twiceReloaded.availabilityReads).toBeGreaterThan(restored.availabilityReads);
  expect(twiceReloaded.committedOperationCount).toBe(1);
  expect(twiceReloaded.undoOperationCount).toBe(1);
  await expect(
    page.getByRole('button', { name: 'Undo last review action', exact: true }),
  ).toBeDisabled();
});

test('owner generic flag survives restart and exact Undo restores its prior projection', async ({
  page,
}) => {
  await enableDurableReviewRestartStory(page);
  await page.goto('/');

  let inbox = await enterReviewInbox(page);
  await expect(inbox.getByText('hello world', { exact: true })).toBeVisible();
  const flag = inbox.locator('#inbox-flag');
  await expect(flag).toBeEnabled();
  await flag.click();
  await expect(inbox.getByText('second durable clip', { exact: true })).toBeVisible({
    timeout: 15_000,
  });

  const flagged = await durableReviewBackendSnapshot(page);
  const flaggedPrimary = flagged.segments.find((row) => row.segment.id === 'e2e-segment-1');
  expect(flaggedPrimary).toMatchObject({ revision: 1, segment: { escalated: true } });
  expect(flagged.latestFlag).toMatchObject({
    segmentId: 'e2e-segment-1',
    priorRevision: 0,
    flagRevision: 1,
  });
  expect(flagged.latestFlag?.operationId).toMatch(UUID_PATTERN);
  expect(flagged.latestFlag?.payloadHash).toMatch(/^[0-9a-f]{64}$/);
  expect(flagged.flagOperationCount).toBe(1);
  expect(flagged.committedOperationCount).toBe(0);
  expect(flagged.undoOperationCount).toBe(0);
  if (flagged.undoAvailability.status !== 'available') {
    throw new Error('the process-backed review store did not publish exact flag Undo authority');
  }
  expect(flagged.undoAvailability.target).toMatchObject({
    kind: 'flag',
    effectEventId: flagged.latestFlag?.effectEventId,
    segmentId: 'e2e-segment-1',
    sourceOperationId: flagged.latestFlag?.operationId,
    sourcePayloadHash: flagged.latestFlag?.payloadHash,
    priorRevision: 0,
    flagRevision: 1,
    flagKind: { kind: 'generic' },
    databaseGeneration: 1,
  });

  await page.reload();
  inbox = await enterReviewInbox(page);
  await expect(inbox.getByText('second durable clip', { exact: true })).toBeVisible();
  const undo = inbox.getByRole('button', {
    name: 'Undo last review action',
    exact: true,
  });
  await expect(undo).toBeEnabled();

  const hydrated = await durableReviewBackendSnapshot(page);
  expect(hydrated.availabilityReads).toBeGreaterThan(flagged.availabilityReads);
  expect(hydrated.latestFlag).toEqual(flagged.latestFlag);
  expect(hydrated.flagOperationCount).toBe(1);
  expect(hydrated.undoOperationCount).toBe(0);

  await undo.click();
  await expect(inbox.getByText('Undone', { exact: true })).toBeVisible({ timeout: 15_000 });
  await inbox
    .getByRole('option', { name: 'Segment 1 of 2: e2e-segment-1', exact: true })
    .click();
  await expect(inbox.getByText('hello world', { exact: true })).toBeVisible({ timeout: 15_000 });

  const restored = await durableReviewBackendSnapshot(page);
  const restoredPrimary = restored.segments.find((row) => row.segment.id === 'e2e-segment-1');
  expect(restoredPrimary?.revision).toBe(2);
  expect(restoredPrimary?.segment.escalated).not.toBe(true);
  expect(restored.latestUndo?.operationId).toMatch(UUID_PATTERN);
  expect(restored.latestUndo?.operationId).not.toBe(flagged.latestFlag?.operationId);
  expect(restored.latestUndo?.target).toEqual(flagged.undoAvailability.target);
  expect(restored.undoAvailability).toEqual({
    status: 'blocked',
    reason: 'latestFlagUndone',
  });
  expect(restored.flagOperationCount).toBe(1);
  expect(restored.undoOperationCount).toBe(1);

  await page.reload();
  inbox = await enterReviewInbox(page);
  await expect(inbox.getByText('hello world', { exact: true })).toBeVisible();
  const twiceReloaded = await durableReviewBackendSnapshot(page);
  expect(twiceReloaded.segments).toEqual(restored.segments);
  expect(twiceReloaded.undoAvailability).toEqual(restored.undoAvailability);
  expect(twiceReloaded.availabilityReads).toBeGreaterThan(restored.availabilityReads);
  expect(twiceReloaded.flagOperationCount).toBe(1);
  expect(twiceReloaded.undoOperationCount).toBe(1);
  await expect(
    inbox.getByRole('button', { name: 'Undo last review action', exact: true }),
  ).toBeDisabled();
});
