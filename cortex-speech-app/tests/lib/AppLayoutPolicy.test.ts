import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';

const root = resolve(import.meta.dirname, '../..');

describe('App layout integrity', () => {
  it('keeps the visible version synchronized with the package version', () => {
    const pkg = JSON.parse(readFileSync(resolve(root, 'package.json'), 'utf8')) as {
      version: string;
    };
    const app = readFileSync(resolve(root, 'src/App.svelte'), 'utf8');
    expect(app).toContain(`>v${pkg.version}</span`);
  });

  it('does not render the duplicate stats side panel in Insights', () => {
    const app = readFileSync(resolve(root, 'src/App.svelte'), 'utf8');
    expect(app).toContain("{#if $filteredSegments.length > 0 && viewMode !== 'insights'}");
  });

  it('keeps export and library actions reachable after the last review advances past the queue', () => {
    const review = readFileSync(resolve(root, 'src/lib/ReviewMode.svelte'), 'utf8');
    const terminalStart = review.indexOf('{:else if !current}');
    const terminal = review.slice(terminalStart, review.indexOf('{:else}', terminalStart));
    expect(terminal).toContain('data-testid="review-terminal-export"');
    expect(terminal).toContain('data-testid="review-terminal-done"');
  });

  it('routes every review transition through the panel-preserving entry and exit helpers', () => {
    const app = readFileSync(resolve(root, 'src/App.svelte'), 'utf8');
    expect(app).toContain("else if (id === 'review') enterReviewMode();");
    expect(app).toContain("else if (viewMode === 'review') leaveReviewMode");
    expect(app).toContain('<StatsDashboard onOpenReview={enterReviewMode} />');
    expect(app).toContain('<ReviewMode onExport={handleExport} onDone={leaveReviewMode} />');
    expect(app).toContain(
      'snapshot?.sidebarWide === sidebarWide ? snapshot.sidebarOpen : sidebarWide',
    );
    expect(app).toContain('snapshot?.statsWide === statsWide ? snapshot.statsOpen : statsWide');
  });

  it('keeps the browser preview review queue filtered and stateful like the real IPC backend', () => {
    const main = readFileSync(resolve(root, 'src/main.ts'), 'utf8');
    expect(main).toContain(
      "const verified = typeof args?.verified === 'boolean' ? args.verified : null;",
    );
    expect(main).toContain('(verified === null || s.verified === verified)');
    expect(main).toContain("if (cmd === 'update_segment_fields')");
    expect(main).toContain('demoSegments[index] = { ...demoSegments[index], ...fields };');
    expect(main).toContain("if (cmd === 'plugin:event|listen') return args?.handler ?? 0;");
    expect(main).toContain('__TAURI_EVENT_PLUGIN_INTERNALS__');
    expect(main).toContain('if (callbacks) delete callbacks[id];');
  });

  it('renders shortcut labels and categories through the active locale', () => {
    const palette = readFileSync(resolve(root, 'src/lib/CommandPalette.svelte'), 'utf8');
    const help = readFileSync(resolve(root, 'src/lib/KeyboardShortcuts.svelte'), 'utf8');
    expect(palette).toContain('s.descriptionKey ? $t(s.descriptionKey) : s.description');
    expect(palette).toContain('$t(shortcutCategoryKeys[s.category] ?? s.category)');
    expect(help).toContain('s.descriptionKey ? $t(s.descriptionKey) : s.description');
  });
});
