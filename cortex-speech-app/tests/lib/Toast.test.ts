import { describe, it, expect, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/svelte';
import Toast from '../../src/lib/Toast.svelte';
import { notifications } from '../../src/lib/stores/notificationStore';
import { locale } from '../../src/lib/i18n';

describe('Toast', () => {
  beforeEach(() => {
    notifications.clear();
    locale.set('en');
  });

  it('renders nothing when there are no notifications', () => {
    const { container } = render(Toast);
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
    const region = container.querySelector('[aria-live="polite"]');
    expect(region).toHaveClass('w-[calc(100%-2rem)]');
    expect(region).not.toHaveClass('w-full');
  });

  it('renders a notification when added to the store', async () => {
    render(Toast);
    notifications.info('Hello, world!');
    await waitFor(() => {
      expect(screen.getByRole('alert')).toBeInTheDocument();
    });
    expect(screen.getByText('Hello, world!')).toBeInTheDocument();
  });

  it('renders multiple notification types', async () => {
    render(Toast);
    notifications.success('Success!');
    notifications.error('Error!');
    notifications.warning('Warning!');
    notifications.info('Info!');
    await waitFor(() => {
      expect(screen.getByText('Success!')).toBeInTheDocument();
    });
    expect(screen.getByText('Error!')).toBeInTheDocument();
    expect(screen.getByText('Warning!')).toBeInTheDocument();
    expect(screen.getByText('Info!')).toBeInTheDocument();
    expect(screen.getAllByRole('alert')).toHaveLength(4);
  });

  it('dismisses a notification when the dismiss button is clicked', async () => {
    render(Toast);
    notifications.info('Dismiss me');
    await waitFor(() => {
      expect(screen.getByText('Dismiss me')).toBeInTheDocument();
    });

    await fireEvent.click(screen.getByLabelText('Dismiss'));
    await waitFor(() => {
      expect(screen.queryByText('Dismiss me')).not.toBeInTheDocument();
    });
  });

  it('dismisses only the specific notification', async () => {
    render(Toast);
    notifications.info('Keep me');
    notifications.info('Remove me');
    await waitFor(() => {
      expect(screen.getAllByRole('alert')).toHaveLength(2);
    });

    const dismissButtons = screen.getAllByLabelText('Dismiss');
    await fireEvent.click(dismissButtons[1]);

    await waitFor(() => {
      expect(screen.queryByText('Remove me')).not.toBeInTheDocument();
    });
    expect(screen.getByText('Keep me')).toBeInTheDocument();
  });

  it('renders detail text when provided', async () => {
    render(Toast);
    notifications.info('Message', { detail: 'Some detail' });
    await waitFor(() => {
      expect(screen.getByText('Some detail')).toBeInTheDocument();
    });
  });

  it('renders only a typed public error reference, never backend prose', async () => {
    render(Toast);
    notifications.error('Save failed', {
      cause: {
        schema: 1,
        code: 'WRITE_FAILED',
        message: 'SQL failed at C:\\private\\library.db',
        retryable: true,
        suggestedAction: 'retry',
        operationId: '018f6e4a-2d71-4c66-8e4b-9d3c4b7e5a10',
      },
    });
    await waitFor(() => expect(screen.getByText(/WRITE_FAILED/)).toBeInTheDocument());
    expect(screen.queryByText(/SQL|private|library\.db/)).not.toBeInTheDocument();
  });
});
