import { fromStore, get } from 'svelte/store';
import * as api from './commands';
import {
  filterVerified,
  searchQuery,
  segments,
  selectedSegmentId,
  sortOrder,
  type SortOrder,
} from './stores/segmentStore';

const VALID_SORT_ORDERS: SortOrder[] = [
  'newest',
  'oldest',
  'duration',
  'verified',
  'confidence',
  'activeLearning',
];

export function createWorkstationSessionController(
  isTauriAvailable: () => boolean,
  reloadSegments?: () => Promise<void>,
) {
  const query = fromStore(searchQuery);
  const order = fromStore(sortOrder);
  const verifiedFilter = fromStore(filterVerified);
  let restored = false;
  let saveTimeout: ReturnType<typeof setTimeout> | null = null;

  $effect(() => {
    const currentQuery = query.current;
    const currentOrder = order.current;
    const currentFilter = verifiedFilter.current;
    if (!restored || !isTauriAvailable()) return;
    if (saveTimeout) clearTimeout(saveTimeout);
    saveTimeout = setTimeout(() => {
      void api
        .saveSession(currentQuery, currentOrder, currentFilter)
        .catch((error) => console.error('Session save failed:', error));
    }, 800);
  });

  async function restoreAndApply(): Promise<void> {
    if (!isTauriAvailable()) return;
    try {
      const session = await api.restoreSession();
      if (!session) return;
      let filtersChanged = false;
      if (session.search_query && session.search_query !== get(searchQuery)) {
        searchQuery.set(session.search_query);
        filtersChanged = true;
      }
      if (
        session.sort_order &&
        VALID_SORT_ORDERS.includes(session.sort_order as SortOrder) &&
        session.sort_order !== get(sortOrder)
      ) {
        sortOrder.set(session.sort_order as SortOrder);
        filtersChanged = true;
      }
      if (
        session.filter_verified !== null &&
        session.filter_verified !== undefined &&
        session.filter_verified !== get(filterVerified)
      ) {
        filterVerified.set(session.filter_verified);
        filtersChanged = true;
      }
      if (filtersChanged && reloadSegments) {
        await reloadSegments();
      }
      if (
        session.selected_segment_id &&
        get(segments).some((segment) => segment.id === session.selected_segment_id)
      ) {
        selectedSegmentId.set(session.selected_segment_id);
      }
    } catch (error) {
      console.error('Session restore failed:', error);
    } finally {
      restored = true;
    }
  }

  function clearTimer(): void {
    if (saveTimeout) clearTimeout(saveTimeout);
    saveTimeout = null;
  }

  return { clearTimer, restoreAndApply };
}
