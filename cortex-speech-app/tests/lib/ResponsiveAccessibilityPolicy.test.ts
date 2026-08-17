import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';

const root = resolve(import.meta.dirname, '../..');

describe('320 CSS-pixel reflow policy', () => {
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
    const source = readFileSync(resolve(root, 'src/lib/ReviewInbox.svelte'), 'utf8');
    const narrow = source.slice(source.indexOf('@media (max-width: 480px)'));
    expect(narrow).toMatch(/\.inbox-body\s*{[^}]*flex-direction:\s*column;/s);
    expect(narrow).toMatch(/\.queue-rail\s*{[^}]*width:\s*100%;/s);
    expect(narrow).toMatch(/\.rail-list\s*{[^}]*overflow-x:\s*auto;/s);
    expect(narrow).toMatch(
      /\.autonomy-dial\s*{[^}]*grid-template-columns:\s*repeat\(2, minmax\(0, 1fr\)\);/s,
    );
  });
});
