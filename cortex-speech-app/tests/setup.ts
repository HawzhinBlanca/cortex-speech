import { vi } from 'vitest';
import '@testing-library/jest-dom';

// jsdom does not implement the Web Animations API, but Svelte 5's transition
// runtime calls element.animate()/getAnimations() (e.g. Modal's intro/outro).
// Without this, every test that renders an animated component throws
// "element.animate is not a function" and the assertions never run. Provide a
// minimal Animation that completes immediately — fire onfinish on a microtask
// (after Svelte assigns its handler) so outros resolve and nodes leave the DOM.
if (typeof Element !== 'undefined' && !Element.prototype.animate) {
  class MockAnimation {
    currentTime = 0;
    startTime = 0;
    playState = 'finished';
    pending = false;
    onfinish: (() => unknown) | null = null;
    oncancel: (() => unknown) | null = null;
    finished: Promise<MockAnimation>;
    constructor() {
      this.finished = Promise.resolve(this);
      queueMicrotask(() => this.onfinish?.());
    }
    cancel() {
      this.oncancel?.();
    }
    finish() {}
    play() {}
    pause() {}
    reverse() {}
    persist() {}
    commitStyles() {}
    updatePlaybackRate() {}
    addEventListener() {}
    removeEventListener() {}
  }
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  Element.prototype.animate = function (): any {
    return new MockAnimation();
  };
  if (!Element.prototype.getAnimations) {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    Element.prototype.getAnimations = function (): any {
      return [];
    };
  }
}

if (typeof globalThis.ResizeObserver === 'undefined') {
  class MockResizeObserver {
    observe() {}
    unobserve() {}
    disconnect() {}
  }
  Object.defineProperty(globalThis, 'ResizeObserver', {
    writable: true,
    configurable: true,
    value: MockResizeObserver,
  });
}

// Mock Tauri invoke for tests
vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(),
  convertFileSrc: vi.fn((path: string) => `asset://localhost/${path}`),
}));

// Mock Tauri event system
vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(() => Promise.resolve(() => {})),
  emit: vi.fn(() => Promise.resolve()),
}));

// Mock window.matchMedia for theme tests
Object.defineProperty(window, 'matchMedia', {
  writable: true,
  value: vi.fn().mockImplementation((query: string) => ({
    matches: false,
    media: query,
    onchange: null,
    addListener: vi.fn(),
    removeListener: vi.fn(),
    addEventListener: vi.fn(),
    removeEventListener: vi.fn(),
    dispatchEvent: vi.fn(),
  })),
});
