import { writable } from 'svelte/store';

export const activeOperations = writable<Set<string>>(new Set());

export function startOperation(opName: string): void {
  activeOperations.update((s) => {
    const next = new Set(s);
    next.add(opName);
    return next;
  });
}

export function endOperation(opName: string): void {
  activeOperations.update((s) => {
    const next = new Set(s);
    next.delete(opName);
    return next;
  });
}
