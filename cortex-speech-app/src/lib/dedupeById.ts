// Order-preserving dedupe by a string `id` — keeps the FIRST occurrence of each id.
//
// A keyed Svelte `{#each … (item.id)}` throws `each_key_duplicate` (and unmounts the whole subtree)
// the instant two rendered items share a key. A list array can transiently hold the same segment id
// twice — e.g. an optimistic append during import racing the background reload that re-fetches the
// same row — so list components render `dedupeById(items)` instead of the raw array. A duplicate then
// degrades to a single rendered row for one frame instead of crashing the list. Persistent duplicates
// are impossible (speech_segments.id is a PRIMARY KEY); this guards only the in-memory transient.
export function dedupeById<T extends { id: string }>(items: readonly T[]): T[] {
  const seen = new Set<string>();
  const out: T[] = [];
  for (const item of items) {
    if (seen.has(item.id)) continue;
    seen.add(item.id);
    out.push(item);
  }
  return out;
}
