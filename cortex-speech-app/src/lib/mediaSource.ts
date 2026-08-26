import { desktopAssetUrl } from './adapters/desktop';

/** Convert a validated local media path to the desktop webview URL understood by the audio element. */
export function localMediaUrl(path: string): string {
  return desktopAssetUrl(path);
}
