import { test as base, expect } from '@playwright/test';
import { createHash } from 'node:crypto';
import { mkdir, writeFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { gzipSync } from 'node:zlib';
import { installTauriMock } from './helpers/tauri-mock';

export const test = base.extend({
  page: async ({ page }, use, testInfo) => {
    const collectCoverage = process.env.CORTEX_E2E_COVERAGE === '1';
    const runToken = process.env.CORTEX_E2E_COVERAGE_RUN_TOKEN;
    const sourceTreeSha256 = process.env.CORTEX_E2E_SOURCE_TREE_SHA256;
    const coverageOrigin = process.env.CORTEX_E2E_COVERAGE_ORIGIN;
    if (collectCoverage && (!runToken || !sourceTreeSha256 || !coverageOrigin)) {
      throw new Error('E2E coverage requires a run token, source-tree digest, and Vite origin');
    }
    if (collectCoverage) {
      await page.coverage.startJSCoverage({
        resetOnNavigation: false,
        reportAnonymousScripts: false,
      });
    }
    await page.addInitScript(() => {
      localStorage.setItem('cortex-locale', 'en');
    });
    await installTauriMock(page);
    let testFailure: unknown;
    try {
      await use(page);
    } catch (error) {
      testFailure = error;
    }
    let coverageFailure: unknown;
    if (collectCoverage) {
      try {
        const expectedOrigin = new URL(coverageOrigin!).origin;
        const captured = (await page.coverage.stopJSCoverage()).filter((entry) => {
          try {
            const url = new URL(entry.url);
            return (
              url.origin === expectedOrigin &&
              url.pathname.startsWith('/src/') &&
              /\.(?:svelte|ts)$/u.test(url.pathname)
            );
          } catch {
            return false;
          }
        });
        const entries = [];
        for (const entry of captured) {
          let source = entry.source;
          // Chromium may release the source body for a dynamically imported module before coverage
          // stops. The V8 ranges remain valid, but persisting an undefined body makes later AST/source
          // map conversion impossible. Re-read the exact same campaign-bound Vite URL while the
          // source tree is still hash-locked; never substitute the raw .svelte/.ts file because V8's
          // offsets describe Vite's transformed module, not the authoring source.
          if (typeof source !== 'string') {
            const response = await fetch(entry.url, { cache: 'no-store' });
            if (!response.ok) {
              throw new Error(
                `could not recover executed coverage source ${entry.url}: HTTP ${response.status}`,
              );
            }
            source = await response.text();
          }
          if (!source) {
            throw new Error(`executed coverage source ${entry.url} is empty`);
          }
          entries.push({ ...entry, source });
        }
        const coverageRoot = resolve(process.cwd(), 'coverage', 'e2e-raw');
        await mkdir(coverageRoot, { recursive: true });
        const identity = createHash('sha256')
          .update(`${runToken}:${testInfo.testId}:${testInfo.project.name}:${testInfo.retry}`)
          .digest('hex');
        await writeFile(
          resolve(coverageRoot, `${identity}.json.gz`),
          gzipSync(
            JSON.stringify({
              schema: 2,
              runToken,
              sourceTreeSha256,
              testId: testInfo.testId,
              projectName: testInfo.project.name,
              retry: testInfo.retry,
              entries,
            }),
          ),
          { flag: 'wx' },
        );
      } catch (error) {
        coverageFailure = error;
      }
    }
    // Preserve the product-test failure as primary. On a clean test, any coverage collection defect
    // is itself a gate failure; on a red test, the campaign is already non-certifying and must report
    // the behavior failure that caused it rather than masking it with teardown evidence.
    if (testFailure) throw testFailure;
    if (coverageFailure) throw coverageFailure;
  },
});

export { expect };
