import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { test } from 'node:test';
import istanbulCoverage from 'istanbul-lib-coverage';
import {
  canonicalSourcePath,
  criticalCoverageSummaries,
  projectBrowserCoverage,
  readRawAuthorityBundle,
  replayCliOptions,
  snapshotDifference,
  validateContract,
  validatePlaywrightReport,
} from './run_merged_frontend_coverage.mjs';

const { createCoverageMap } = istanbulCoverage;

const sha256 = (value) => createHash('sha256').update(value).digest('hex');

test('raw replay rejects a duplicate-key non-canonical bundle header', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'cortex-coverage-header-'));
  try {
    const content = Buffer.from('{}', 'utf8');
    const digest = sha256(content);
    const header = { bytes: content.length, path: 'unit/coverage-final.json', sha256: digest };
    const duplicateHeader = Buffer.from(
      `{"bytes":${content.length},"path":"unit/coverage-final.json",` +
        `"path":"unit/coverage-final.json","sha256":"${digest}"}`,
      'utf8',
    );
    const headerLength = Buffer.alloc(4);
    headerLength.writeUInt32BE(duplicateHeader.length);
    const contentLength = Buffer.alloc(8);
    contentLength.writeBigUInt64BE(BigInt(content.length));
    const bundle = Buffer.concat([
      Buffer.from('CORTEX_FRONTEND_COVERAGE_RAW_V1\n', 'utf8'),
      headerLength,
      duplicateHeader,
      contentLength,
      content,
    ]);
    const bundlePath = join(directory, 'frontend-coverage-raw.v1.bin');
    await writeFile(bundlePath, bundle, { flag: 'wx' });
    const manifest = {
      bundle: {
        format: 'CORTEX_FRONTEND_COVERAGE_RAW_V1',
        sha256: sha256(bundle),
        bytes: bundle.length,
        entries: [header],
      },
    };
    await assert.rejects(
      readRawAuthorityBundle(manifest, bundlePath),
      /malformed, duplicated, or unsafe/u,
    );
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

const lockedThresholds = {
  statements: 85,
  branches: 80,
  functions: 80,
  lines: 85,
};

const criticalThresholds = {
  statements: 95,
  branches: 90,
  functions: 90,
  lines: 95,
};

test('replay CLI accepts only the exact absolute copied-authority layout', () => {
  const manifest = resolve('copied', 'frontend-coverage-raw-manifest.json');
  const bundle = resolve('copied', 'frontend-coverage-raw.v1.bin');
  const temporaryParent = resolve('temporary-parent');
  assert.deepEqual(replayCliOptions([]), {});
  assert.deepEqual(
    replayCliOptions([
      '--manifest',
      manifest,
      '--bundle',
      bundle,
      '--temporary-parent',
      temporaryParent,
    ]),
    { manifestPath: manifest, bundlePath: bundle, temporaryParent },
  );
  for (const argv of [
    ['--manifest', manifest],
    ['--bundle', bundle, '--manifest', manifest, '--temporary-parent', temporaryParent],
    ['--manifest', 'relative.json', '--bundle', bundle, '--temporary-parent', temporaryParent],
    [
      '--manifest',
      `${manifest}\0suffix`,
      '--bundle',
      bundle,
      '--temporary-parent',
      temporaryParent,
    ],
  ]) {
    assert.throws(() => replayCliOptions(argv), /Frontend coverage gate: replay/u);
  }
});

function testContract(overrides = {}) {
  return {
    schema: 1,
    thresholds: lockedThresholds,
    criticalThresholds,
    criticalDomains: {
      fixture: ['src/example.ts'],
    },
    playwrightProject: 'chromium',
    viteOrigin: 'http://localhost:1420',
    minimumFullE2ETests: 2,
    minimumInstrumentedE2ETests: 1,
    standaloneE2EFiles: ['couch-page.spec.ts'],
    minimumE2EConvertedSourceFiles: 1,
    ...overrides,
  };
}

function reportTest(id, file) {
  return {
    id,
    file,
    title: id,
    tests: [
      {
        expectedStatus: 'passed',
        projectId: 'chromium',
        projectName: 'chromium',
        status: 'expected',
        results: [{ retry: 0, status: 'passed' }],
      },
    ],
  };
}

function cleanReport() {
  return {
    config: {
      projects: [{ id: 'chromium', name: 'chromium', retries: 0 }],
    },
    suites: [
      { file: 'app.spec.ts', specs: [reportTest('app-one', 'app.spec.ts')] },
      { file: 'couch-page.spec.ts', specs: [reportTest('couch-one', 'couch-page.spec.ts')] },
    ],
    errors: [],
    stats: { expected: 2, skipped: 0, unexpected: 0, flaky: 0 },
  };
}

test('accepts one exact clean Chromium attempt and separates standalone coverage', () => {
  const result = validatePlaywrightReport(cleanReport(), testContract());
  assert.equal(result.tests.length, 2);
  assert.deepEqual(
    result.instrumented.map(({ id }) => id),
    ['app-one'],
  );
});

test('rejects report-level retries even when the final attempt passed', () => {
  const report = cleanReport();
  report.config.projects[0].retries = 1;
  assert.throws(
    () => validatePlaywrightReport(report, testContract()),
    /report allowed 1 retries/u,
  );
});

test('rejects a retried test and a skipped full-suite result', () => {
  const retryReport = cleanReport();
  retryReport.suites[0].specs[0].tests[0].results[0].retry = 1;
  assert.throws(
    () => validatePlaywrightReport(retryReport, testContract()),
    /lacks one clean zero-retry pass/u,
  );

  const skippedReport = cleanReport();
  skippedReport.stats = { expected: 1, skipped: 1, unexpected: 0, flaky: 0 };
  assert.throws(
    () => validatePlaywrightReport(skippedReport, testContract()),
    /not a clean full pass/u,
  );
});

test('rejects a missing declared standalone suite and a reduced app campaign', () => {
  const report = cleanReport();
  report.suites.pop();
  report.stats.expected = 1;
  assert.throws(
    () =>
      validatePlaywrightReport(
        report,
        testContract({ minimumFullE2ETests: 1, minimumInstrumentedE2ETests: 1 }),
      ),
    /standalone E2E files were not run/u,
  );

  assert.throws(
    () => validatePlaywrightReport(cleanReport(), testContract({ minimumInstrumentedE2ETests: 2 })),
    /only 1 Vite-app E2E tests were selected/u,
  );
});

test('canonical source paths admit shipped TS/Svelte only', () => {
  const appRoot = resolve(import.meta.dirname, '..');
  assert.equal(
    canonicalSourcePath('http://127.0.0.1:1420/src/lib/example.ts?t=1'),
    resolve(appRoot, 'src', 'lib', 'example.ts'),
  );
  assert.equal(
    canonicalSourcePath(resolve(appRoot, 'src', 'App.svelte')),
    resolve(appRoot, 'src', 'App.svelte'),
  );
  assert.equal(canonicalSourcePath(resolve(appRoot, 'src', 'example.test.ts')), null);
  assert.equal(canonicalSourcePath(resolve(appRoot, 'e2e', 'app.spec.ts')), null);
});

test('snapshot comparison identifies added, removed, and changed inputs', () => {
  assert.deepEqual(
    snapshotDifference(
      {
        entries: [
          { path: 'removed.ts', sha256: 'a' },
          { path: 'changed.ts', sha256: 'b' },
        ],
      },
      {
        entries: [
          { path: 'changed.ts', sha256: 'c' },
          { path: 'added.ts', sha256: 'd' },
        ],
      },
    ),
    { added: ['added.ts'], removed: ['removed.ts'], changed: ['changed.ts'] },
  );
});

test('browser execution is projected onto one locked denominator without double counting', () => {
  const sharedOne = { start: { line: 1, column: 0 }, end: { line: 1, column: 2 } };
  const sharedTwo = { start: { line: 2, column: 0 }, end: { line: 2, column: 2 } };
  const browserWrapper = { start: { line: 3, column: 0 }, end: { line: 3, column: 2 } };
  const unitMap = createCoverageMap({
    'C:/app/src/example.ts': {
      path: 'C:/app/src/example.ts',
      statementMap: { 0: sharedOne, 1: sharedTwo },
      fnMap: {},
      branchMap: {
        0: {
          type: 'binary-expr',
          line: 1,
          loc: sharedOne,
          locations: [sharedOne, sharedTwo],
        },
      },
      s: { 0: 1, 1: 0 },
      f: {},
      b: { 0: [0, 0] },
    },
  });
  const browserMap = createCoverageMap({
    'C:/app/src/example.ts': {
      path: 'C:/app/src/example.ts',
      statementMap: { 0: sharedTwo, 1: browserWrapper },
      fnMap: {},
      branchMap: {
        0: { type: 'if', line: 2, loc: sharedTwo, locations: [sharedTwo] },
        1: { type: 'if', line: 3, loc: browserWrapper, locations: [browserWrapper] },
      },
      s: { 0: 1, 1: 1 },
      f: {},
      b: { 0: [1], 1: [1] },
    },
  });
  const { coverageMap, semanticMatch } = projectBrowserCoverage(
    unitMap,
    browserMap,
    testContract(),
  );
  assert.deepEqual(coverageMap.getCoverageSummary().toJSON().statements, {
    total: 2,
    covered: 2,
    skipped: 0,
    pct: 100,
  });
  assert.deepEqual(semanticMatch.statements, {
    incomingItems: 2,
    matchedItems: 1,
    unmatchedItems: 1,
    pct: 50,
  });
  assert.deepEqual(coverageMap.getCoverageSummary().toJSON().branches, {
    total: 2,
    covered: 1,
    skipped: 0,
    pct: 50,
  });
  assert.deepEqual(semanticMatch.branches, {
    incomingItems: 2,
    matchedItems: 1,
    unmatchedItems: 1,
    pct: 50,
  });
});

test('contract validation fails closed on incomplete metric authority', () => {
  assert.throws(
    () =>
      validateContract(testContract({ thresholds: { ...lockedThresholds, branches: Number.NaN } })),
    /invalid thresholds\.branches threshold/u,
  );
  assert.throws(
    () => validateContract(testContract({ standaloneE2EFiles: 'couch-page.spec.ts' })),
    /invalid standaloneE2EFiles/u,
  );
  assert.throws(
    () => validateContract(testContract({ viteOrigin: 'http://localhost:1420/not-an-origin' })),
    /invalid Vite origin/u,
  );
  assert.throws(
    () =>
      validateContract(
        testContract({ criticalThresholds: { ...criticalThresholds, functions: 89 / 0 } }),
      ),
    /invalid criticalThresholds\.functions threshold/u,
  );
  assert.throws(
    () =>
      validateContract(
        testContract({
          criticalDomains: { one: ['src/example.ts'], two: ['src/example.ts'] },
        }),
      ),
    /invalid or overlapping critical domain two/u,
  );
});

test('critical domain summaries use exact non-empty source denominators', () => {
  const location = { start: { line: 1, column: 0 }, end: { line: 1, column: 2 } };
  const file = resolve(import.meta.dirname, '..', 'src', 'example.ts');
  const map = createCoverageMap({
    [file]: {
      path: file,
      statementMap: { 0: location },
      fnMap: {
        0: { name: 'example', decl: location, loc: location, line: 1 },
      },
      branchMap: {
        0: { type: 'if', line: 1, loc: location, locations: [location, location] },
      },
      s: { 0: 1 },
      f: { 0: 1 },
      b: { 0: [1, 0] },
    },
  });
  const summaries = criticalCoverageSummaries(map, testContract());
  assert.equal(summaries.fixture.lines.pct, 100);
  assert.equal(summaries.fixture.functions.pct, 100);
  assert.equal(summaries.fixture.branches.pct, 50);

  assert.throws(
    () =>
      criticalCoverageSummaries(
        map,
        testContract({ criticalDomains: { missing: ['src/missing.ts'] } }),
      ),
    /source is absent from the coverage denominator/u,
  );
});
