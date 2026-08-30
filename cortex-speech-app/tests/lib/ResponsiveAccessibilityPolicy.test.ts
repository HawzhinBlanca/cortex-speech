import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';

const root = resolve(import.meta.dirname, '../..');

describe('320 CSS-pixel reflow policy', () => {
  it('keeps modal content and confirmation actions reachable at extreme zoom', () => {
    const modal = readFileSync(resolve(root, 'src/lib/Modal.svelte'), 'utf8');
    const confirm = readFileSync(resolve(root, 'src/lib/ConfirmDialog.svelte'), 'utf8');

    expect(modal).toMatch(/modal-backdrop[^"\n]*overflow-y-auto/);
    expect(modal).toContain('flex-wrap items-center justify-end');
    const shortViewport = modal.slice(modal.indexOf('@media (max-height: 360px)'));
    expect(shortViewport).toMatch(/\.modal-dialog\s*{[^}]*max-height:\s*none;[^}]*overflow:\s*visible;/s);
    expect(shortViewport).toMatch(/\.modal-body\s*{[^}]*flex:\s*none;[^}]*overflow:\s*visible;/s);
    expect(confirm.match(/max-w-full whitespace-normal text-center/g)).toHaveLength(3);
  });

  it('keeps the audio timeline shrinkable inside a wrapping toolbar', () => {
    const source = readFileSync(resolve(root, 'src/lib/AudioPlayer.svelte'), 'utf8');
    expect(source).toMatch(
      /class="flex flex-wrap items-center[^"]*"[\s\S]*data-testid="audio-player-controls"/,
    );
    expect(source).toMatch(
      /class="flex min-w-0 flex-1 basis-52[^"]*"[\s\S]*?data-testid="audio-player-timeline"/,
    );
  });

  it('stacks the review queue rail above the focus card without clipping narrow content', () => {
    const owner = readFileSync(resolve(root, 'src/lib/ReviewInboxWorkspace.svelte'), 'utf8');
    const rail = readFileSync(resolve(root, 'src/lib/ReviewInboxQueueRail.svelte'), 'utf8');
    const header = readFileSync(resolve(root, 'src/lib/ReviewInboxHeader.svelte'), 'utf8');
    expect(owner).toContain("import ReviewInboxQueueRail from './ReviewInboxQueueRail.svelte'");
    expect(owner).toContain('<ReviewInboxQueueRail');
    expect(owner.slice(owner.indexOf('@media (max-width: 480px)'))).toMatch(
      /\.inbox-body\s*{[^}]*flex-direction:\s*column;/s,
    );
    const narrowRail = rail.slice(rail.indexOf('@media (max-width: 480px)'));
    expect(narrowRail).toMatch(/\.queue-rail\s*{[^}]*width:\s*100%;/s);
    expect(narrowRail).toMatch(/\.rail-list\s*{[^}]*overflow-x:\s*auto;/s);
    expect(header.slice(header.indexOf('@media (max-width: 480px)'))).toMatch(
      /\.autonomy-dial\s*{[^}]*grid-template-columns:\s*repeat\(2, minmax\(0, 1fr\)\);/s,
    );
  });
});
