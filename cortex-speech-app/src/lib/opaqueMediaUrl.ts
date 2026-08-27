const GRANT_ID = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
const DEVELOPMENT_WAV_PREFIX = 'data:audio/wav;base64,';

/**
 * Accept only the backend's opaque custom-protocol origin. This is a second boundary behind CSP:
 * even a malformed IPC payload cannot turn the audio element into a file reader or network client.
 */
export function validatedOpaqueMediaUrl(
  raw: string,
  allowDevelopmentData = import.meta.env.DEV,
): string | null {
  if (allowDevelopmentData && raw.startsWith(DEVELOPMENT_WAV_PREFIX)) return raw;
  if (
    !raw.startsWith('http://cortex-media.localhost/') &&
    !raw.startsWith('cortex-media://localhost/')
  ) {
    return null;
  }
  try {
    const url = new URL(raw);
    const windowsOrigin = url.protocol === 'http:' && url.hostname === 'cortex-media.localhost';
    const nativeOrigin = url.protocol === 'cortex-media:' && url.hostname === 'localhost';
    const id = url.pathname.startsWith('/') ? url.pathname.slice(1) : '';
    if (
      (!windowsOrigin && !nativeOrigin) ||
      url.username ||
      url.password ||
      url.port ||
      url.search ||
      url.hash ||
      !GRANT_ID.test(id)
    ) {
      return null;
    }
    return url.toString();
  } catch {
    return null;
  }
}
