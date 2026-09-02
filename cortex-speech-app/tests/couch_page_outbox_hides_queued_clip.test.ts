import { readFileSync } from 'fs';
import path from 'path';
import { JSDOM } from 'jsdom';
import { afterEach, describe, expect, it } from 'vitest';

// A clip whose decision is still queued in this phone's outbox (sent, never acknowledged) must not be
// served again with its draft: measured 2026-09-02 on a reviewer's failing link, every save paired
// with a server-side connection error, and the page re-showing the clip read as "my correction came
// back" and invited a second submission.

const PAGE = path.resolve(__dirname, '..', 'src-tauri', 'assets', 'couch.html');
const OP_QUEUED = '10000000-0000-4000-8000-000000000001';

let active: JSDOM | null = null;

function jsonResponse(body: unknown) {
  return {
    ok: true,
    status: 200,
    headers: { get: (name: string) => (name.toLowerCase() === 'content-type' ? 'application/json' : null) },
    json: async () => body,
  };
}

async function pageWithQueue(items: Array<{ id: string }>, seedOutboxFor: string | null) {
  const dom = new JSDOM(readFileSync(PAGE, 'utf-8'), {
    runScripts: 'dangerously',
    url: 'http://127.0.0.1:8737/',
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    beforeParse(win: any) {
      win.fetch = async (input: string) => {
        const url = String(input);
        if (url.endsWith('/api/queue')) {
          return jsonResponse({ playbackContractVersion: 4, reviewer: 'Sara', items, heldByOthers: 0 });
        }
        return jsonResponse({});
      };
      win.HTMLCanvasElement.prototype.getContext = () => ({ clearRect: () => {}, fillRect: () => {}, fillStyle: '' });
      win.HTMLMediaElement.prototype.play = async () => {};
      win.HTMLMediaElement.prototype.pause = () => {};
      win.HTMLMediaElement.prototype.load = () => {};
    },
  });
  active = dom;
  if (seedOutboxFor) {
    dom.window.eval(
      `writeOperationRecord({ operationId: ${JSON.stringify(OP_QUEUED)}, id: ${JSON.stringify(seedOutboxFor)}, ` +
        `action: 'edit', text: 'ڕاستکراوە', reviewer: 'Sara', heardMs: 1500, clipDurationMs: 1500 }, Date.now(), 0)`,
    );
  }
  await dom.window.eval('load()');
  return dom;
}

afterEach(() => {
  active?.window.close();
  active = null;
});

describe('couch page outbox', () => {
  const items = [
    { id: 's1', text: 'دەقی یەکەم', durationMs: 1500, rowVersion: 'r1' },
    { id: 's2', text: 'دەقی دووەم', durationMs: 1500, rowVersion: 'r2' },
  ];

  it('keeps a clip with a queued, unacknowledged decision out of the batch', async () => {
    const dom = await pageWithQueue(items, 's1');
    expect(dom.window.eval('queue.map((s) => s.id)')).toEqual(['s2']);
  });

  it('serves every clip when nothing is queued', async () => {
    const dom = await pageWithQueue(items, null);
    expect(dom.window.eval('queue.map((s) => s.id)')).toEqual(['s1', 's2']);
  });

  it('does not hide a clip queued by a different reviewer on a shared phone', async () => {
    const dom = new JSDOM(readFileSync(PAGE, 'utf-8'), {
      runScripts: 'dangerously',
      url: 'http://127.0.0.1:8737/',
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      beforeParse(win: any) {
        win.fetch = async (input: string) =>
          String(input).endsWith('/api/queue')
            ? jsonResponse({ playbackContractVersion: 4, reviewer: 'Sara', items, heldByOthers: 0 })
            : jsonResponse({});
        win.HTMLCanvasElement.prototype.getContext = () => ({ clearRect: () => {}, fillRect: () => {}, fillStyle: '' });
        win.HTMLMediaElement.prototype.play = async () => {};
        win.HTMLMediaElement.prototype.pause = () => {};
        win.HTMLMediaElement.prototype.load = () => {};
      },
    });
    active = dom;
    dom.window.eval(
      `writeOperationRecord({ operationId: ${JSON.stringify(OP_QUEUED)}, id: 's1', action: 'edit', text: 'x', ` +
        `reviewer: 'Hemn', heardMs: 1500, clipDurationMs: 1500 }, Date.now(), 0)`,
    );
    await dom.window.eval('load()');
    expect(dom.window.eval('queue.map((s) => s.id)')).toEqual(['s1', 's2']);
  });
});
