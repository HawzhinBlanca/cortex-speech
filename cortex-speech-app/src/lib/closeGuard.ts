import { currentDesktopWindow, type DesktopUnlisten } from './adapters/desktop';

interface DurableCloseGuardOptions {
  flush: () => Promise<void>;
  timeoutMs: number;
  onFlushError: (error: unknown) => void;
  onCloseError: (error: unknown) => void;
  logError?: (message: string, error: unknown) => void;
}

/**
 * Register the native close boundary without leaking Tauri window authority into the workstation.
 *
 * The first close is intercepted until drafts are durable. A successful flush destroys the window;
 * if destroy is unavailable, the second close request passes through. A failed flush keeps the
 * window open and restores interception, so visible human text is never silently discarded.
 */
export async function registerDurableCloseGuard({
  flush,
  timeoutMs,
  onFlushError,
  onCloseError,
  logError = (message, error) => console.error(message, error),
}: DurableCloseGuardOptions): Promise<DesktopUnlisten> {
  const appWindow = await currentDesktopWindow();
  let closing = false;

  return appWindow.onCloseRequested(async (event) => {
    if (closing) return;
    event.preventDefault();
    closing = true;

    let timeout: ReturnType<typeof setTimeout> | undefined;
    try {
      await Promise.race([
        flush(),
        new Promise<never>((_, reject) => {
          timeout = setTimeout(
            () => reject(new Error('Timed out while making the visible review draft durable')),
            timeoutMs,
          );
        }),
      ]);
    } catch (error) {
      closing = false;
      onFlushError(error);
      return;
    } finally {
      if (timeout) clearTimeout(timeout);
    }

    try {
      await appWindow.destroy();
    } catch (destroyError) {
      logError('window.destroy failed; falling back to close():', destroyError);
      try {
        await appWindow.close();
      } catch (closeError) {
        closing = false;
        logError('window.close fallback also failed:', closeError);
        onCloseError(closeError);
      }
    }
  });
}
