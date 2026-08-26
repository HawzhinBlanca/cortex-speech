import { describe, it, expect, beforeEach, vi } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import { createRawSnippet, flushSync } from 'svelte';
import ErrorBoundary from '../../src/lib/ErrorBoundary.svelte';
import BoundaryHarness from './fixtures/BoundaryHarness.svelte';
import { locale } from '../../src/lib/i18n';

const child = createRawSnippet(() => ({
  render: () => `<div data-testid="boundary-child">panel content</div>`,
}));

describe('ErrorBoundary', () => {
  beforeEach(() => locale.set('en'));

  it('renders its children normally', () => {
    render(ErrorBoundary, { props: { children: child } });
    expect(screen.getByTestId('boundary-child')).toBeInTheDocument();
  });

  /**
   * The whole point of the 2026-08-17 rewrite. Workstation.svelte mounts these boundaries; when each one
   * listened for window 'error' itself, a single uncaught error made all ten render their fallback
   * — the user saw the entire app replaced by red boxes because one panel failed. A boundary must
   * only ever catch what is below it.
   */
  it('catches a render failure in its OWN subtree and leaves siblings untouched', () => {
    // Svelte logs the caught error; that is correct behaviour, not test noise.
    const consoleError = vi.spyOn(console, 'error').mockImplementation(() => {});
    render(BoundaryHarness, { props: { explode: true } });
    flushSync();

    // The failing panel shows the fallback...
    expect(screen.getByText('Retry')).toBeInTheDocument();
    expect(screen.queryByTestId('exploding-panel')).not.toBeInTheDocument();
    // ...and the panel next to it is still on screen, still working.
    expect(screen.getByTestId('healthy-panel')).toBeInTheDocument();
    // Exactly one fallback, not one per boundary.
    expect(screen.getAllByText('Retry')).toHaveLength(1);
    expect(screen.queryByText('undefined')).not.toBeInTheDocument();
    expect(screen.queryByText('panel exploded')).not.toBeInTheDocument();
    consoleError.mockRestore();
  });

  it('does NOT blank a panel for an uncaught window error it cannot attribute', () => {
    render(ErrorBoundary, { props: { children: child } });
    flushSync();
    // A global uncaught error is surfaced as a toast by installGlobalErrorTrap — see
    // globalErrorTrap.test.ts. It must NOT tear down an unrelated panel that is rendering fine.
    // The swallow listener is test plumbing only: without it jsdom runs its default action and
    // reports this deliberate event as an unhandled exception, failing the run for the wrong reason.
    const swallow = (e: Event) => e.preventDefault();
    window.addEventListener('error', swallow);
    window.dispatchEvent(
      new ErrorEvent('error', { message: 'kaboom', error: new Error('kaboom'), cancelable: true }),
    );
    flushSync();
    window.removeEventListener('error', swallow);
    expect(screen.getByTestId('boundary-child')).toBeInTheDocument();
    expect(screen.queryByText('Retry')).not.toBeInTheDocument();
  });
});
