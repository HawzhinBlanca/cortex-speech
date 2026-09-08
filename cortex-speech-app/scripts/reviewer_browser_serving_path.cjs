// Explicit browser/backend gate launched by reviewer_serving_path.rs. All data is synthetic.
// Never run against a live profile: the Rust parent owns profile creation and all DB assertions.
const { chromium, expect } = require('@playwright/test');
const { readFileSync, writeFileSync, existsSync } = require('node:fs');
const { resolve, relative, isAbsolute } = require('node:path');
const process = require('node:process');
const console = require('node:console');
const { URL } = require('node:url');

async function twoTabs(context, base, token, reportPath) {
  const origin = new URL(base).origin;
  const pages = [await context.newPage(), await context.newPage()];
  const errors = [];
  pages.forEach((page) => page.on('pageerror', (error) => errors.push(error.message)));
  let replay = false;
  const held = [];
  const replayBarrier = [];
  const responses = [];
  await context.route('**/*', async (route) => {
    const request = route.request();
    const url = new URL(request.url());
    if (url.origin !== origin) return route.abort('blockedbyclient');
    if (url.pathname !== '/api/decision') return route.continue();
    const operation = request.postDataJSON();
    if (!replay) {
      // Barrier: both tabs must durably queue their own operation before either fails.
      await new Promise((resolve) => {
        held.push({ operation, release: resolve });
        if (held.length >= 2) held.forEach((item) => item.release());
      });
      return route.abort('failed');
    }
    if (replayBarrier.length < 2) {
      await new Promise((resolve) => {
        replayBarrier.push({ operation, release: resolve });
        if (replayBarrier.length === 2) {
          expect(replayBarrier[0].operation).toEqual(replayBarrier[1].operation);
          replayBarrier.forEach((item) => item.release());
        }
      });
    }
    const response = await route.fetch({ maxRetries: 0 });
    responses.push({ operation, status: response.status(), body: await response.text() });
    return route.fulfill({ response });
  });
  const queues = [];
  for (const [index, page] of pages.entries()) {
    const queue = page.waitForResponse(
      (response) => new URL(response.url()).pathname === '/api/queue' && response.status() === 200,
    );
    await page.goto(base + '/' + (index === 0 ? '#t=' + encodeURIComponent(token) : ''));
    queues.push(await (await queue).json());
    await expect(page.locator('#who')).toContainText('Fixture-Reviewer-One');
    await expect(page.locator('#text')).toHaveValue(queues[index].items[0].text);
  }
  expect(queues[1].items[0].id).toBe(queues[0].items[0].id);
  const drafts = queues.map((queue, index) => queue.items[0].text + [' یەک', ' دوو'][index]);
  await Promise.all(
    pages.map(async (page, index) => {
      await page.locator('#play').click();
      await expect.poll(() => page.locator('#player').evaluate((audio) => audio.ended)).toBe(true);
      await page.locator('#text').fill(drafts[index]);
      const finalize = page.waitForResponse(
        (response) => new URL(response.url()).pathname === '/api/playback/finalize',
      );
      await page.locator('#save').click();
      expect((await finalize).status()).toBe(200);
    }),
  );
  const outbox = (page) =>
    page.evaluate(() =>
      Object.keys(globalThis.localStorage)
        .filter((key) => key.startsWith('cortex.couch.outbox.operation.'))
        .map((key) => JSON.parse(globalThis.localStorage.getItem(key)).submission),
    );
  await expect.poll(async () => (await outbox(pages[0])).length).toBe(2);
  const operations = await outbox(pages[0]);
  expect(new Set(operations.map((item) => item.operationId)).size).toBe(2);
  expect(operations.map((item) => item.text).sort()).toEqual([...drafts].sort());
  for (const operation of operations) expect(operation.playbackReceiptId).toBeTruthy();
  await expect.poll(() => held.length).toBe(2);
  // Both real pages restore their shared outbox and race replay. The server owns arbitration.
  replay = true;
  await Promise.all(pages.map((page) => page.reload()));
  await expect.poll(async () => (await outbox(pages[0])).length).toBe(0);
  await expect.poll(() => responses.some((response) => response.status === 409)).toBe(true);
  const winning = responses.find((response) => response.status === 200)?.operation;
  expect(winning).toBeTruthy();
  await expect
    .poll(
      () =>
        responses.filter((response) => response.operation.operationId === winning.operationId)
          .length,
    )
    .toBeGreaterThanOrEqual(2);
  const losing = operations.find((operation) => operation.operationId !== winning.operationId);
  const losingPage = pages[drafts.indexOf(losing.text)];
  const draftValues = await losingPage.evaluate(() => Object.values(globalThis.sessionStorage));
  expect(draftValues).toContain(losing.text);
  await expect(losingPage.locator('#draftRecovery')).toBeVisible();
  await losingPage.locator('#draftRecoveryTitle').click();
  await losingPage.locator('#draftRecoveryList').selectOption('cortex.couch.draft.' + losing.id);
  await expect(losingPage.locator('#draftRecoveryText')).toHaveValue(losing.text);
  await expect(losingPage.locator('#draftRecoveryText')).toHaveAttribute('readonly', '');
  await context.grantPermissions(['clipboard-read', 'clipboard-write']);
  await losingPage.locator('#draftRecoveryText').focus();
  await losingPage.keyboard.press('ControlOrMeta+c');
  expect(await losingPage.evaluate(() => globalThis.navigator.clipboard.readText())).toBe(
    losing.text,
  );
  await expect(losingPage.locator('#err')).toBeVisible();
  expect(await losingPage.evaluate('errKind')).toBe('refused');
  for (const response of responses) {
    if (response.operation.operationId === winning.operationId) {
      expect(response.status, JSON.stringify(response)).toBe(200);
    } else {
      expect(response.status, JSON.stringify(response)).toBe(409);
    }
    expect(response.operation).toEqual(
      operations.find((item) => item.operationId === response.operation.operationId),
    );
  }
  expect(errors).toEqual([]);
  writeFileSync(
    reportPath,
    JSON.stringify({ operations, winning, losing, responses, pageErrors: errors }, null, 2),
  );
  await context.close();
}

