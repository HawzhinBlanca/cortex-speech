import { describe, it, expect, beforeEach } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import { createRawSnippet, flushSync } from 'svelte';
import ErrorBoundary from '../../src/lib/ErrorBoundary.svelte';
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

  it('IGNORES resource-load errors (e.g. a CSP-blocked font link) and keeps the panel', () => {
    render(ErrorBoundary, { props: { children: child } });
    flushSync(); // run the $effect that attaches the window 'error' listener
    // A failed <link>/<img>/<audio> dispatches 'error' on the element; the window capture-phase
    // listener sees it with target = the element, NOT window. The boundary must ignore it.
    const link = document.createElement('link');
    document.body.appendChild(link);
    link.dispatchEvent(new Event('error', { bubbles: false, cancelable: true }));
    flushSync();
    expect(screen.getByTestId('boundary-child')).toBeInTheDocument();
    expect(screen.queryByText('undefined')).not.toBeInTheDocument();
    expect(screen.queryByText('Retry')).not.toBeInTheDocument();
  });

  it('still catches a real uncaught script error (target === window)', () => {
    render(ErrorBoundary, { props: { children: child } });
    flushSync(); // run the $effect that attaches the window 'error' listener
    window.dispatchEvent(new ErrorEvent('error', { message: 'kaboom', error: new Error('kaboom') }));
    flushSync();
    expect(screen.getByText('Retry')).toBeInTheDocument();
    // and it shows a readable message, never the literal "undefined"
    expect(screen.queryByText('undefined')).not.toBeInTheDocument();
  });
});
