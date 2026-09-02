export const REVIEW_OPERATION_TIMEOUT_MS = 15_000;

/** Bound a renderer-side operation even when the native bridge never settles its Promise. */
export function withReviewOperationTimeout<T>(
  promise: Promise<T>,
  code: string,
  timeoutMs = REVIEW_OPERATION_TIMEOUT_MS,
): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    let settled = false;
    const timer = setTimeout(() => {
      if (settled) return;
      settled = true;
      reject(new Error(code));
    }, timeoutMs);
    promise.then(
      (value) => {
        if (settled) return;
        settled = true;
        clearTimeout(timer);
        resolve(value);
      },
      (error: unknown) => {
        if (settled) return;
        settled = true;
        clearTimeout(timer);
        reject(error);
      },
    );
  });
}