async function reopenedRound(context, base, token, profile, reportPath) {
  const origin = new URL(base).origin;
  const page = await context.newPage();
  const errors = [];
  page.on('pageerror', (error) => errors.push(error.message));
  let replay = false;
  const responses = [];
  const requests = [];
  await context.route('**/*', async (route) => {
    const request = route.request();
    const url = new URL(request.url());
    if (url.origin !== origin) return route.abort('blockedbyclient');
    if (url.pathname !== '/api/decision') return route.continue();
    const operation = request.postDataJSON();
    requests.push(operation);
    if (!replay) return route.abort('failed');
    const response = await route.fetch({ maxRetries: 0 });
    responses.push({ operation, status: response.status(), body: await response.text() });
    return route.fulfill({ response });
  });
  const firstQueue = page.waitForResponse(
    (r) => new URL(r.url()).pathname === '/api/queue' && r.status() === 200,
  );
  await page.goto(base + '/#t=' + encodeURIComponent(token));
  const served = await (await firstQueue).json();
  await expect(page.locator('#who')).toContainText('Fixture-Reviewer-Two');
  const clip = served.items[0];
  expect(clip.id).toBe('serving-path-first');
  await expect(page.locator('#text')).toHaveValue(clip.text);
  await page.locator('#play').click();
  await expect.poll(() => page.locator('#player').evaluate((audio) => audio.ended)).toBe(true);
  const oldText = clip.text + ' کۆن';
  await page.locator('#text').fill(oldText);
  const finalize = page.waitForResponse(
    (r) => new URL(r.url()).pathname === '/api/playback/finalize',
  );
  await page.locator('#save').click();
  expect((await finalize).status()).toBe(200);
  const outbox = () =>
    page.evaluate(() =>
      Object.keys(globalThis.localStorage)
        .filter((key) => key.startsWith('cortex.couch.outbox.operation.'))
        .map((key) => JSON.parse(globalThis.localStorage.getItem(key)).submission),
    );
  await expect.poll(async () => (await outbox()).length).toBe(1);
  await expect.poll(() => requests.length).toBeGreaterThan(0);
  const operation = (await outbox())[0];
  expect(operation).toEqual(requests[0]);
  expect(operation.playbackReceiptId).toBeTruthy();
  // The Rust parent now shuts down only the disposable server, runs the actual offline owner
  // preview/apply CLI (including snapshot), and resumes it. This tab and its draft remain alive.
  writeFileSync(resolve(profile, 'reopen-ready.json'), JSON.stringify(operation));
  await expect
    .poll(() => existsSync(resolve(profile, 'reopen-applied.json')), { timeout: 45000 })
    .toBe(true);
  const owner = JSON.parse(readFileSync(resolve(profile, 'reopen-applied.json'), 'utf8'));
  expect(owner.reopenedOrAlreadyApplied).toBe(1);
  expect(owner.paymentHistoryChanged).toBe(false);
  expect(owner.preReopenPinnedSnapshot).toBeTruthy();
  replay = true;
  const returnedQueue = page.waitForResponse(
    (r) => new URL(r.url()).pathname === '/api/queue' && r.status() === 200,
  );
  await page.reload();
  const returned = await (await returnedQueue).json();
  const fresh = returned.items.find((item) => item.id === operation.id);
  expect(fresh).toBeTruthy();
  expect(fresh.redo).toBe(true);
  expect(fresh.rowVersion).not.toBe(operation.rowVersion);
  await expect.poll(async () => (await outbox()).length).toBe(0);
  await expect
    .poll(() =>
      responses.some((response) => response.operation.operationId === operation.operationId),
    )
    .toBe(true);
  const staleResponses = responses.filter(
    (response) => response.operation.operationId === operation.operationId,
  );
  for (const response of staleResponses) expect(response.status, response.body).toBe(409);
  for (const request of requests.filter((request) => request.operationId === operation.operationId))
    expect(request).toEqual(operation);
  await expect(page.locator('#text')).toHaveValue(fresh.text);
  expect(fresh.text).not.toBe(oldText);
  await expect(page.locator('#returnBanner')).toBeVisible();
  await expect(page.locator('#draftRecovery')).toBeVisible();
  await page.locator('#draftRecoveryTitle').click();
  await expect(page.locator('#draftRecoveryText')).toHaveValue(oldText);
  await context.grantPermissions(['clipboard-read', 'clipboard-write']);
  await page.locator('#draftRecoveryText').focus();
  await page.keyboard.press('ControlOrMeta+c');
  expect(await page.evaluate(() => globalThis.navigator.clipboard.readText())).toBe(oldText);
  await expect(page.locator('#text')).toHaveValue(fresh.text);
  // Prove usable recovery, not merely refusal: earn fresh listening authority and submit a new
  // operation in this round. Copying alone above minted no new outbox operation.
  expect(await outbox()).toEqual([]);
  await page.locator('#play').click();
  await expect.poll(() => page.locator('#player').evaluate((audio) => audio.ended)).toBe(true);
  const freshText = fresh.text + ' نوێ';
  await page.locator('#text').fill(freshText);
  const saved = page.waitForResponse(
    (r) => new URL(r.url()).pathname === '/api/decision' && r.status() === 200,
  );
  await page.locator('#save').click();
  const saveResponse = await saved;
  const newOperation = saveResponse.request().postDataJSON();
  const newReply = await saveResponse.json();
  expect(newOperation.operationId).not.toBe(operation.operationId);
  expect(newOperation.playbackReceiptId).not.toBe(operation.playbackReceiptId);
  expect(newOperation.rowVersion).toBe(fresh.rowVersion);
  expect(newOperation.text).toBe(freshText);
  expect(newReply.poolDecisionId).toBeGreaterThan(0);
  await expect.poll(async () => (await outbox()).length).toBe(0);
  const duplicate = await page.evaluate(async (submission) => {
    const response = await globalThis.fetch('/api/decision', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(submission),
    });
    return { status: response.status, body: await response.json() };
  }, newOperation);
  expect(duplicate.status).toBe(200);
  expect(duplicate.body.duplicate).toBe(true);
  expect(duplicate.body.poolDecisionId).toBe(newReply.poolDecisionId);
  expect(errors).toEqual([]);
  writeFileSync(
    reportPath,
    JSON.stringify(
      { operation, fresh, newOperation, newReply, staleResponses, pageErrors: errors },
      null,
      2,
    ),
  );
  await context.close();
}

