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

/** Put the page into its REVIEWING state — the state a reviewer actually spends their session in.
 *
 * Evaluated as a STRING, not an arrow function. The page's state lives in top-level `let` bindings,
 * and `let` at global scope does NOT become a property of `window` — so `window.queue = …` would
 * create a second, unrelated variable that `show()` never reads (it did, and every assertion failed
 * against an empty card). A bare assignment resolves through the scope chain to the real binding. */
async function showAClip(page: import('@playwright/test').Page) {
  await page.evaluate(`
    queue = [
      { id: 's1', text: 'ئەمە دەقێکی نموونەیی کوردییە بۆ پێداچوونەوە', durationMs: 4200, speakerId: 'spk_01' },
      { id: 's2', text: 'دەقی دووەم', durationMs: 3100, speakerId: null },
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

  test('opens in Sorani RTL — a Kurdish-first app must not greet its reviewer in English', async ({ page }) => {
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

  test('the empty state is actually empty — `hidden` is not defeated by a display rule', async ({ page }) => {
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
      (window as unknown as { __net: { fail: boolean; calls: string[] } }).__net = { fail: false, calls: [] };
      window.fetch = async (input: RequestInfo | URL, init?: RequestInit) => {
        const net = (window as unknown as { __net: { fail: boolean; calls: string[] } }).__net;
        const url = String(input);
        net.calls.push(url + ' ' + String(init?.body ?? ''));
        // A dropped request is a TypeError from fetch — no status — which is exactly how the page
        // tells "never arrived" (retry) from "the server refused" (do not retry).
        if (net.fail) throw new TypeError('Failed to fetch');
        return new Response('{"ok":true}', { status: 200, headers: { 'content-type': 'application/json' } });
      };
    });
    await page.goto(PAGE);
    await showAClip(page);

    // The network drops, then the reviewer saves.
    await page.evaluate(`window.__net.fail = true`);
    await page.locator('#accept').click();

    // Their decision is HELD, not lost — and they are moved on rather than stranded on a dead clip.
    const queued = await page.evaluate(`JSON.parse(localStorage.getItem('cortex.couch.outbox') || '[]')`);
    expect(queued).toHaveLength(1);
    expect((queued as Array<{ id: string }>)[0].id).toBe('s1');
    await expect(page.locator('#text')).toHaveValue('دەقی دووەم', { timeout: 5000 });

    // Network returns; the next load flushes it. Replay is safe only because the SERVER answers an
    // identical re-submit as already-recorded (couch.rs is_repeat_of_stored_decision).
    await page.evaluate(`window.__net.fail = false`);
    await page.reload();
    await page.waitForFunction(`JSON.parse(localStorage.getItem('cortex.couch.outbox') || '[]').length === 0`, null, {
      timeout: 5000,
    });
    const calls = (await page.evaluate(`window.__net.calls`)) as string[];
    expect(calls.some((c) => c.includes('/api/decision') && c.includes('s1'))).toBe(true);
  });

  test('a submit the server REFUSES is dropped, not retried forever', async ({ page }) => {
    // The mirror image, and just as important: a 409 (someone else took the clip) or a 400 is a real
    // ANSWER. Queueing it would wedge the outbox behind a decision that can never land, and every
    // later decision would be stuck behind it.
    await page.addInitScript(() => {
      window.fetch = async (input: RequestInfo | URL) => {
        if (String(input).includes('/api/decision')) return new Response('another reviewer', { status: 409 });
        return new Response('{"ok":true}', { status: 200, headers: { 'content-type': 'application/json' } });
      };
    });
    await page.goto(PAGE);
    await showAClip(page);
    await page.locator('#accept').click();
    await expect
      .poll(async () => page.evaluate(`JSON.parse(localStorage.getItem('cortex.couch.outbox') || '[]').length`))
      .toBe(0);
  });

  test('draining a batch fetches the next one instead of claiming the corpus is finished', async ({ page }) => {
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
        [{ id: 'a1', text: 'یەکەم', durationMs: 1000, speakerId: null }],
        [{ id: 'b1', text: 'دووەم', durationMs: 1000, speakerId: null }],
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
        return new Response('{"ok":true}', { status: 200, headers: { 'content-type': 'application/json' } });
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

  test('a refused replay keeps the typed correction and says so, instead of deleting it in silence', async ({ page }) => {
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
              items: [{ id: 'x1', text: 'دەقی سەرەتایی', durationMs: 1000, speakerId: null }],
              heldByOthers: 0,
            }),
            { status: 200, headers: { 'content-type': 'application/json' } },
          );
        }
        if (url.includes('/api/decision')) {
          if (net.offline) throw new TypeError('Failed to fetch');
          return new Response('already reviewed by Hemn', { status: 409 });
        }
        return new Response('{"ok":true}', { status: 200, headers: { 'content-type': 'application/json' } });
      };
    });
    await page.goto(PAGE);
    await expect(page.locator('#text')).toHaveValue('دەقی سەرەتایی', { timeout: 5000 });

    // Offline: type a correction and save. It is QUEUED, and must not be called "Saved".
    await page.locator('#text').fill('ڕاستکراوەی سارا');
    await page.locator('#save').click();
    await expect
      .poll(async () => page.evaluate(`JSON.parse(localStorage.getItem('cortex.couch.outbox') || '[]').length`))
      .toBe(1);
    await expect(page.locator('#toast')).toHaveText('لە ڕیزدایە — کاتێک گەڕایتەوە سەر ئینتەرنێت دەنێردرێت');
    const queued = (await page.evaluate(
      `JSON.parse(localStorage.getItem('cortex.couch.outbox') || '[]')[0]`,
    )) as { reviewer?: string; text: string };
    expect(queued.reviewer).toBe('Sara'); // stamped, so it can never be replayed under another name
    expect(queued.text).toBe('ڕاستکراوەی سارا');

    // Back online, the server refuses it. Driven by the 'online' event rather than a reload:
    // addInitScript re-runs on every navigation and would reset __net.offline back to true.
    await page.evaluate(`window.__net.offline = false; dispatchEvent(new Event('online'))`);
    await expect
      .poll(async () => page.evaluate(`JSON.parse(localStorage.getItem('cortex.couch.outbox') || '[]').length`))
      .toBe(0);

    // The decision is dropped — correct — but the WORK is kept and the reviewer is told.
    expect(await page.evaluate(`localStorage.getItem('cortex.couch.draft.x1')`)).toBe('ڕاستکراوەی سارا');
    await expect(page.locator('#err')).toBeVisible();
    await expect(page.locator('#err')).toContainText('1');
  });

  test('a decision queued by one reviewer is never flushed under another reviewer name', async ({ page }) => {
    // localStorage is per-ORIGIN, not per-reviewer. Two people sharing a phone, or one person opening
    // a colleague's link, share one outbox — so an unstamped decision flushed under whoever's cookie
    // is current would record Sara's judgement of a clip as Hemn's, permanently and invisibly.
    await page.addInitScript(() => {
      (window as unknown as { __sent: string[] }).__sent = [];
      window.fetch = async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = String(input);
        if (url.includes('/api/decision')) {
          (window as unknown as { __sent: string[] }).__sent.push(String(init?.body ?? ''));
          return new Response('{"ok":true}', { status: 200, headers: { 'content-type': 'application/json' } });
        }
        if (url.includes('/api/queue')) {
          return new Response(JSON.stringify({ reviewer: 'Hemn', items: [], heldByOthers: 0 }), {
            status: 200,
            headers: { 'content-type': 'application/json' },
          });
        }
        return new Response('{"ok":true}', { status: 200, headers: { 'content-type': 'application/json' } });
      };
    });
    await page.goto(PAGE);
    // Wait until the page knows who it is — the guard is meaningless before /api/queue answers.
    await expect(page.locator('#who')).toHaveText(' · Hemn');
    // Seeded AFTER load so addInitScript cannot wipe it, and flushed via the 'online' event, which is
    // exactly how a reconnecting phone triggers it.
    await page.evaluate(`
      localStorage.setItem('cortex.couch.outbox', JSON.stringify([
        { id: 'sara1', action: 'edit', text: 'هی سارا', reviewer: 'Sara' },
        { id: 'hemn1', action: 'edit', text: 'هی هێمن', reviewer: 'Hemn' },
      ]));
      dispatchEvent(new Event('online'));
    `);
    await expect.poll(async () => page.evaluate(`window.__sent.length`)).toBe(1);

    const sent = (await page.evaluate(`window.__sent`)) as string[];
    expect(sent[0]).toContain('hemn1');
    expect(sent.join(' ')).not.toContain('sara1');
    // Sara's decision is still waiting for Sara — held, not sent, and not thrown away either.
    const left = (await page.evaluate(
      `JSON.parse(localStorage.getItem('cortex.couch.outbox') || '[]')`,
    )) as Array<{ id: string }>;
    expect(left.map((q) => q.id)).toEqual(['sara1']);
  });

  test('the drained state is honest: undo reachable, audio stopped, count not off by one', async ({ page }) => {
    // Four separate defects that all only appear once the batch runs out — the moment the reviewer is
    // least likely to be watching closely, and most likely to close the page.
    await page.addInitScript(() => {
      (window as unknown as { __q: { n: number } }).__q = { n: 0 };
      window.fetch = async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url.includes('/api/queue')) {
          const st = (window as unknown as { __q: { n: number } }).__q;
          const items =
            st.n === 0 ? [{ id: 'z1', text: 'تاکە پارچە', durationMs: 1000, speakerId: null }] : [];
          st.n += 1;
          return new Response(JSON.stringify({ reviewer: 'Sara', items, heldByOthers: 0 }), {
            status: 200,
            headers: { 'content-type': 'application/json' },
          });
        }
        return new Response('{"ok":true}', { status: 200, headers: { 'content-type': 'application/json' } });
      };
    });
    await page.goto(PAGE);
    await expect(page.locator('#text')).toHaveValue('تاکە پارچە', { timeout: 5000 });
    // Header must never read "Clip 2 of 1".
    await expect(page.locator('#progress')).toHaveText('پارچەی 1 لە 1');

    await page.locator('#accept').click();
    await expect(page.locator('#done')).toBeVisible({ timeout: 5000 });

    // UNDO must still be reachable. It used to live inside #card, so it vanished exactly when the
    // reviewer wanted to take back the decision they had just made.
    await expect(page.locator('#undo')).toBeVisible();
    // The <audio> element must not still be playing behind the empty state.
    expect(await page.evaluate(`document.getElementById('player').paused`)).toBe(true);
    // And the progress counter must not have run past the end of the batch.
    expect(await page.evaluate(`document.getElementById('progress').textContent`)).not.toContain('2');
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
            st.n === 0 ? [{ id: 'w1', text: 'کۆتا پارچە', durationMs: 1000, speakerId: null }] : [];
          st.n += 1;
          return new Response(JSON.stringify({ reviewer: 'Sara', items, heldByOthers: 0 }), {
            status: 200,
            headers: { 'content-type': 'application/json' },
          });
        }
        if (url.includes('/api/decision')) throw new TypeError('Failed to fetch'); // offline
        return new Response('{"ok":true}', { status: 200, headers: { 'content-type': 'application/json' } });
      };
    });
    await page.goto(PAGE);
    await expect(page.locator('#text')).toHaveValue('کۆتا پارچە', { timeout: 5000 });
    await page.locator('#accept').click();

    await expect(page.locator('#done')).toBeVisible({ timeout: 5000 });
    await expect(page.locator('#done')).not.toContainText('هەموو پارچەکان');
    await expect(page.locator('#done')).toContainText('نەنێردراون'); // "have not been sent"
  });

  test('a clip whose audio will not load offers a skip instead of forcing a false verdict', async ({ page }) => {
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
                { id: 'broken', text: 'پارچەی تێکچوو', durationMs: 1000, speakerId: null },
                { id: 'fine', text: 'پارچەی باش', durationMs: 1000, speakerId: null },
              ],
              heldByOthers: 0,
            }),
            { status: 200, headers: { 'content-type': 'application/json' } },
          );
        }
        if (url.includes('/api/decision')) {
          (window as unknown as { __sent: string[] }).__sent.push(String(init?.body ?? ''));
        }
        return new Response('{"ok":true}', { status: 200, headers: { 'content-type': 'application/json' } });
      };
    });
    await page.goto(PAGE);
    await expect(page.locator('#text')).toHaveValue('پارچەی تێکچوو', { timeout: 5000 });
    // NOTE ON THIS ENVIRONMENT: the page is served over file://, so <audio src="/api/audio/..."> is a
    // real request that always fails — window.fetch is stubbed, media loads are not. The handler
    // therefore fires for EVERY clip here, which is correct behaviour, so "hidden by default" cannot be
    // observed asynchronously. The reset is asserted in the SAME TICK as show() instead: show() clears
    // the flag synchronously and the error event is async, so this reads the reset deterministically.
    const hiddenRightAfterShow = await page.evaluate(
      `show(); document.getElementById('skip').hidden`,
    );
    expect(hiddenRightAfterShow).toBe(true);

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
    // ...and NOTHING was written about the broken one. That is the whole point.
    const sent = (await page.evaluate(`window.__sent`)) as string[];
    expect(sent.join(' ')).not.toContain('broken');
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
              items: [{ id: 'p1', text: 'دەق', durationMs: 1000, speakerId: null }],
              heldByOthers: 0,
            }),
            { status: 200, headers: { 'content-type': 'application/json' } },
          );
        }
        return new Response('{"ok":true}', { status: 200, headers: { 'content-type': 'application/json' } });
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
                { id: 't1', text: 'یەکەم', durationMs: 1000, speakerId: null },
                { id: 't2', text: 'دووەم', durationMs: 1000, speakerId: null },
              ],
              heldByOthers: 0,
            }),
            { status: 200, headers: { 'content-type': 'application/json' } },
          );
        }
        if (url.includes('/api/decision') && (window as unknown as { __throttle: boolean }).__throttle) {
          return new Response('rate limit exceeded', { status: 429 });
        }
        return new Response('{"ok":true}', { status: 200, headers: { 'content-type': 'application/json' } });
      };
    });
    await page.goto(PAGE);
    await expect(page.locator('#text')).toHaveValue('یەکەم', { timeout: 5000 });

    await page.locator('#accept').click();
    // HELD in the outbox, and the reviewer is moved on rather than stranded re-submitting.
    await expect
      .poll(async () => page.evaluate(`JSON.parse(localStorage.getItem('cortex.couch.outbox') || '[]').length`))
      .toBe(1);
    await expect(page.locator('#text')).toHaveValue('دووەم');
    // Not reported as saved — it is queued, which is the truth.
    await expect(page.locator('#toast')).toHaveText('لە ڕیزدایە — کاتێک گەڕایتەوە سەر ئینتەرنێت دەنێردرێت');
    // And NOT counted as a refused decision: throttling is not a verdict.
    expect(await page.evaluate(`localStorage.getItem('cortex.couch.refused')`)).toBeNull();

    // The limiter refills; the held decision lands on the next flush.
    await page.evaluate(`window.__throttle = false; dispatchEvent(new Event('online'))`);
    await expect
      .poll(async () => page.evaluate(`JSON.parse(localStorage.getItem('cortex.couch.outbox') || '[]').length`))
      .toBe(0);
    await expect(page.locator('#err')).toBeHidden();
  });

  test('the refused-decisions banner is retracted once that clip is re-reviewed', async ({ page }) => {
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
              items: [{ id: 'g1', text: 'پارچە', durationMs: 1000, speakerId: null }],
              heldByOthers: 0,
            }),
            { status: 200, headers: { 'content-type': 'application/json' } },
          );
        }
        if (url.includes('/api/decision')) {
          return (window as unknown as { __refuse: boolean }).__refuse
            ? new Response('already reviewed by Hemn', { status: 409 })
            : new Response('{"ok":true}', { status: 200, headers: { 'content-type': 'application/json' } });
        }
        return new Response('{"ok":true}', { status: 200, headers: { 'content-type': 'application/json' } });
      };
    });
    await page.goto(PAGE);
    await expect(page.locator('#text')).toHaveValue('پارچە', { timeout: 5000 });

    // Seed a refusal through the real flush path, then confirm the banner is up.
    await page.evaluate(`
      localStorage.setItem('cortex.couch.outbox', JSON.stringify([{ id: 'g1', action: 'edit', text: 'x', reviewer: 'Sara' }]));
      dispatchEvent(new Event('online'));
    `);
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
              items: [{ id: 'h1', text: 'پارچە', durationMs: 1000, speakerId: null }],
              heldByOthers: 0,
            }),
            { status: 200, headers: { 'content-type': 'application/json' } },
          );
        }
        return new Response('{"ok":true}', { status: 200, headers: { 'content-type': 'application/json' } });
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
              items: [{ id: 'k1', text: 'پارچە', durationMs: 1000, speakerId: null }],
              heldByOthers: 0,
            }),
            { status: 200, headers: { 'content-type': 'application/json' } },
          );
        }
        if (url.includes('/api/decision')) {
          if (w.__throttle) return new Response('rate limit exceeded', { status: 429 });
          w.__sent.push(String(init?.body ?? ''));
          return new Response('{"ok":true}', { status: 200, headers: { 'content-type': 'application/json' } });
        }
        return new Response('{"ok":true}', { status: 200, headers: { 'content-type': 'application/json' } });
      };
    });
    await page.goto(PAGE);
    await expect(page.locator('#text')).toHaveValue('پارچە', { timeout: 5000 });

    await page.locator('#accept').click();
    await expect
      .poll(async () => page.evaluate(`JSON.parse(localStorage.getItem('cortex.couch.outbox') || '[]').length`))
      .toBe(1);

    // The throttle clears. NOTHING else happens: no reload, no online event, no batch drain.
    await page.evaluate(`window.__throttle = false`);
    await page.clock.runFor(31_000);

    await expect
      .poll(async () => page.evaluate(`JSON.parse(localStorage.getItem('cortex.couch.outbox') || '[]').length`))
      .toBe(0);
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
          return new Response('{"ok":true}', { status: 200, headers: { 'content-type': 'application/json' } });
        }
        return new Response('{"ok":true}', { status: 200, headers: { 'content-type': 'application/json' } });
      };
    });
    await page.goto(PAGE);
    await expect(page.locator('#done')).toBeVisible({ timeout: 5000 });

    // One queued decision, then every flush trigger fired at once while the first is still in flight.
    await page.evaluate(`
      localStorage.setItem('cortex.couch.outbox', JSON.stringify([{ id: 'x9', action: 'edit', text: 'y', reviewer: 'Sara' }]));
      window.__posts = [];
      dispatchEvent(new Event('online'));
      dispatchEvent(new Event('online'));
      dispatchEvent(new Event('online'));
      void load();
    `);
    await expect
      .poll(async () => page.evaluate(`JSON.parse(localStorage.getItem('cortex.couch.outbox') || '[]').length`))
      .toBe(0);

    const posts = (await page.evaluate(`window.__posts`)) as string[];
    const forX9 = posts.filter((b) => b.includes('x9'));
    expect(forX9).toHaveLength(1);
  });

  test('an expired link KEEPS queued work and says so, instead of discarding it silently', async ({ page }) => {
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
    await expect
      .poll(async () => page.evaluate(`JSON.parse(localStorage.getItem('cortex.couch.outbox') || '[]').length`))
      .toBe(1);

    // Back online, but the owner restarted the server meanwhile — the token is dead.
    await page.evaluate(`localStorage.setItem('__netmode', 'expired')`);
    await page.reload();

    // The reviewer is TOLD the link expired...
    await expect(page.locator('#err')).toBeVisible();
    await expect(page.locator('#err')).toContainText('بەسەرچووە');
    // ...and their unsent decision is STILL THERE, waiting for a working link.
    expect(
      await page.evaluate(`JSON.parse(localStorage.getItem('cortex.couch.outbox') || '[]').length`),
    ).toBe(1);
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
        if (String(input).includes('/api/decision')) return new Response('unauthorized', { status: 401 });
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
    await expect
      .poll(async () => page.evaluate(`JSON.parse(localStorage.getItem('cortex.couch.outbox') || '[]').length`))
      .toBe(1);
    const held = (await page.evaluate(
      `JSON.parse(localStorage.getItem('cortex.couch.outbox') || '[]')`,
    )) as Array<{ id: string; action: string }>;
    expect(held[0]).toMatchObject({ id: 's1', action: 'accept' });

    // ...the reviewer is told why, persistently rather than in a toast that vanishes...
    await expect(page.locator('#err')).toBeVisible();
    await expect(page.locator('#err')).toContainText('بەسەرچووە');
    // ...and they are moved on so they can keep working instead of tapping a dead clip.
    await expect(page.locator('#text')).toHaveValue('دەقی دووەم');
  });

  test('coming back to the page renews the lease immediately, not four minutes later', async ({ page }) => {
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
      expect(u, 'a tokenless page must not send an empty t= — it shadows the cookie').not.toMatch(/[?&]t=(&|$)/);
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
          return new Response('{"ok":true}', { status: 200, headers: { 'content-type': 'application/json' } });
        }
        if (url.includes('/api/queue')) {
          if (!up) return new Response('unauthorized', { status: 401 }); // no cookie was ever minted
          return new Response(
            JSON.stringify({
              reviewer: 'Sara',
              items: [{ id: 'c1', text: 'پارچە', durationMs: 1000, speakerId: null }],
              heldByOthers: 0,
            }),
            { status: 200, headers: { 'content-type': 'application/json' } },
          );
        }
        return new Response('{"ok":true}', { status: 200, headers: { 'content-type': 'application/json' } });
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

  test('a genuinely refused claim still shows link-expired, with no retry offered', async ({ page }) => {
    await page.addInitScript(() => {
      window.fetch = async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url.includes('/api/claim')) return new Response('unauthorized', { status: 401 }); // real verdict
        if (url.includes('/api/queue')) return new Response('unauthorized', { status: 401 });
        return new Response('{"ok":true}', { status: 200, headers: { 'content-type': 'application/json' } });
      };
    });
    await page.goto(PAGE + '#t=revoked-token');
    await page.reload();
    await expect(page.locator('#err')).toBeVisible({ timeout: 5000 });
    await expect(page.locator('#err')).toContainText('بەسەرچووە');
    await expect(page.locator('#retry')).toBeHidden(); // retrying a verdict would be a lie
  });

  test('a fragment token is claimed by POST and stripped, never sent in any request URL', async ({ page }) => {
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
    await page.waitForFunction(`(window.__reqs || []).some((r) => r.url.includes('/api/claim'))`, null, {
      timeout: 5000,
    });
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
                { id: 'q1', text: 'یەکەم', durationMs: 1000, speakerId: null },
                { id: 'q2', text: 'دووەم', durationMs: 1000, speakerId: null },
              ],
              heldByOthers: 0,
              pendingTotal: 407, // a real backlog behind a 2-clip batch
            }),
            { status: 200, headers: { 'content-type': 'application/json' } },
          );
        }
        return new Response('{"ok":true}', { status: 200, headers: { 'content-type': 'application/json' } });
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

  test('a server that sends no total still counts honestly, against the batch', async ({ page }) => {
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
              items: [{ id: 'q1', text: 'یەکەم', durationMs: 1000, speakerId: null }],
              heldByOthers: 0,
            }),
            { status: 200, headers: { 'content-type': 'application/json' } },
          );
        }
        return new Response('{"ok":true}', { status: 200, headers: { 'content-type': 'application/json' } });
      };
    });
    await page.goto(PAGE);
    await expect(page.locator('#text')).toHaveValue('یەکەم', { timeout: 5000 });
    await expect(page.locator('#progress')).toHaveText('پارچەی 1 لە 1');
  });

  test('the Retry button shows it is working, and a double tap costs one fetch', async ({ page }) => {
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
              items: [{ id: 'r1', text: 'پارچە', durationMs: 1000, speakerId: null }],
              heldByOthers: 0,
              pendingTotal: 1,
            }),
            { status: 200, headers: { 'content-type': 'application/json' } },
          );
        }
        return new Response('{"ok":true}', { status: 200, headers: { 'content-type': 'application/json' } });
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

  for (const scheme of ['light', 'dark'] as const) {
    test(`has zero WCAG 2.2 AA violations while reviewing (${scheme})`, async ({ page }) => {
      // Both themes, because the page renders in whichever the phone is set to and a contrast
      // failure in one is invisible from the other.
      await page.emulateMedia({ colorScheme: scheme });
      await showAClip(page);
      const results = await new AxeBuilder({ page }).withTags(WCAG_AA).analyze();
      for (const v of results.violations) {
        for (const n of v.nodes) {
          console.log(`[axe:${scheme}] ${v.id}: ${n.target.join(' ')} — ${n.failureSummary?.split('\n')[1] ?? ''}`);
        }
      }
      expect(results.violations.map((v) => `${v.id} x${v.nodes.length}`)).toEqual([]);
    });
  }
});
