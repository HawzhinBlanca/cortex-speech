import { test, expect } from '@playwright/test';
import { AxeBuilder } from '@axe-core/playwright';
import { pathToFileURL } from 'node:url';
import { resolve } from 'node:path';

// The phone review page (src-tauri/assets/couch.html) is served straight out of the Rust binary via
// include_str!, so it never passes through Vite and NOTHING in the existing e2e suite touched it —
// axe.spec.ts covers the desktop App root and Settings only. It is also the surface handed to people
// who are not the owner, which makes it the one most in need of an accessibility floor, not the least.
//
// Loaded from the file system rather than a live server: this asserts the page's own markup,
// theming and i18n, which need no backend. The server-side contract (auth, leases, attribution,
// spot checks) is covered by the Rust suite against a real HTTP server in src-tauri/src/couch.rs.
const PAGE = pathToFileURL(resolve(process.cwd(), 'src-tauri/assets/couch.html')).href;

const WCAG_AA = ['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa', 'wcag22aa'];

const OUTBOX_OPERATION_PREFIX = 'cortex.couch.outbox.operation.';

type CouchOutboxSubmission = {
  operationId: string;
  id: string;
  action: string;
  text: string;
  reviewer?: string;
  [key: string]: unknown;
};

/** Read the physical v2 operation records, not the retired aggregate-array compatibility key. */
async function readOperationOutbox(
  page: import('@playwright/test').Page,
): Promise<CouchOutboxSubmission[]> {
  return page.evaluate(`
    (() => Object.keys(localStorage)
      .filter((key) => key.startsWith('${OUTBOX_OPERATION_PREFIX}'))
      .sort()
      .map((key) => {
        const record = JSON.parse(localStorage.getItem(key));
        if (!record || record.version !== 1 || !record.submission) {
          throw new Error('expected a version-1 operation outbox record');
        }
        return record.submission;
      }))()
  `);
}

async function operationOutboxCount(page: import('@playwright/test').Page): Promise<number> {
  return (await readOperationOutbox(page)).length;
}

/** Seed through the page's real writer so fixtures exercise validation, wrapping and readback. */
async function seedOperationOutbox(
  page: import('@playwright/test').Page,
  submissions: CouchOutboxSubmission[],
): Promise<void> {
  await page.evaluate(
    `(${JSON.stringify(submissions)}).forEach((submission) => queueSubmission(submission))`,
  );
}

/** Put the page into its REVIEWING state — the state a reviewer actually spends their session in.
 *
 * Evaluated as a STRING, not an arrow function. The page's state lives in top-level `let` bindings,
 * and `let` at global scope does NOT become a property of `window` — so `window.queue = …` would
 * create a second, unrelated variable that `show()` never reads (it did, and every assertion failed
 * against an empty card). A bare assignment resolves through the scope chain to the real binding. */
async function showAClip(page: import('@playwright/test').Page) {
  await page.evaluate(`
    queue = [
      { id: 's1', text: 'ئەمە دەقێکی نموونەیی کوردییە بۆ پێداچوونەوە', durationMs: 4200, speakerId: 'spk_01', rowVersion: '1', pilotAfterReviewEventId: null },
      { id: 's2', text: 'دەقی دووەم', durationMs: 3100, speakerId: null, rowVersion: '2', pilotAfterReviewEventId: null },
    ];
    i = 0;
    document.getElementById('err').hidden = true;
    show();
  `);
  await expect(page.locator('#card')).toBeVisible();
}