async function main() {
  const [phase, base, profile, reportPath] = process.argv.slice(2);
  const url = new URL(base);
  if (
    url.protocol !== 'https:' ||
    url.hostname !== '127.0.0.1' ||
    !url.port ||
    url.port === '8737'
  ) {
    throw new Error('Only a non-production loopback HTTPS fixture is allowed');
  }
  const marker = JSON.parse(readFileSync(resolve(profile, 'browser-fixture.json'), 'utf8'));
  if (marker.base !== base || marker.synthetic !== true)
    throw new Error('Missing fixture ownership marker');
  const reportRelative = relative(resolve(profile), resolve(reportPath));
  if (reportRelative.startsWith('..') || isAbsolute(reportRelative))
    throw new Error('Report escapes fixture');
  if (!['request-lost', 'response-lost', 'replay', 'two-tabs', 'owner-reopen'].includes(phase))
    throw new Error('Unknown fixture phase');

  const statePath = resolve(profile, 'browser-state.json');
  const browser = await chromium.launch({ headless: true });
  try {
    // The generated fixture certificate is not publicly trusted. This setting belongs only to this
    // disposable context; every request outside the exact loopback origin is aborted below.
    const context = await browser.newContext({
      ignoreHTTPSErrors: true,
      viewport: { width: 390, height: 844 },
      storageState: ['request-lost', 'two-tabs', 'owner-reopen'].includes(phase)
        ? undefined
        : statePath,
    });
    if (phase === 'two-tabs') {
      const token = process.env.CORTEX_BROWSER_FIXTURE_TOKEN;
      if (!token) throw new Error('Synthetic pairing token required');
      return await twoTabs(context, base, token, reportPath);
    }
    if (phase === 'owner-reopen') {
      const token = process.env.CORTEX_BROWSER_FIXTURE_TOKEN;
      if (!token) throw new Error('Synthetic pairing token required');
      return await reopenedRound(context, base, token, profile, reportPath);
    }
    const requests = [];
    let acknowledged = null;
    let forwarded = false;
    await context.route('**/*', async (route) => {
      const request = route.request();
      const requestUrl = new URL(request.url());
      if (requestUrl.origin !== url.origin) return route.abort('blockedbyclient');
      if (requestUrl.pathname !== '/api/decision') return route.continue();
      requests.push(request.postDataJSON());
      if (phase === 'request-lost' || (phase === 'response-lost' && forwarded))
        return route.abort('failed');
      forwarded = true;
      // maxRetries=0: the harness must not silently retry a POST on the browser's behalf.
      const response = await route.fetch({ maxRetries: 0 });
      expect(response.status()).toBe(200);
      acknowledged = await response.json();
      if (phase === 'response-lost') return route.abort('failed');
      return route.fulfill({ response });
    });
    const page = await context.newPage();
    const errors = [];
    page.on('pageerror', (error) => errors.push(error.message));
    const token = process.env.CORTEX_BROWSER_FIXTURE_TOKEN;
    if (phase === 'request-lost' && !token) throw new Error('Synthetic pairing token required');
    const queueResponse = page.waitForResponse(
      (response) => new URL(response.url()).pathname === '/api/queue' && response.status() === 200,
    );
    await page.goto(
      base + '/' + (phase === 'request-lost' ? '#t=' + encodeURIComponent(token) : ''),
    );
    const served = await (await queueResponse).json();
    await expect(page.locator('#who')).toContainText('Fixture-Reviewer-One');
    const outbox = () =>
      page.evaluate(() =>
        Object.keys(globalThis.localStorage)
          .filter((key) => key.startsWith('cortex.couch.outbox.operation.'))
          .map((key) => JSON.parse(globalThis.localStorage.getItem(key)).submission),
      );

    if (phase === 'request-lost') {
      // Flexible pool order is not fixed. Review the actual first served fixture clip.
      await expect(page.locator('#text')).toHaveValue(served.items[0].text);
      await page.locator('#play').click();
      await expect.poll(() => page.locator('#player').evaluate((audio) => audio.ended)).toBe(true);
      await page.locator('#text').fill(served.items[0].text + ' نوێ');
      const finalization = page.waitForResponse(
        (response) => new URL(response.url()).pathname === '/api/playback/finalize',
      );
      await page.locator('#save').click();
      const finalized = await finalization;
      expect(
        finalized.status(),
        JSON.stringify({
          response: await finalized.text(),
          request: finalized.request().postDataJSON(),
        }),
      ).toBe(200);
      await expect.poll(() => requests.length).toBeGreaterThan(0);
      await expect.poll(async () => (await outbox()).length).toBe(1);
      expect(forwarded).toBe(false);
    } else {
      await expect.poll(() => acknowledged).not.toBeNull();
      expect(Boolean(acknowledged.duplicate)).toBe(phase === 'replay');
      await expect.poll(async () => (await outbox()).length).toBe(phase === 'replay' ? 0 : 1);
    }
    expect(errors).toEqual([]);
    const operation = requests[0];
    const expected =
      phase === 'request-lost'
        ? { id: served.items[0].id, action: 'edit', text: served.items[0].text + ' نوێ' }
        : JSON.parse(readFileSync(resolve(profile, 'browser-request-lost.json'), 'utf8')).operation;
    expect(operation).toMatchObject(expected);
    expect(operation.playbackReceiptId).toBeTruthy();
    for (const request of requests) expect(request).toEqual(operation);
    let undone = null;
    if (phase === 'replay') {
      await expect(page.locator('#undo')).toBeVisible();
      const reply = page.waitForResponse(
        (response) => new URL(response.url()).pathname === '/api/undo',
      );
      await page.locator('#undo').click();
      const response = await reply;
      expect(response.status()).toBe(200);
      undone = await response.json();
      expect(undone.id).toBe(operation.id);
      await expect(page.locator('#text')).toBeVisible();
    }
    await context.storageState({ path: statePath });
    writeFileSync(
      reportPath,
      JSON.stringify(
        {
          phase,
          operation,
          acknowledged,
          undone,
          outbox: await outbox(),
          undoVisible: await page.locator('#undo').isVisible(),
          pageErrors: errors,
        },
        null,
        2,
      ),
    );
    await context.close();
  } finally {
    await browser.close();
  }
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
