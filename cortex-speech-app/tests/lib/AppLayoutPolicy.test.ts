import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';

const root = resolve(import.meta.dirname, '../..');
const workstationSource = () => readFileSync(resolve(root, 'src/Workstation.svelte'), 'utf8');

describe('App layout integrity', () => {
  it('keeps App as a bounded composition root', () => {
    const app = readFileSync(resolve(root, 'src/App.svelte'), 'utf8');
    expect(app.split(/\r?\n/).length).toBeLessThanOrEqual(350);
    expect(app).toContain("import Workstation from './Workstation.svelte';");
    expect(app).toContain('<Workstation />');
    expect(app).not.toContain('@tauri-apps/');
    expect(app).not.toMatch(/\bapi\./);
  });

  it('keeps the visible version synchronized with the package version', () => {
    const pkg = JSON.parse(readFileSync(resolve(root, 'package.json'), 'utf8')) as {
      version: string;
    };
    const workstation = workstationSource();
    const header = readFileSync(resolve(root, 'src/lib/WorkstationHeader.svelte'), 'utf8');
    expect(workstation).toContain("import WorkstationHeader from './lib/WorkstationHeader.svelte'");
    expect(workstation).toContain('<WorkstationHeader');
    expect(header).toContain(`>v${pkg.version}</span`);
  });

  it('does not render the duplicate stats side panel in Insights', () => {
    const app = workstationSource();
    expect(app).toContain("{#if $filteredSegments.length > 0 && view.viewMode !== 'insights'}");
  });

  it('keeps export and library actions reachable after the last review advances past the queue', () => {
    const terminal = readFileSync(resolve(root, 'src/lib/ReviewModeTerminal.svelte'), 'utf8');
    expect(terminal).toContain('data-testid="review-terminal-export"');
    expect(terminal).toContain('data-testid="review-terminal-done"');
  });

  it('routes every review transition through the panel-preserving entry and exit helpers', () => {
    const app = workstationSource();
    const view = readFileSync(resolve(root, 'src/lib/workstationViewController.svelte.ts'), 'utf8');
    const center = readFileSync(resolve(root, 'src/lib/WorkstationCenter.svelte'), 'utf8');
    expect(view).toContain("else if (id === 'review') enterReviewModeForIntent(intent);");
    expect(view).toContain("void leaveReviewModeForIntent(id as 'curate' | 'insights', intent);");
    expect(app).toContain("const loadStatsDashboard = () => import('./lib/StatsDashboard.svelte')");
    expect(app).toContain('{loadStatsDashboard}');
    expect(center).toContain('load={loadStatsDashboard}');
    expect(center).toContain('componentProps={{ onOpenReview: onEnterReview }}');
    expect(app).toContain("const loadReviewMode = () => import('./lib/ReviewMode.svelte')");
    // Passing leaveReviewMode directly would feed the DOM MouseEvent into its `nextView` parameter,
    // assigning an event object to viewMode instead of returning to the library.
    expect(center).toContain('componentProps={{ onExport, onDone: () => void onLeaveReview() }}');
    expect(view).toContain(
      'snapshot?.sidebarWide === sidebarWide ? snapshot.sidebarOpen : sidebarWide',
    );
    expect(view).toContain('snapshot?.statsWide === statsWide ? snapshot.statsOpen : statsWide');
  });

  it('does not pass a DOM event into the optional settings-tab parameter', () => {
    const app = workstationSource();
    expect(app).toContain('onOpenSettings={() => openSettings()}');
    expect(app).not.toContain('onOpenSettings={openSettings}');
  });

  it('does not unmount a review editor until its visible recovery draft is durable', () => {
    const view = readFileSync(resolve(root, 'src/lib/workstationViewController.svelte.ts'), 'utf8');
    const start = view.indexOf('async function leaveReviewModeForIntent(');
    const end = view.indexOf('\n  function leaveReviewMode(', start);
    expect(start).toBeGreaterThanOrEqual(0);
    expect(end).toBeGreaterThan(start);
    const exit = view.slice(start, end);

    const flush = exit.indexOf('await flushReviewDrafts();');
    const unmount = exit.indexOf('viewMode = nextView;');
    expect(flush).toBeGreaterThanOrEqual(0);
    expect(unmount).toBeGreaterThan(flush);
    expect(exit).toContain('catch (error)');
    expect(exit).toContain("notifications.error(translate('review.closeDraftFailed')");
    expect(exit).toContain('intent !== surfaceIntentSequence ||');
    expect(exit).toContain("viewMode !== 'review' ||");
    expect(exit).toContain('sharedDurableReviewUndo.blocksSurfaceTransition()');
  });

  it('keeps the browser preview review queue filtered and stateful like the real IPC backend', () => {
    const main = readFileSync(resolve(root, 'src/main.ts'), 'utf8');
    expect(main).toContain(
      "const verified = typeof args?.verified === 'boolean' ? args.verified : null;",
    );
    expect(main).toContain('(verified === null || s.verified === verified)');
    expect(main).toContain("if (cmd === 'update_segment_metadata_v1')");
    expect(main).toContain("code: 'STALE_SEGMENT_METADATA'");
    expect(main).toContain('demoSegments[index] = next;');
    expect(main).toContain("if (cmd === 'plugin:event|listen') return args?.handler ?? 0;");
    expect(main).toContain('__TAURI_EVENT_PLUGIN_INTERNALS__');
    expect(main).toContain('if (callbacks) delete callbacks[id];');
  });

  it('renders shortcut labels and categories through the active locale', () => {
    const palette = readFileSync(resolve(root, 'src/lib/CommandPalette.svelte'), 'utf8');
    const help = readFileSync(resolve(root, 'src/lib/KeyboardShortcuts.svelte'), 'utf8');
    expect(palette).toContain('s.descriptionKey ? $t(s.descriptionKey) : s.description');
    expect(palette).toContain('function shortcutCategoryLabel(category: string): string');
    expect(palette).toContain('category: shortcutCategoryLabel(s.category)');
    expect(help).toContain('s.descriptionKey ? $t(s.descriptionKey) : s.description');
  });
});