test.describe('Couch Review phone page', () => {
  test.beforeEach(async ({ page }) => {
    // A fresh origin per test: the page persists locale/size/loop in localStorage, and a leaked
    // choice from a previous test would silently change what the next one asserts.
    await page.goto(PAGE);
    await page.evaluate(() => localStorage.clear());
    await page.goto(PAGE);
  });

  test('opens in Sorani RTL — a Kurdish-first app must not greet its reviewer in English', async ({
    page,
  }) => {
    await expect(page.locator('html')).toHaveAttribute('lang', 'ckb');
    await expect(page.locator('html')).toHaveAttribute('dir', 'rtl');
    await showAClip(page);
    await expect(page.locator('#accept')).toContainText('دروستە');
    await expect(page.locator('#save')).toHaveText('پاشەکەوت و دواتر');
    await expect(page.locator('#bad')).toContainText('ڕەتکردنەوە');
  });

  test('the language toggle flips the UI but never the transcript direction', async ({ page }) => {
    await showAClip(page);
    await page.locator('#lang').click();
    await expect(page.locator('html')).toHaveAttribute('dir', 'ltr');
    await expect(page.locator('#save')).toHaveText('Save & next');
    // The corpus is Sorani whatever language the chrome is in. An LTR transcript box would put the
    // caret on the wrong side of every correction the reviewer types.
    await expect(page.locator('#text')).toHaveCSS('direction', 'rtl');
  });

  test('a typed correction survives a reload', async ({ page }) => {
    await showAClip(page);
    await page.locator('#text').fill('ڕاستکراوەی من');
    await page.reload();
    await showAClip(page);
    await expect(page.locator('#text')).toHaveValue('ڕاستکراوەی من');
  });

  test('the empty state is actually empty — `hidden` is not defeated by a display rule', async ({
    page,
  }) => {
    // Regression: #card sets display:flex, which beats the hidden attribute's display:none default,
    // so the empty review card rendered next to the "all reviewed" message.
    // `exhausted = true` states what this test is actually about: the TRUE empty state. Without it,
    // show() now (correctly) tries a refill, whose fetch fails on file:// — and the failure path takes
    // down the "Loading clips…" placeholder, which is the dead-end fix, not a regression. This test's
    // subject is the CSS rule, so it must reach the empty state directly instead of relying on a
    // placeholder that used to sit there forever.
    await page.evaluate(
      `queue = []; i = 0; exhausted = true; document.getElementById('err').hidden = true; show();`,
    );
    await expect(page.locator('#card')).toBeHidden();
    await expect(page.locator('#done')).toBeVisible();
  });

  test('the transcript text size is adjustable and persists', async ({ page }) => {
    await showAClip(page);
    const size = () => page.locator('#text').evaluate((el) => getComputedStyle(el).fontSize);
    expect(await size()).toBe('18px');
    await page.locator('#textsize').click();
    const bigger = await size();
    expect(parseInt(bigger, 10)).toBeGreaterThan(18);
    await page.reload();
    await showAClip(page);
    expect(await size()).toBe(bigger);
  });

  test('a dropped submit goes to the outbox and is replayed on the next load', async ({ page }) => {
    // THE DATA-LOSS PATH, and it had no test. A phone at the edge of Wi-Fi loses requests; the outbox
    // is what turns "your correction is gone" into "it lands when you reconnect". If this silently
    // broke, a reviewer would keep working and their decisions would evaporate one by one — the exact
    // failure that is invisible until the corpus is short and nobody knows why.
    //
    // fetch is stubbed rather than routed: page.route cannot intercept a file:// page, and the point
    // here is the PAGE's behaviour when the network fails, not the server's.
    await page.addInitScript(() => {
      (window as unknown as { __net: { fail: boolean; calls: string[] } }).__net = {
        fail: false,
        calls: [],
      };
      window.fetch = async (input: RequestInfo | URL, init?: RequestInit) => {
        const net = (window as unknown as { __net: { fail: boolean; calls: string[] } }).__net;
        const url = String(input);
        net.calls.push(url + ' ' + String(init?.body ?? ''));
        // A dropped request is a TypeError from fetch — no status — which is exactly how the page
        // tells "never arrived" (retry) from "the server refused" (do not retry).
        if (net.fail) throw new TypeError('Failed to fetch');
        return new Response('{"ok":true}', {
          status: 200,
          headers: { 'content-type': 'application/json' },
        });
      };
    });
    await page.goto(PAGE);
    await showAClip(page);

    // The network drops, then the reviewer saves.
    await page.evaluate(`window.__net.fail = true`);
    await page.locator('#accept').click();

    // Their decision is HELD, not lost — and they are moved on rather than stranded on a dead clip.
    const queued = await readOperationOutbox(page);
    expect(queued).toHaveLength(1);
    expect(queued[0]).toMatchObject({ id: 's1', action: 'accept' });
    expect(queued[0].operationId).toMatch(
      /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/,
    );
    await expect
      .poll(async () => page.evaluate(`localStorage.getItem('cortex.couch.outbox')`))
      .toBeNull();
    await expect(page.locator('#text')).toHaveValue('دەقی دووەم', { timeout: 5000 });

    // Network returns; the next load flushes it. Replay is safe only because the SERVER answers an
    // identical re-submit as already-recorded (couch.rs is_repeat_of_stored_decision).
    await page.evaluate(`window.__net.fail = false`);
    await page.reload();
    await expect.poll(async () => operationOutboxCount(page), { timeout: 5000 }).toBe(0);
    const calls = (await page.evaluate(`window.__net.calls`)) as string[];
    expect(calls.some((c) => c.includes('/api/decision') && c.includes('s1'))).toBe(true);
  });

  test('outbox readback uses the same undefined-property semantics as JSON storage and wire', async ({
    page,
  }) => {
    const operationId = '00000000-0000-4000-8000-000000000001';
    await page.evaluate(`queueSubmission({
      operationId: '${operationId}',
      id: 'json-shape',
      action: 'skip',
      text: 'پارچە',
      reviewer: 'Sara',
      rowVersion: '1',
      pilotAfterReviewEventId: undefined,
      heardMs: 0,
      clipDurationMs: 1000,
    })`);

    const queued = await readOperationOutbox(page);
    expect(queued).toHaveLength(1);
    expect(queued[0]).toMatchObject({ operationId, id: 'json-shape', action: 'skip' });
    expect(queued[0]).not.toHaveProperty('pilotAfterReviewEventId');
  });

  test('a submit the server REFUSES is dropped, not retried forever', async ({ page }) => {
    // The mirror image, and just as important: a 409 (someone else took the clip) or a 400 is a real
    // ANSWER. Queueing it would wedge the outbox behind a decision that can never land, and every
    // later decision would be stuck behind it.
    await page.addInitScript(() => {
      window.fetch = async (input: RequestInfo | URL) => {
        if (String(input).includes('/api/decision'))
          return new Response('another reviewer', { status: 409 });
        return new Response('{"ok":true}', {
          status: 200,
          headers: { 'content-type': 'application/json' },
        });
      };
    });
    await page.goto(PAGE);
    await showAClip(page);
    await page.locator('#accept').click();
    await expect.poll(async () => operationOutboxCount(page)).toBe(0);
  });

  test('draining a batch fetches the next one instead of claiming the corpus is finished', async ({
    page,
  }) => {
    // THE THROUGHPUT LIE. The server hands out at most QUEUE_BATCH (25) clips per fetch and says
    // nothing about the backlog behind them, but the page called load() in exactly two places — on
    // open and after an undo — so when the local array ran out it went straight to
    // "🎉 All clips reviewed!". Measured against the owner's own library: 116 pending clips, so a
    // reviewer did 25 and was told the corpus was done with 91 left. The installed PWA is
    // display:standalone, so there is no address bar to reload from either — the lie was also a dead
    // end. The server comment claimed "the page refetches as it drains"; it did not.
    await page.addInitScript(() => {
      (window as unknown as { __q: { fetches: number } }).__q = { fetches: 0 };
      const batches = [
        [
          {
            id: 'a1',
            text: 'یەکەم',
            durationMs: 1000,
            speakerId: null,
            rowVersion: '1',
            pilotAfterReviewEventId: null,
          },
        ],
        [
          {
            id: 'b1',
            text: 'دووەم',
            durationMs: 1000,
            speakerId: null,
            rowVersion: '2',
            pilotAfterReviewEventId: null,
          },
        ],
        [],
      ];
      window.fetch = async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url.includes('/api/queue')) {
          const st = (window as unknown as { __q: { fetches: number } }).__q;
          const items = batches[Math.min(st.fetches, batches.length - 1)];
          st.fetches += 1;
          return new Response(JSON.stringify({ reviewer: 'Sara', items, heldByOthers: 0 }), {
            status: 200,
            headers: { 'content-type': 'application/json' },
          });
        }
        return new Response('{"ok":true}', {
          status: 200,
          headers: { 'content-type': 'application/json' },
        });
      };
    });
    await page.goto(PAGE);
    await expect(page.locator('#text')).toHaveValue('یەکەم', { timeout: 5000 });

    // Draining batch 1 must go BACK to the server, not declare victory.
    await page.locator('#accept').click();
    await expect(page.locator('#text')).toHaveValue('دووەم', { timeout: 5000 });
    await expect(page.locator('#done')).toBeHidden();

    // Only a fetch that genuinely comes back empty may draw the finished state.
    await page.locator('#accept').click();
    await expect(page.locator('#done')).toBeVisible({ timeout: 5000 });
    await expect(page.locator('#done')).toContainText('هەموو پارچەکان');
    expect(await page.evaluate(`window.__q.fetches`)).toBe(3);

    // ...and it must STOP there. An empty answer that still triggered a refill would spin the phone's
    // radio forever on a drained corpus, which on a battery is worse than the bug it replaced.
    await page.waitForTimeout(300);
    expect(await page.evaluate(`window.__q.fetches`)).toBe(3);
  });

  test('a refused replay keeps the typed correction and says so, instead of deleting it in silence', async ({
    page,
  }) => {
    // THE QUIETEST DATA LOSS IN THE SYSTEM. Offline, the page queued the decision and toasted
    // "Saved". Back online, the server answered 409 (someone else got the clip, or the owner decided
    // it at the desktop) — and the page deleted BOTH the queued decision and the reviewer's typed
    // Sorani correction, with no toast, no banner, no counter. They had already been told it was
    // safe, so they had no reason to look. Dropping the queued decision is right; retrying a 409
    // cannot change it. Destroying the text and saying nothing is not.
    await page.addInitScript(() => {
      (window as unknown as { __net: { offline: boolean } }).__net = { offline: true };
      window.fetch = async (input: RequestInfo | URL) => {
        const url = String(input);
        const net = (window as unknown as { __net: { offline: boolean } }).__net;
        if (url.includes('/api/queue')) {
          return new Response(
            JSON.stringify({
              reviewer: 'Sara',
              items: [
                {
                  id: 'x1',
                  text: 'دەقی سەرەتایی',
                  durationMs: 1000,
                  speakerId: null,
                  rowVersion: '1',
                  pilotAfterReviewEventId: null,
                },
              ],
              heldByOthers: 0,
            }),
            { status: 200, headers: { 'content-type': 'application/json' } },
          );
        }
        if (url.includes('/api/decision')) {
          if (net.offline) throw new TypeError('Failed to fetch');
          return new Response('already reviewed by Hemn', { status: 409 });
        }
        return new Response('{"ok":true}', {
          status: 200,
          headers: { 'content-type': 'application/json' },
        });
      };
    });
    await page.goto(PAGE);
    await expect(page.locator('#text')).toHaveValue('دەقی سەرەتایی', { timeout: 5000 });

    // Offline: type a correction and save. It is QUEUED, and must not be called "Saved".
    await page.locator('#text').fill('ڕاستکراوەی سارا');
    await page.locator('#save').click();
    await expect.poll(async () => operationOutboxCount(page)).toBe(1);
    await expect(page.locator('#toast')).toHaveText(
      'لە ڕیزدایە — کاتێک گەڕایتەوە سەر ئینتەرنێت دەنێردرێت',
    );
    const [queued] = await readOperationOutbox(page);
    expect(queued.reviewer).toBe('Sara'); // stamped, so it can never be replayed under another name
    expect(queued.text).toBe('ڕاستکراوەی سارا');
    expect(queued.operationId).toMatch(
      /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/,
    );

    // Back online, the server refuses it. Driven by the 'online' event rather than a reload:
    // addInitScript re-runs on every navigation and would reset __net.offline back to true.
    await page.evaluate(`window.__net.offline = false; dispatchEvent(new Event('online'))`);
    await expect.poll(async () => operationOutboxCount(page)).toBe(0);

    // The decision is dropped — correct — but the WORK is kept and the reviewer is told.
    expect(await page.evaluate(`sessionStorage.getItem('cortex.couch.draft.x1')`)).toBe(
      'ڕاستکراوەی سارا',
    );
    await expect(page.locator('#err')).toBeVisible();
    await expect(page.locator('#err')).toContainText('1');
  });

  test('a decision queued by one reviewer is never flushed under another reviewer name', async ({
    page,
  }) => {
    // localStorage is per-ORIGIN, not per-reviewer. Two people sharing a phone, or one person opening
    // a colleague's link, share one outbox — so an unstamped decision flushed under whoever's cookie
    // is current would record Sara's judgement of a clip as Hemn's, permanently and invisibly.
    await page.addInitScript(() => {
      (window as unknown as { __sent: string[] }).__sent = [];
      window.fetch = async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = String(input);
        if (url.includes('/api/decision')) {
          (window as unknown as { __sent: string[] }).__sent.push(String(init?.body ?? ''));
          return new Response('{"ok":true}', {
            status: 200,
            headers: { 'content-type': 'application/json' },
          });
        }
        if (url.includes('/api/queue')) {
          return new Response(JSON.stringify({ reviewer: 'Hemn', items: [], heldByOthers: 0 }), {
            status: 200,
            headers: { 'content-type': 'application/json' },
          });
        }
        return new Response('{"ok":true}', {
          status: 200,
          headers: { 'content-type': 'application/json' },
        });
      };
    });
    await page.goto(PAGE);
    // Wait until the page knows who it is — the guard is meaningless before /api/queue answers.
    await expect(page.locator('#who')).toHaveText(' · Hemn');
    // Seeded AFTER load so addInitScript cannot wipe it, and flushed via the 'online' event, which is
    // exactly how a reconnecting phone triggers it.
    await seedOperationOutbox(page, [
      {
        operationId: '00000000-0000-4000-8000-000000000011',
        id: 'sara1',
        action: 'edit',
        text: 'هی سارا',
        reviewer: 'Sara',
        rowVersion: '1',
        pilotAfterReviewEventId: null,
        heardMs: 1000,
        clipDurationMs: 1000,
      },
      {
        operationId: '00000000-0000-4000-8000-000000000012',
        id: 'hemn1',
        action: 'edit',
        text: 'هی هێمن',
        reviewer: 'Hemn',
        rowVersion: '2',
        pilotAfterReviewEventId: null,
        heardMs: 1000,
        clipDurationMs: 1000,
      },
    ]);
    await page.evaluate(`dispatchEvent(new Event('online'))`);
    await expect.poll(async () => page.evaluate(`window.__sent.length`)).toBe(1);

    const sent = (await page.evaluate(`window.__sent`)) as string[];
    expect(sent[0]).toContain('hemn1');
    expect(sent.join(' ')).not.toContain('sara1');
    // Sara's decision is still waiting for Sara — held, not sent, and not thrown away either.
    const left = await readOperationOutbox(page);
    expect(left.map((q) => q.id)).toEqual(['sara1']);
  });

  test('the drained state is honest: undo reachable, audio stopped, count not off by one', async ({
    page,
  }) => {
    // Four separate defects that all only appear once the batch runs out — the moment the reviewer is
    // least likely to be watching closely, and most likely to close the page.
    await page.addInitScript(() => {
      (window as unknown as { __q: { n: number } }).__q = { n: 0 };
      window.fetch = async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url.includes('/api/queue')) {
          const st = (window as unknown as { __q: { n: number } }).__q;
          const items =
            st.n === 0
              ? [
                  {
                    id: 'z1',
                    text: 'تاکە پارچە',
                    durationMs: 1000,
                    speakerId: null,
                    rowVersion: '1',
                    pilotAfterReviewEventId: null,
                  },
                ]
              : [];
          st.n += 1;
          return new Response(JSON.stringify({ reviewer: 'Sara', items, heldByOthers: 0 }), {
            status: 200,
            headers: { 'content-type': 'application/json' },
          });
        }
        return new Response('{"ok":true}', {
          status: 200,
          headers: { 'content-type': 'application/json' },
        });
      };
    });
    await page.goto(PAGE);
    await expect(page.locator('#text')).toHaveValue('تاکە پارچە', { timeout: 5000 });
    // Header must never read "Clip 2 of 1".
    // The clip's own length rides beside the position (owner ask 2026-08-17), from the same
    // durationMs the fixture serves — so the exact-match keeps guarding the count against off-by-one.
    await expect(page.locator('#progress')).toHaveText('پارچەی 1 لە 1 (1s)');

    await page.locator('#accept').click();
    await expect(page.locator('#done')).toBeVisible({ timeout: 5000 });

    // UNDO must still be reachable. It used to live inside #card, so it vanished exactly when the
    // reviewer wanted to take back the decision they had just made.
    await expect(page.locator('#undo')).toBeVisible();
    // The <audio> element must not still be playing behind the empty state.
    expect(await page.evaluate(`document.getElementById('player').paused`)).toBe(true);
    // And the progress counter must not have run past the end of the batch.
    expect(await page.evaluate(`document.getElementById('progress').textContent`)).not.toContain(
      '2',
    );
  });

  test('"all reviewed" is never shown while decisions are still unsent', async ({ page }) => {
    // The empty state outranked the outbox: a reviewer who went offline, decided their last clips and
    // saw "🎉 All clips reviewed!" was being told they were finished at the precise moment closing the
    // page would have cost them every one of those decisions.
    await page.addInitScript(() => {
      (window as unknown as { __q: { n: number } }).__q = { n: 0 };
      window.fetch = async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url.includes('/api/queue')) {
          const st = (window as unknown as { __q: { n: number } }).__q;
          const items =
            st.n === 0
              ? [
                  {
                    id: 'w1',
                    text: 'کۆتا پارچە',
                    durationMs: 1000,
                    speakerId: null,
                    rowVersion: '1',
                    pilotAfterReviewEventId: null,
                  },
                ]
              : [];
          st.n += 1;
          return new Response(JSON.stringify({ reviewer: 'Sara', items, heldByOthers: 0 }), {
            status: 200,
            headers: { 'content-type': 'application/json' },
          });
        }
        if (url.includes('/api/decision')) throw new TypeError('Failed to fetch'); // offline
        return new Response('{"ok":true}', {
          status: 200,
          headers: { 'content-type': 'application/json' },
        });
      };
    });
    await page.goto(PAGE);
    await expect(page.locator('#text')).toHaveValue('کۆتا پارچە', { timeout: 5000 });
    await page.locator('#accept').click();

    await expect(page.locator('#done')).toBeVisible({ timeout: 5000 });
    await expect(page.locator('#done')).not.toContainText('هەموو پارچەکان');
    await expect(page.locator('#done')).toContainText('نەنێردراون'); // "have not been sent"
  });

  test('a clip whose audio will not load offers a skip instead of forcing a false verdict', async ({
    page,
  }) => {
    // The trap: audio 500s (file moved off disk), #player has no error handler, loadWave swallows the
    // body, and show() hides #warn on every clip — so the reviewer saw a normal card with a dead
    // player and no explanation. The queue only advances on a DECISION, so the two ways out both
    // write a verdict nobody can justify: accept promotes an unheard ASR draft to gold, reject
    // permanently excludes a clip that may be perfectly fine.
    await page.addInitScript(() => {
      (window as unknown as { __sent: string[] }).__sent = [];
      window.fetch = async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = String(input);
        if (url.includes('/api/queue')) {
          return new Response(
            JSON.stringify({
              reviewer: 'Sara',
              items: [
                {
                  id: 'broken',
                  text: 'پارچەی تێکچوو',
                  durationMs: 1000,
                  speakerId: null,
                  rowVersion: '1',
                  pilotAfterReviewEventId: null,
                },
                {
                  id: 'fine',
                  text: 'پارچەی باش',
                  durationMs: 1000,
                  speakerId: null,
                  rowVersion: '2',
                  pilotAfterReviewEventId: null,
                },
              ],
              heldByOthers: 0,
            }),
            { status: 200, headers: { 'content-type': 'application/json' } },
          );
        }
        if (url.includes('/api/decision')) {
          (window as unknown as { __sent: string[] }).__sent.push(String(init?.body ?? ''));
        }
        return new Response('{"ok":true}', {
          status: 200,
          headers: { 'content-type': 'application/json' },
        });
      };
    });
    await page.goto(PAGE);
    await expect(page.locator('#text')).toHaveValue('پارچەی تێکچوو', { timeout: 5000 });
    // R4.4: the exit is ALWAYS on screen, not revealed only by a failure. Broken audio was just the
    // most obvious way to meet a clip you cannot judge — two people talking over each other, or an
    // accent you do not have, leaves a reviewer equally stuck with no honest way forward.
    await expect(page.locator('#skip')).toBeVisible();

    // The <audio> element reports a load failure (a 500 body into src does exactly this).
    await page.evaluate(`
      const p = document.getElementById('player');
      p.src = 'data:audio/wav;base64,QUJD';
      p.dispatchEvent(new Event('error'));
    `);
    await expect(page.locator('#warn')).toBeVisible();
    await expect(page.locator('#skip')).toBeVisible();

    await page.locator('#skip').click();
    // Moved on to the next clip...
    await expect(page.locator('#text')).toHaveValue('پارچەی باش');
    // ...and the only thing said about the broken one is that nobody judged it. The skip DOES reach the
    // server now — that is what releases the lease, keeps the clip out of this reviewer's next batch,
    // and records that a human met it and could not call it — but it must be a skip and nothing else.
    // Stricter than the old "nothing was sent": that would now pass for a request this test never saw.
    // (The server writing nothing is asserted where it can be: the couch unit test, and the real-HTTP
    // leg in e2e_real_app.cjs that diffs six columns through the app's own IPC.)
    const sent = (await page.evaluate(`window.__sent`)) as string[];
    const aboutBroken = sent.filter((s) => s.includes('broken'));
    expect(aboutBroken).toHaveLength(1);
    expect(JSON.parse(aboutBroken[0]).action).toBe('skip');
  });

  test('a slow first load says so instead of showing a blank page', async ({ page }) => {
    // Both panels start hidden, so until /api/queue resolved the reviewer saw nothing at all — and on
    // a phone on weak signal, which is the normal case for this page, a blank screen is
    // indistinguishable from a dead link. The most likely moment to give up is the first one.
    await page.addInitScript(() => {
      window.fetch = async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url.includes('/api/queue')) {
          await new Promise((r) => setTimeout(r, 1500)); // a slow round trip
          return new Response(
            JSON.stringify({
              reviewer: 'Sara',
              items: [
                {
                  id: 'p1',
                  text: 'دەق',
                  durationMs: 1000,
                  speakerId: null,
                  rowVersion: '1',
                  pilotAfterReviewEventId: null,
                },
              ],
              heldByOthers: 0,
            }),
            { status: 200, headers: { 'content-type': 'application/json' } },
          );
        }
        return new Response('{"ok":true}', {
          status: 200,
          headers: { 'content-type': 'application/json' },
        });
      };
    });
    await page.goto(PAGE);
    // While the fetch is still in flight the reviewer is told what is happening.
    await expect(page.locator('#done')).toBeVisible();
    await expect(page.locator('#done')).toContainText('بارکردنی');
    // ...and it gives way to the clip once it lands.
    await expect(page.locator('#text')).toHaveValue('دەق', { timeout: 8000 });
    await expect(page.locator('#done')).toBeHidden();
  });

  test('a throttled decision is held for retry, not thrown away', async ({ page }) => {
    // 429 means LATER, never NO. The couch limiter is 120/min per reviewer with a 60 burst, and a
    // reviewer moving fast through obvious clips spends three requests each (audio, prefetch,
    // decision) — so a real session can reach it, and the 130-clip drain soak hit it at clip 73.
    // The page treated 429 as a permanent verdict: the decision was dropped and the reviewer was
    // left stranded on the clip, at exactly the moment they were working fastest.
    await page.addInitScript(() => {
      (window as unknown as { __throttle: boolean }).__throttle = true;
      window.fetch = async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url.includes('/api/queue')) {
          return new Response(
            JSON.stringify({
              reviewer: 'Sara',
              items: [
                {
                  id: 't1',
                  text: 'یەکەم',
                  durationMs: 1000,
                  speakerId: null,
                  rowVersion: '1',
                  pilotAfterReviewEventId: null,
                },
                {
                  id: 't2',
                  text: 'دووەم',
                  durationMs: 1000,
                  speakerId: null,
                  rowVersion: '2',
                  pilotAfterReviewEventId: null,
                },
              ],
              heldByOthers: 0,
            }),
            { status: 200, headers: { 'content-type': 'application/json' } },
          );
        }
        if (
          url.includes('/api/decision') &&
          (window as unknown as { __throttle: boolean }).__throttle
        ) {
          return new Response('rate limit exceeded', { status: 429 });
        }
        return new Response('{"ok":true}', {
          status: 200,
          headers: { 'content-type': 'application/json' },
        });
      };
    });
    await page.goto(PAGE);
    await expect(page.locator('#text')).toHaveValue('یەکەم', { timeout: 5000 });

    await page.locator('#accept').click();
    // HELD in the outbox, and the reviewer is moved on rather than stranded re-submitting.
    await expect.poll(async () => operationOutboxCount(page)).toBe(1);
    await expect(page.locator('#text')).toHaveValue('دووەم');
    // Not reported as saved — it is queued, which is the truth.
    await expect(page.locator('#toast')).toHaveText(
      'لە ڕیزدایە — کاتێک گەڕایتەوە سەر ئینتەرنێت دەنێردرێت',
    );
    // And NOT counted as a refused decision: throttling is not a verdict.
    expect(await page.evaluate(`localStorage.getItem('cortex.couch.refused')`)).toBeNull();

    // The limiter refills; the held decision lands on the next flush.
    await page.evaluate(`window.__throttle = false; dispatchEvent(new Event('online'))`);
    await expect.poll(async () => operationOutboxCount(page)).toBe(0);
    await expect(page.locator('#err')).toBeHidden();
  });

  test('the refused-decisions banner is retracted once that clip is re-reviewed', async ({
    page,
  }) => {
    // The banner had no way down. noteRefused only ever appended, so one 409 pinned it for the life of
    // the browser profile — still insisting work had failed after the reviewer went back and redid it.
    // That is the same lie the banner exists to prevent, aimed the other way.
    await page.addInitScript(() => {
      (window as unknown as { __refuse: boolean }).__refuse = true;
      window.fetch = async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url.includes('/api/queue')) {
          return new Response(
            JSON.stringify({
              reviewer: 'Sara',
              items: [
                {
                  id: 'g1',
                  text: 'پارچە',
                  durationMs: 1000,
                  speakerId: null,
                  rowVersion: '1',
                  pilotAfterReviewEventId: null,
                },
              ],
              heldByOthers: 0,
            }),
            { status: 200, headers: { 'content-type': 'application/json' } },
          );
        }
        if (url.includes('/api/decision')) {
          return (window as unknown as { __refuse: boolean }).__refuse
            ? new Response('already reviewed by Hemn', { status: 409 })
            : new Response('{"ok":true}', {
                status: 200,
                headers: { 'content-type': 'application/json' },
              });
        }
        return new Response('{"ok":true}', {
          status: 200,
          headers: { 'content-type': 'application/json' },
        });
      };
    });
    await page.goto(PAGE);
    await expect(page.locator('#text')).toHaveValue('پارچە', { timeout: 5000 });

    // Seed a refusal through the real flush path, then confirm the banner is up.
    await seedOperationOutbox(page, [
      {
        operationId: '00000000-0000-4000-8000-000000000021',
        id: 'g1',
        action: 'edit',
        text: 'x',
        reviewer: 'Sara',
        rowVersion: '1',
        pilotAfterReviewEventId: null,
        heardMs: 1000,
        clipDurationMs: 1000,
      },
    ]);
    await page.evaluate(`dispatchEvent(new Event('online'))`);
    await expect(page.locator('#err')).toBeVisible();
    await expect(page.locator('#err')).toContainText('1 بڕیار');

    // The reviewer goes back and re-reviews that same clip, and this time it lands.
    await page.evaluate(`window.__refuse = false`);
    await page.locator('#accept').click();

    await expect
      .poll(async () => page.evaluate(`localStorage.getItem('cortex.couch.refused')`))
      .toBeNull();
    await expect(page.locator('#err')).toBeHidden();
  });

  test('retracting a refusal must not swallow the link-expired notice', async ({ page }) => {
    // #err is shared, and link-expired is the more urgent message: it needs an action from the reviewer
    // (ask for a new link). Clearing a refusal must never take that banner down with it.
    await page.addInitScript(() => {
      window.fetch = async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url.includes('/api/queue')) {
          return new Response(
            JSON.stringify({
              reviewer: 'Sara',
              items: [
                {
                  id: 'h1',
                  text: 'پارچە',
                  durationMs: 1000,
                  speakerId: null,
                  rowVersion: '1',
                  pilotAfterReviewEventId: null,
                },
              ],
              heldByOthers: 0,
            }),
            { status: 200, headers: { 'content-type': 'application/json' } },
          );
        }
        return new Response('{"ok":true}', {
          status: 200,
          headers: { 'content-type': 'application/json' },
        });
      };
    });
    await page.goto(PAGE);
    await expect(page.locator('#text')).toHaveValue('پارچە', { timeout: 5000 });

    // A stale refusal on record, while the URGENT notice is what is actually on screen.
    await page.evaluate(`
      localStorage.setItem('cortex.couch.refused', JSON.stringify(['h1']));
      document.getElementById('err').hidden = false;
      document.getElementById('err').textContent = STRINGS[locale].linkExpired;
    `);
    await page.locator('#accept').click();

    // The refusal is retracted from storage, but the link notice stays up.
    await expect
      .poll(async () => page.evaluate(`localStorage.getItem('cortex.couch.refused')`))
      .toBeNull();
    await expect(page.locator('#err')).toBeVisible();
    await expect(page.locator('#err')).toContainText('بەسەرچووە');
  });

  test('held work is retried on a clock, with no reload and no online event', async ({ page }) => {
    // A gap created by making 429 a HOLD: flushOutbox only ran on the `online` event and inside
    // load(), and throttling never fires `online` — the phone was never offline. So a rate-limited
    // decision sat in localStorage until the batch happened to drain, up to a whole batch later, while
    // the reviewer watched a "not sent yet" counter. Work was not lost, but "queued" must mean it goes.
    //
    // Driven with fake timers rather than a real 30s wait: the assertion is that a TIMER exists and
    // drains the outbox unaided — not how long a stopwatch takes.
    await page.clock.install();
    await page.addInitScript(() => {
      (window as unknown as { __throttle: boolean; __sent: string[] }).__throttle = true;
      (window as unknown as { __sent: string[] }).__sent = [];
      window.fetch = async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = String(input);
        const w = window as unknown as { __throttle: boolean; __sent: string[] };
        if (url.includes('/api/queue')) {
          return new Response(
            JSON.stringify({
              reviewer: 'Sara',
              items: [
                {
                  id: 'k1',
                  text: 'پارچە',
                  durationMs: 1000,
                  speakerId: null,
                  rowVersion: '1',
                  pilotAfterReviewEventId: null,
                },
              ],
              heldByOthers: 0,
            }),
            { status: 200, headers: { 'content-type': 'application/json' } },
          );
        }
        if (url.includes('/api/decision')) {
          if (w.__throttle) return new Response('rate limit exceeded', { status: 429 });
          w.__sent.push(String(init?.body ?? ''));
          return new Response('{"ok":true}', {
            status: 200,
            headers: { 'content-type': 'application/json' },
          });
        }
        return new Response('{"ok":true}', {
          status: 200,
          headers: { 'content-type': 'application/json' },
        });
      };
    });
    await page.goto(PAGE);
    await expect(page.locator('#text')).toHaveValue('پارچە', { timeout: 5000 });

    await page.locator('#accept').click();
    await expect.poll(async () => operationOutboxCount(page)).toBe(1);

    // The throttle clears. NOTHING else happens: no reload, no online event, no batch drain.
    await page.evaluate(`window.__throttle = false`);
    await page.clock.runFor(31_000);

    await expect.poll(async () => operationOutboxCount(page)).toBe(0);
    const sent = (await page.evaluate(`window.__sent`)) as string[];
    expect(sent.join(' ')).toContain('k1');
  });

  test('overlapping flushes never double-post the same decision', async ({ page }) => {
    // Three things call flushOutbox now — the online event, load(), and the 30s retry timer added an
    // hour ago. Without a guard, an overlapping run re-POSTs items another is already sending. The
    // server dedups so nothing corrupts, but every duplicate spends a rate-limiter token, which is
    // perverse: throttling is the whole reason that timer exists, so re-entrancy makes being throttled
    // worse. Provoked here by firing every trigger at once against a deliberately slow server.
    await page.addInitScript(() => {
      (window as unknown as { __posts: string[] }).__posts = [];
      window.fetch = async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = String(input);
        if (url.includes('/api/queue')) {
          return new Response(JSON.stringify({ reviewer: 'Sara', items: [], heldByOthers: 0 }), {
            status: 200,
            headers: { 'content-type': 'application/json' },
          });
        }
        if (url.includes('/api/decision')) {
          (window as unknown as { __posts: string[] }).__posts.push(String(init?.body ?? ''));
          await new Promise((r) => setTimeout(r, 300)); // slow enough for a second trigger to land
          return new Response('{"ok":true}', {
            status: 200,
            headers: { 'content-type': 'application/json' },
          });
        }
        return new Response('{"ok":true}', {
          status: 200,
          headers: { 'content-type': 'application/json' },
        });
      };
    });
    await page.goto(PAGE);
    await expect(page.locator('#done')).toBeVisible({ timeout: 5000 });

    // One queued decision, then every flush trigger fired at once while the first is still in flight.
    await seedOperationOutbox(page, [
      {
        operationId: '00000000-0000-4000-8000-000000000031',
        id: 'x9',
        action: 'edit',
        text: 'y',
        reviewer: 'Sara',
        rowVersion: '1',
        pilotAfterReviewEventId: null,
        heardMs: 1000,
        clipDurationMs: 1000,
      },
    ]);
    await page.evaluate(`
      window.__posts = [];
      dispatchEvent(new Event('online'));
      dispatchEvent(new Event('online'));
      dispatchEvent(new Event('online'));
      void load();
    `);
    await expect.poll(async () => operationOutboxCount(page)).toBe(0);

    const posts = (await page.evaluate(`window.__posts`)) as string[];
    const forX9 = posts.filter((b) => b.includes('x9'));
    expect(forX9).toHaveLength(1);
  });

  test('an expired link KEEPS queued work and says so, instead of discarding it silently', async ({
    page,
  }) => {
    // DATA LOSS, and made MORE reachable by fixing Stop/Start: every outstanding token is regenerated
    // when the owner restarts Couch Review or reissues a link. A reviewer who was offline then
    // reconnects to a 401 — and the outbox used to treat ANY status as "the server gave a real
    // answer, drop it", so their queued decisions were thrown away without a word.
    //
    // 401 is not a verdict on the decision. It says the LINK died. A fresh link replays it.
    // The mode lives in localStorage, not on `window`: addInitScript re-runs on every navigation, so
    // a mode held in a variable would be reset to 'ok' by the very reload this test depends on.
    await page.addInitScript(() => {
      window.fetch = async () => {
        const mode = localStorage.getItem('__netmode') || 'ok';
        if (mode === 'offline') throw new TypeError('Failed to fetch');
        if (mode === 'expired') return new Response('unauthorized', { status: 401 });
        return new Response('{"ok":true,"items":[],"reviewer":"Sara"}', {
          status: 200,
          headers: { 'content-type': 'application/json' },
        });
      };
    });
    await page.goto(PAGE);
    await showAClip(page);

    // Offline: the decision is held.
    await page.evaluate(`localStorage.setItem('__netmode', 'offline')`);
    await page.locator('#accept').click();
    await expect.poll(async () => operationOutboxCount(page)).toBe(1);

    // Back online, but the owner restarted the server meanwhile — the token is dead.
    await page.evaluate(`localStorage.setItem('__netmode', 'expired')`);
    await page.reload();

    // The reviewer is TOLD the link expired...
    await expect(page.locator('#err')).toBeVisible();
    await expect(page.locator('#err')).toContainText('بەسەرچووە');
    // ...and their unsent decision is STILL THERE, waiting for a working link.
    expect(await operationOutboxCount(page)).toBe(1);
  });

  test('a decision made after the link dies is HELD, not just toasted away', async ({ page }) => {
    // The sibling of the outbox-replay bug, and the more likely one: the link does not usually die
    // while the reviewer is idle, it dies WHILE THEY ARE WORKING — the owner restarts Couch Review to
    // add a second reviewer and every outstanding token is regenerated.
    //
    // Before this, a 401 on submit only raised a toast reading "Failed to save edit: unauthorized".
    // The reviewer stayed on the clip, their typed text survived as a draft, but the VERDICT
    // (accept / reject / edit) was never recorded anywhere and was simply gone.
    await page.addInitScript(() => {
      window.fetch = async (input: RequestInfo | URL) => {
        if (String(input).includes('/api/decision'))
          return new Response('unauthorized', { status: 401 });
        return new Response('{"ok":true,"items":[],"reviewer":"Sara"}', {
          status: 200,
          headers: { 'content-type': 'application/json' },
        });
      };
    });
    await page.goto(PAGE);
    await showAClip(page);
    await page.locator('#accept').click();

    // The verdict is held for the next working link...
    await expect.poll(async () => operationOutboxCount(page)).toBe(1);
    const held = await readOperationOutbox(page);
    expect(held[0]).toMatchObject({ id: 's1', action: 'accept' });

    // ...the reviewer is told why, persistently rather than in a toast that vanishes...
    await expect(page.locator('#err')).toBeVisible();
    await expect(page.locator('#err')).toContainText('بەسەرچووە');
    // ...and they are moved on so they can keep working instead of tapping a dead clip.
    await expect(page.locator('#text')).toHaveValue('دەقی دووەم');
  });

  test('coming back to the page renews the lease immediately, not four minutes later', async ({
    page,
  }) => {
    // Renewal ticks are skipped while the page is hidden, deliberately: a reviewer who wanders off
    // should release their clips. But on a phone, backgrounding is constant — a call, a notification,
    // the screen locking — and the tick is every 4 minutes against a 15-minute lease. Someone who
    // comes back at minute 13 would have the lease lapse under them at 15, while actively typing,
    // with no renewal due until 16. Another reviewer takes the clip in that window and their
    // correction is refused at save time.
    await page.addInitScript(() => {
      (window as unknown as { __renews: string[] }).__renews = [];
      window.fetch = async (input: RequestInfo | URL, init?: RequestInit) => {
        if (String(input).includes('/api/renew')) {
          (window as unknown as { __renews: string[] }).__renews.push(String(init?.body ?? ''));
        }
        return new Response('{"ok":true,"items":[],"reviewer":"Sara"}', {
          status: 200,
          headers: { 'content-type': 'application/json' },
        });
      };
    });
    await page.goto(PAGE);
    await showAClip(page);
    await page.evaluate(`window.__renews = []`);

    // Going away must NOT renew — that is the property that lets an idle reviewer's clips go back.
    await page.evaluate(`Object.defineProperty(document, 'hidden', { value: true, configurable: true });
                         document.dispatchEvent(new Event('visibilitychange'));`);
    expect(await page.evaluate(`window.__renews.length`)).toBe(0);

    // Coming back must renew at once.
    await page.evaluate(`Object.defineProperty(document, 'hidden', { value: false, configurable: true });
                         document.dispatchEvent(new Event('visibilitychange'));`);
    await expect.poll(async () => page.evaluate(`window.__renews.length`)).toBe(1);
    expect(await page.evaluate(`window.__renews[0]`)).toContain('s1');
  });

  test('a return visit sends no token at all rather than an empty one', async ({ page }) => {
    // THE REPORTED BUG: "I close the browser on my iPhone and go back and it doesn't open."
    //
    // The page strips ?t= from the URL after the first load, so on a return visit its `token` is "".
    // It still appended `?t=` to every request, and the server read that empty value as a
    // supplied-but-WRONG credential, which shadowed the valid HttpOnly cookie riding the same
    // request. The reviewer was refused while the browser was sending exactly what would have let
    // them in. Both sides are fixed; this pins the page half.
    const urls: string[] = [];
    await page.addInitScript(() => {
      window.fetch = async (input: RequestInfo | URL) => {
        (window as unknown as { __urls: string[] }).__urls ??= [];
        (window as unknown as { __urls: string[] }).__urls.push(String(input));
        return new Response('{"ok":true,"items":[],"reviewer":"Sara"}', {
          status: 200,
          headers: { 'content-type': 'application/json' },
        });
      };
    });
    // No token in the URL — exactly the state a returning reviewer's history entry is in.
    await page.goto(PAGE);
    await page.waitForFunction(`(window.__urls || []).length > 0`, null, { timeout: 5000 });
    urls.push(...((await page.evaluate(`window.__urls`)) as string[]));

    expect(urls.length).toBeGreaterThan(0);
    for (const u of urls) {
      expect(u, 'a tokenless page must not send an empty t= — it shadows the cookie').not.toMatch(
        /[?&]t=(&|$)/,
      );
    }
  });

  test('a transient claim failure is retryable, never a false "link expired"', async ({ page }) => {
    // THE DEFECT (found by the adversarial plan review, shipped until today): the fragment->cookie
    // claim was a one-shot const promise created at script parse. A first-ever visitor whose claim
    // POST failed transiently — server restarting, cellular blip — fell through to a 401 queue fetch
    // and was told the link had EXPIRED. False, and unrecoverable: the fragment is stripped from the
    // URL, so even a manual reload has no token. The only escape was re-tapping the original chat
    // message. The token is still in page memory the whole time, so the claim must stay claimable.
    await page.addInitScript(() => {
      (window as unknown as { __up: boolean }).__up = false; // the server is briefly unreachable
      window.fetch = async (input: RequestInfo | URL) => {
        const url = String(input);
        const up = (window as unknown as { __up: boolean }).__up;
        if (url.includes('/api/claim')) {
          if (!up) throw new TypeError('Failed to fetch'); // transient network failure, NOT a verdict
          return new Response('{"ok":true}', {
            status: 200,
            headers: { 'content-type': 'application/json' },
          });
        }
        if (url.includes('/api/queue')) {
          if (!up) return new Response('unauthorized', { status: 401 }); // no cookie was ever minted
          return new Response(
            JSON.stringify({
              reviewer: 'Sara',
              items: [
                {
                  id: 'c1',
                  text: 'پارچە',
                  durationMs: 1000,
                  speakerId: null,
                  rowVersion: '1',
                  pilotAfterReviewEventId: null,
                },
              ],
              heldByOthers: 0,
            }),
            { status: 200, headers: { 'content-type': 'application/json' } },
          );
        }
        return new Response('{"ok":true}', {
          status: 200,
          headers: { 'content-type': 'application/json' },
        });
      };
    });
    await page.goto(PAGE + '#t=valid-token-99');
    await page.reload();

    // The failure must present as RETRYABLE — never as the terminal "link expired" verdict.
    await expect(page.locator('#err')).toBeVisible({ timeout: 5000 });
    await expect(page.locator('#err')).not.toContainText('بەسەرچووە'); // "expired"
    await expect(page.locator('#retry')).toBeVisible();

    // The blip passes; the reviewer taps Retry and lands on their clip with the same in-memory token.
    await page.evaluate(`window.__up = true`);
    await page.locator('#retry').click();
    await expect(page.locator('#text')).toHaveValue('پارچە', { timeout: 5000 });
    await expect(page.locator('#err')).toBeHidden();

    // A genuinely revoked token must still be a real verdict: 401 from the claim itself = expired.
  });

  test('a genuinely refused claim still shows link-expired, with no retry offered', async ({
    page,
  }) => {
    await page.addInitScript(() => {
      window.fetch = async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url.includes('/api/claim')) return new Response('unauthorized', { status: 401 }); // real verdict
        if (url.includes('/api/queue')) return new Response('unauthorized', { status: 401 });
        return new Response('{"ok":true}', {
          status: 200,
          headers: { 'content-type': 'application/json' },
        });
      };
    });
    await page.goto(PAGE + '#t=revoked-token');
    await page.reload();
    await expect(page.locator('#err')).toBeVisible({ timeout: 5000 });
    await expect(page.locator('#err')).toContainText('بەسەرچووە');
    await expect(page.locator('#retry')).toBeHidden(); // retrying a verdict would be a lie
  });

  test('a fragment token is claimed by POST and stripped, never sent in any request URL', async ({
    page,
  }) => {
    // Phase 2 (docs/REMOTE_PUBLIC_LINKS_PLAN.md): links now carry #t=. A fragment never leaves the
    // browser, so a chat app's preview bot fetching the pasted link gets the empty shell — the page
    // must present the token exactly once, in a POST body, and then remove it from the address bar.
    await page.addInitScript(() => {
      (window as unknown as { __reqs: Array<{ url: string; body: string }> }).__reqs = [];
      window.fetch = async (input: RequestInfo | URL, init?: RequestInit) => {
        (window as unknown as { __reqs: Array<{ url: string; body: string }> }).__reqs.push({
          url: String(input),
          body: String(init?.body ?? ''),
        });
        return new Response('{"ok":true,"items":[],"reviewer":"Sara"}', {
          status: 200,
          headers: { 'content-type': 'application/json' },
        });
      };
    });
    // goto() from PAGE to PAGE#t=... is a SAME-DOCUMENT navigation (only the hash changes) and the
    // boot script would not re-run — reload makes it a real load with the fragment present.
    await page.goto(PAGE + '#t=frag-secret-42');
    await page.reload();
    await page.waitForFunction(
      `(window.__reqs || []).some((r) => r.url.includes('/api/claim'))`,
      null,
      {
        timeout: 5000,
      },
    );
    const reqs = (await page.evaluate(`window.__reqs`)) as Array<{ url: string; body: string }>;

    const claim = reqs.find((r) => r.url.includes('/api/claim'));
    expect(claim?.body).toContain('frag-secret-42');
    for (const r of reqs) {
      expect(r.url, 'the token must never appear in a request URL').not.toContain('frag-secret-42');
    }
    await expect.poll(async () => page.url()).not.toContain('frag-secret-42');
  });

  test('the token is stripped from the visible URL after the first load', async ({ page }) => {
    // The server plants an HttpOnly cookie, so the token no longer needs to ride in the URL where it
    // lands in history and in any proxy log. The link the reviewer was SENT keeps working; this only
    // cleans up after it.
    await page.goto(PAGE + '?t=secret-token-123');
    await expect.poll(async () => page.url()).not.toContain('secret-token-123');
    await expect.poll(async () => page.evaluate(`location.search`)).not.toContain('t=');
  });

  test('progress counts the whole backlog, not just the batch in hand', async ({ page }) => {
    // "Clip 7 of 25" was true of the clips in the page's hands and useless as progress: the server
    // hands out at most 25 at a time, so a reviewer working a long backlog watched that number fill
    // and reset over and over, unable to tell a nearly-finished corpus from a barely-started one.
    // The server now sends the real pending total (R1.5) and the line counts against THAT.
    await page.addInitScript(() => {
      window.fetch = async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url.includes('/api/queue')) {
          return new Response(
            JSON.stringify({
              reviewer: 'Sara',
              items: [
                {
                  id: 'q1',
                  text: 'یەکەم',
                  durationMs: 1000,
                  speakerId: null,
                  rowVersion: '1',
                  pilotAfterReviewEventId: null,
                },
                {
                  id: 'q2',
                  text: 'دووەم',
                  durationMs: 1000,
                  speakerId: null,
                  rowVersion: '2',
                  pilotAfterReviewEventId: null,
                },
              ],
              heldByOthers: 0,
              pendingTotal: 407, // a real backlog behind a 2-clip batch
            }),
            { status: 200, headers: { 'content-type': 'application/json' } },
          );
        }
        return new Response('{"ok":true}', {
          status: 200,
          headers: { 'content-type': 'application/json' },
        });
      };
    });
    await page.goto(PAGE);
    await expect(page.locator('#text')).toHaveValue('یەکەم', { timeout: 5000 });
    // The denominator is the corpus, not the batch — 407, never 2.
    await expect(page.locator('#progress')).toContainText('407');
    await expect(page.locator('#progress')).not.toContainText('لە 2');
    // And the position advances within it rather than restarting at every batch boundary.
    await page.locator('#accept').click();
    await expect(page.locator('#progress')).toContainText('2');
    await expect(page.locator('#progress')).toContainText('407');
  });

  test('a server that sends no total still counts honestly, against the batch', async ({
    page,
  }) => {
    // Belt and braces on the same change: the page is served out of the binary, so page and server
    // always ship together — but a missing/zero total must degrade to the old batch counting rather
    // than render "Clip 1 of 0".
    await page.addInitScript(() => {
      window.fetch = async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url.includes('/api/queue')) {
          return new Response(
            JSON.stringify({
              reviewer: 'Sara',
              items: [
                {
                  id: 'q1',
                  text: 'یەکەم',
                  durationMs: 1000,
                  speakerId: null,
                  rowVersion: '1',
                  pilotAfterReviewEventId: null,
                },
              ],
              heldByOthers: 0,
            }),
            { status: 200, headers: { 'content-type': 'application/json' } },
          );
        }
        return new Response('{"ok":true}', {
          status: 200,
          headers: { 'content-type': 'application/json' },
        });
      };
    });
    await page.goto(PAGE);
    await expect(page.locator('#text')).toHaveValue('یەکەم', { timeout: 5000 });
    // The clip's own length rides beside the position (owner ask 2026-08-17), from the same
    // durationMs the fixture serves — so the exact-match keeps guarding the count against off-by-one.
    await expect(page.locator('#progress')).toHaveText('پارچەی 1 لە 1 (1s)');
  });

  test('the Retry button shows it is working, and a double tap costs one fetch', async ({
    page,
  }) => {
    // A reviewer on bad signal taps Retry and NOTHING changes on screen — the failure message stays,
    // the button looks identical — so they cannot tell a working retry from a dead button, and tap
    // again. Each tap must be visibly acknowledged, and the extra taps must not multiply requests.
    //
    // The de-duplication also covers a latent hazard behind the same guard: `load()` is awaited for
    // its DATA by refill() and by undo, and an early `return` would have resolved those awaits with
    // the previous queue still in place. Joining the in-flight promise keeps both honest.
    await page.addInitScript(() => {
      const w = window as unknown as { __n: number; __up: boolean; __release?: () => void };
      w.__n = 0;
      w.__up = false;
      window.fetch = async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url.includes('/api/queue')) {
          w.__n += 1;
          if (!w.__up) throw new TypeError('Failed to fetch');
          // Hold the reply open until the test releases it, so "in flight" is a state it can observe.
          await new Promise<void>((resolve) => {
            w.__release = resolve;
          });
          return new Response(
            JSON.stringify({
              reviewer: 'Sara',
              items: [
                {
                  id: 'r1',
                  text: 'پارچە',
                  durationMs: 1000,
                  speakerId: null,
                  rowVersion: '1',
                  pilotAfterReviewEventId: null,
                },
              ],
              heldByOthers: 0,
              pendingTotal: 1,
            }),
            { status: 200, headers: { 'content-type': 'application/json' } },
          );
        }
        return new Response('{"ok":true}', {
          status: 200,
          headers: { 'content-type': 'application/json' },
        });
      };
    });
    await page.goto(PAGE);
    await expect(page.locator('#retry')).toBeVisible({ timeout: 5000 });
    expect(await page.evaluate(`window.__n`)).toBe(1);

    // Two taps in quick succession while the request hangs.
    await page.evaluate(`window.__up = true`);
    await page.locator('#retry').click({ force: true });
    // Acknowledged: disabled and relabelled, so the tap is visibly doing something.
    await expect(page.locator('#retry')).toBeDisabled();
    await expect(page.locator('#retry')).toContainText('بارکردنی پارچەکان');
    await page.locator('#retry').click({ force: true });
    expect(await page.evaluate(`window.__n`)).toBe(2); // the second tap joined, it did not re-fetch

    // Releasing the held reply lands the clip and returns the button to its resting state.
    await page.evaluate(`window.__release && window.__release()`);
    await expect(page.locator('#text')).toHaveValue('پارچە', { timeout: 5000 });
    await expect(page.locator('#err')).toBeHidden();
    await expect(page.locator('#retry')).toBeEnabled();
    await expect(page.locator('#retry')).toContainText('دووبارە');
  });

  test('deciding a clip starts the next one, so a session is one tap per clip', async ({
    page,
  }) => {
    // TWO TAPS PER CLIP, a hundred times a session: decide, then reach for the player's small native
    // play control. Deciding a clip IS the reviewer saying "next", so the next clip should just start.
    // Not inside a user gesture (decide() awaits a POST first) — it works because the element is
    // already unlocked by the reviewer having pressed play once, which is also why there is no
    // welcome/Start screen: the first clip's own play button does the unlocking.
    await page.addInitScript(() => {
      const w = window as unknown as { __plays: string[] };
      w.__plays = [];
      // jsdom-less Chromium will not decode a nonexistent src, so record the INTENT rather than
      // relying on real playback: the assertion is about whether the page asks to play, and when.
      const proto = window.HTMLMediaElement.prototype;
      proto.play = function play() {
        w.__plays.push(this.getAttribute('src') || '');
        return Promise.resolve();
      };
      window.fetch = async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url.includes('/api/queue')) {
          return new Response(
            JSON.stringify({
              reviewer: 'Sara',
              items: [
                {
                  id: 'one',
                  text: 'یەکەم',
                  durationMs: 1000,
                  speakerId: null,
                  rowVersion: '1',
                  pilotAfterReviewEventId: null,
                },
                {
                  id: 'two',
                  text: 'دووەم',
                  durationMs: 1000,
                  speakerId: null,
                  rowVersion: '2',
                  pilotAfterReviewEventId: null,
                },
              ],
              heldByOthers: 0,
              pendingTotal: 2,
            }),
            { status: 200, headers: { 'content-type': 'application/json' } },
          );
        }
        return new Response('{"ok":true}', {
          status: 200,
          headers: { 'content-type': 'application/json' },
        });
      };
    });
    await page.goto(PAGE);
    await expect(page.locator('#text')).toHaveValue('یەکەم', { timeout: 5000 });
    // Opening the page must NOT start audio on its own — arriving is not a request to play.
    expect(await page.evaluate(`window.__plays`)).toEqual([]);

    await page.locator('#accept').click();
    await expect(page.locator('#text')).toHaveValue('دووەم', { timeout: 5000 });
    await expect.poll(async () => page.evaluate(`window.__plays.length`)).toBe(1);
    // ...and it played the clip that is now on screen, not the one just decided.
    expect(await page.evaluate(`window.__plays[0].includes('two')`)).toBe(true);
  });

  test('navigation never starts audio — only a decision does', async ({ page }) => {
    // The other half of the same rule. A skip is the reviewer saying "I cannot judge this", and an undo
    // is them going back to look; both deserve a loaded clip and silence, not a burst of sound.
    await page.addInitScript(() => {
      const w = window as unknown as { __plays: number };
      w.__plays = 0;
      window.HTMLMediaElement.prototype.play = function play() {
        w.__plays += 1;
        return Promise.resolve();
      };
      window.fetch = async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url.includes('/api/queue')) {
          return new Response(
            JSON.stringify({
              reviewer: 'Sara',
              items: [
                {
                  id: 'one',
                  text: 'یەکەم',
                  durationMs: 1000,
                  speakerId: null,
                  rowVersion: '1',
                  pilotAfterReviewEventId: null,
                },
                {
                  id: 'two',
                  text: 'دووەم',
                  durationMs: 1000,
                  speakerId: null,
                  rowVersion: '2',
                  pilotAfterReviewEventId: null,
                },
              ],
              heldByOthers: 0,
              pendingTotal: 2,
            }),
            { status: 200, headers: { 'content-type': 'application/json' } },
          );
        }
        return new Response('{"ok":true}', {
          status: 200,
          headers: { 'content-type': 'application/json' },
        });
      };
    });
    await page.goto(PAGE);
    await expect(page.locator('#text')).toHaveValue('یەکەم', { timeout: 5000 });
    await page.locator('#skip').click();
    await expect(page.locator('#text')).toHaveValue('دووەم', { timeout: 5000 });
    await page.waitForTimeout(200);
    expect(await page.evaluate(`window.__plays`)).toBe(0);
  });

  test('typing pauses the audio, and resuming hands back the last 2 seconds', async ({ page }) => {
    // A reviewer reaching for the transcript while audio runs is trying to do two things at once and
    // loses the tail of what they just heard. Pause on focus, and give the run-up back on resume —
    // but ONLY when this mechanism caused the pause: a pause the reviewer chose is a position they
    // chose, and moving it would be the tool overriding them.
    await showAClip(page);
    await page.evaluate(`
      const p = document.getElementById('player');
      Object.defineProperty(p, 'paused', { value: false, writable: true, configurable: true });
      p.currentTime = 10;
      p.pause = function () { this.paused = true; };
    `);
    await page.locator('#text').focus();
    expect(await page.evaluate(`document.getElementById('player').paused`)).toBe(true);
    expect(await page.evaluate(`pausedByEdit`)).toBe(true);

    // Resuming rewinds exactly the ↺2s amount.
    await page.evaluate(`document.getElementById('player').dispatchEvent(new Event('play'))`);
    expect(await page.evaluate(`document.getElementById('player').currentTime`)).toBe(8);
    expect(await page.evaluate(`pausedByEdit`)).toBe(false);

    // A SECOND play with no intervening edit-pause must not rewind again.
    await page.evaluate(`document.getElementById('player').dispatchEvent(new Event('play'))`);
    expect(await page.evaluate(`document.getElementById('player').currentTime`)).toBe(8);

    // And ↺2s must not double up: it clears the flag before playing, or the reviewer silently gets -4s.
    await page.evaluate(`
      const p = document.getElementById('player');
      p.currentTime = 10;
      pausedByEdit = true;
      p.play = function () { this.dispatchEvent(new Event('play')); return Promise.resolve(); };
    `);
    await page.locator('#again').click();
    expect(await page.evaluate(`document.getElementById('player').currentTime`)).toBe(8);
  });

  test('the keyboard can never cover the save buttons or the toast', async ({ page }) => {
    // THE WORST FLOW-BREAKER ON THE PAGE, verified: on iOS the on-screen keyboard does not resize the
    // layout viewport, so a reviewer who tapped the transcript to fix a word had Save/Accept/Reject —
    // and the "Saved" toast — sitting underneath it. They typed the correction and could not see the
    // button that commits it. The page now tracks the covered height in `--kb`.
    await showAClip(page);
    const kb = () =>
      page.evaluate(`getComputedStyle(document.documentElement).getPropertyValue('--kb').trim()`);
    // Nothing covered yet: no phantom padding from ordinary browser chrome.
    expect(['', '0px']).toContain(await kb());

    const roomBefore = await page.evaluate(`document.documentElement.scrollHeight`);
    // Simulate the keyboard by shrinking the visual viewport, exactly as iOS reports it — the layout
    // viewport does NOT change there, which is the whole reason this listener has to exist.
    await page.locator('#text').focus();
    await page.evaluate(`
      Object.defineProperty(window.visualViewport, 'height', { value: window.innerHeight - 320, configurable: true });
      Object.defineProperty(window.visualViewport, 'offsetTop', { value: 0, configurable: true });
      window.visualViewport.dispatchEvent(new Event('resize'));
    `);
    expect(await kb()).toBe('320px');

    // Half one: the page can now scroll far enough for the row to clear the keyboard. Without this
    // there is no room at all — the buttons sit behind it with nowhere to go.
    expect(await page.evaluate(`document.documentElement.scrollHeight`)).toBe(roomBefore + 320);

    // Half two: it is actually scrolled there, so the reviewer does not have to discover that they
    // could scroll. Every decision button must be inside what they can see.
    const visibleBottom = await page.evaluate(`window.innerHeight - 320`);
    for (const id of ['save', 'accept', 'bad']) {
      await expect
        .poll(
          async () => {
            const box = await page.locator(`#${id}`).boundingBox();
            return box ? box.y + box.height : Number.POSITIVE_INFINITY;
          },
          { message: `#${id} must clear the keyboard`, timeout: 5000 },
        )
        .toBeLessThanOrEqual(visibleBottom);
    }
    // Retracting the keyboard puts the padding back, rather than leaving a permanent gap.
    await page.evaluate(`
      Object.defineProperty(window.visualViewport, 'height', { value: window.innerHeight, configurable: true });
      window.visualViewport.dispatchEvent(new Event('resize'));
    `);
    expect(await kb()).toBe('0px');
  });

  test('a failure toast waits to be read; a success toast does not', async ({ page }) => {
    // 1.4s is right for "Saved" — the reviewer knows what they did and is already moving. It is wrong
    // for "could not save", which arrives while their attention is on the NEXT clip: a message about
    // work that did not land, gone before it is read, is the same as no message.
    await showAClip(page);
    await page.evaluate(`toast('SUCCESS-MSG')`);
    await expect(page.locator('#toast')).toHaveClass(/show/);
    await expect(page.locator('#toast')).not.toHaveClass(/sticky/);
    await expect(page.locator('#toast')).not.toHaveClass(/show/, { timeout: 4000 });

    await page.evaluate(`toast('FAILURE-MSG', true)`);
    await expect(page.locator('#toast')).toHaveClass(/sticky/);
    await page.waitForTimeout(2200); // well past the success lifetime
    await expect(page.locator('#toast')).toHaveClass(/show/);
    // Dismissible, because a message that cannot be cleared is its own problem.
    await page.locator('#toast').click();
    await expect(page.locator('#toast')).not.toHaveClass(/show/);

    // A success arriving after a failure must not be frozen on screen by the failure's missing timer.
    await page.evaluate(`toast('FAILURE-MSG', true)`);
    await page.evaluate(`toast('SUCCESS-MSG')`);
    await expect(page.locator('#toast')).not.toHaveClass(/sticky/);
    await expect(page.locator('#toast')).not.toHaveClass(/show/, { timeout: 4000 });
  });

  test('no raw English server text ever reaches a Sorani sentence', async ({ page }) => {
    // Every string here is translated, and then the one word saying WHAT WENT WRONG used to arrive in
    // English from the server — "unauthorized", "Failed to fetch" — inside a Sorani sentence. The
    // status code is language-neutral, still diagnostic, and needs no new Sorani.
    await page.addInitScript(() => {
      window.fetch = async (input: RequestInfo | URL) => {
        if (String(input).includes('/api/queue')) {
          return new Response('another reviewer is working on this clip', { status: 409 });
        }
        return new Response('{"ok":true}', {
          status: 200,
          headers: { 'content-type': 'application/json' },
        });
      };
    });
    await page.goto(PAGE);
    await expect(page.locator('#err')).toBeVisible({ timeout: 5000 });
    const text = (await page.locator('#err').textContent()) || '';
    expect(text).toContain('409'); // the diagnostic survives
    expect(text).not.toMatch(/[a-z]{4,}/i); // ...with no English word in it
  });

  test('tapping the refused banner goes to a refused clip, or honestly does nothing', async ({
    page,
  }) => {
    // The banner says "find those clips and review them again" and gave the reviewer no way to find
    // them — ids they were never shown, in a queue they must scan by eye on a phone.
    await showAClip(page);
    // Refuse the SECOND clip, so a correct jump is observable as a move.
    await page.evaluate(`
      localStorage.setItem('cortex.couch.refused', JSON.stringify(['s2']));
      renderRefused();
    `);
    await expect(page.locator('#err')).toBeVisible();
    await expect(page.locator('#text')).toHaveValue(/نموونەیی/); // still on clip 1
    await page.locator('#err').click();
    await expect(page.locator('#text')).toHaveValue('دەقی دووەم', { timeout: 5000 });

    // A refusal for a clip NOT in this batch — the usual case, since someone else took it — must not
    // jump anywhere. Landing on the wrong clip would be worse than not moving: the reviewer would
    // trust it and re-review the wrong audio.
    await page.evaluate(`
      i = 0; show();
      localStorage.setItem('cortex.couch.refused', JSON.stringify(['not-in-this-batch']));
      renderRefused();
    `);
    await expect(page.locator('#text')).toHaveValue(/نموونەیی/);
    await page.locator('#err').click();
    await page.waitForTimeout(200);
    await expect(page.locator('#text')).toHaveValue(/نموونەیی/);
  });

  test('everything that changes on its own is announced to a screen reader', async ({ page }) => {
    // The progress counter after a decision, a warning about a clip, the drained-queue verdict: each
    // changes without the reviewer touching anything, which is exactly what a screen reader cannot
    // discover by itself. `polite` so none of them interrupts a word mid-utterance.
    await showAClip(page);
    for (const [id, expected] of [
      ['progress', 'polite'],
      ['warn', 'polite'],
      ['done', 'polite'],
      ['toast', 'polite'],
    ] as const) {
      expect(await page.locator(`#${id}`).getAttribute('aria-live'), `#${id}`).toBe(expected);
    }
    // #err is the one that must interrupt: it carries the link-expired verdict and the refused banner,
    // both of which need an action from the reviewer before they can keep working.
    expect(await page.locator('#err').getAttribute('role')).toBe('alert');
  });

  test('the swipe shows what it is about to do, and snaps back when it will not', async ({
    page,
  }) => {
    // The gesture worked and was invisible: nothing moved while the finger moved, so a reviewer whose
    // thumb happened to travel 90 px cast a verdict with no warning, and one who wanted to use the
    // gesture had no way to discover it or to see that they had crossed the threshold.
    await showAClip(page);
    // Begin the gesture on the card itself (not the textarea, which must never read as a verdict), and
    // REMEMBER where it started. Re-reading getBoundingClientRect() per move is wrong once the card is
    // translated — the rect moves with the transform, so each dx would be measured from the card's new
    // position instead of the finger's origin. (That was a bug in this test, not in the page: it made a
    // 120 px swipe arrive as 78 px and silently fall under the commit distance.)
    await page.evaluate(`
      const card = document.getElementById('card');
      const box = card.getBoundingClientRect();
      window.__origin = { x: box.left + 20, y: box.top + 40 };
      card.dispatchEvent(new TouchEvent('touchstart', { bubbles: true, changedTouches: [
        new Touch({ identifier: 1, target: card, clientX: window.__origin.x, clientY: window.__origin.y }) ] }));
    `);
    const swipe = async (dx: number, dy: number, phase: 'move' | 'end') => {
      await page.evaluate(
        ([x, y, p]) => {
          const card = document.getElementById('card')!;
          const origin = (window as unknown as { __origin: { x: number; y: number } }).__origin;
          card.dispatchEvent(
            new TouchEvent(p === 'move' ? 'touchmove' : 'touchend', {
              bubbles: true,
              changedTouches: [
                new Touch({
                  identifier: 1,
                  target: card,
                  clientX: origin.x + (x as number),
                  clientY: origin.y + (y as number),
                }),
              ],
            }),
          );
        },
        [dx, dy, phase] as const,
      );
    };

    // UNDER the threshold: the card follows the finger, but promises nothing.
    await swipe(40, 0, 'move');
    const t40 = await page.evaluate(`document.getElementById('card').style.transform`);
    expect(t40).toContain('translateX');
    expect(t40).not.toBe('translateX(0.0px)');
    await expect(page.locator('#card')).toHaveClass(/dragging/);
    await expect(page.locator('#card')).not.toHaveClass(/willAccept|willReject/);

    // PAST it: the page is RTL, so leftward (negative dx) is forward = accept.
    await swipe(-120, 0, 'move');
    await expect(page.locator('#card')).toHaveClass(/willAccept/);
    await swipe(120, 0, 'move');
    await expect(page.locator('#card')).toHaveClass(/willReject/);

    // Too vertical is a scroll, and the feedback must drop instantly rather than leave the card
    // hanging mid-drag while the page moves under it.
    await swipe(120, 90, 'move');
    await expect(page.locator('#card')).not.toHaveClass(/dragging|willAccept|willReject/);
    expect(await page.evaluate(`document.getElementById('card').style.transform`)).toBe('');

    // Releasing under the commit distance decides NOTHING and leaves no transform behind.
    await swipe(30, 0, 'move');
    await swipe(30, 0, 'end');
    expect(await page.evaluate(`document.getElementById('card').style.transform`)).toBe('');
    await expect(page.locator('#text')).toHaveValue(/نموونەیی/); // still on the same clip
    await expect(page.locator('#card')).not.toHaveClass(/dragging/);
  });

  for (const scheme of ['light', 'dark'] as const) {
    test(`has zero WCAG 2.2 AA violations while reviewing (${scheme})`, async ({ page }) => {
      // Both themes, because the page renders in whichever the phone is set to and a contrast
      // failure in one is invisible from the other.
      await page.emulateMedia({ colorScheme: scheme });
      await showAClip(page);
      const results = await new AxeBuilder({ page }).withTags(WCAG_AA).analyze();
      for (const v of results.violations) {
        for (const n of v.nodes) {
          console.log(
            `[axe:${scheme}] ${v.id}: ${n.target.join(' ')} — ${n.failureSummary?.split('\n')[1] ?? ''}`,
          );
        }
      }
      expect(results.violations.map((v) => `${v.id} x${v.nodes.length}`)).toEqual([]);
    });
  }
});
