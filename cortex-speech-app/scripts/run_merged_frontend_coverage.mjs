import { createHash, randomUUID } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import { closeSync, fsyncSync, mkdirSync, openSync, writeSync } from 'node:fs';
import { mkdir, mkdtemp, open, readFile, readdir, rename, rm, writeFile } from 'node:fs/promises';
import { gunzipSync } from 'node:zlib';
import { dirname, extname, isAbsolute, join, normalize, relative, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { convert } from 'ast-v8-to-istanbul';
import { mergeScriptCovs } from '@bcoe/v8-coverage';
import istanbulCoverage from 'istanbul-lib-coverage';
import istanbulReport from 'istanbul-lib-report';
import istanbulReports from 'istanbul-reports';
import { parseAstAsync } from 'vite';
import contract from './frontend_coverage_contract.v1.json' with { type: 'json' };

const { createCoverageMap } = istanbulCoverage;
const { createContext } = istanbulReport;
const { create: createReport } = istanbulReports;

const appRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const sourceRoot = join(appRoot, 'src');
const coverageRoot = join(appRoot, 'coverage');
const unitRoot = join(coverageRoot, 'unit');
const rawRoot = join(coverageRoot, 'e2e-raw');
const mergedRoot = join(coverageRoot, 'merged');
const playwrightReportPath = join(rawRoot, 'playwright-report.json');
const rawAuthorityRoot = join(coverageRoot, 'raw-authority');
const rawAuthorityManifestPath = join(coverageRoot, 'frontend-coverage-raw-manifest.json');
const rawAuthorityBundlePath = join(coverageRoot, 'frontend-coverage-raw.v1.bin');
const publishedArtifacts = [
  join(coverageRoot, 'coverage-final.json'),
  join(coverageRoot, 'coverage-summary.json'),
  join(coverageRoot, 'frontend-coverage-evidence.json'),
  rawAuthorityManifestPath,
  rawAuthorityBundlePath,
];

function fail(message) {
  throw new Error(`Frontend coverage gate: ${message}`);
}

function sha256(value) {
  return createHash('sha256').update(value).digest('hex');
}

function validateContract(candidate = contract) {
  if (candidate?.schema !== 1) fail('unsupported coverage contract schema');
  for (const thresholdSet of ['thresholds', 'criticalThresholds']) {
    for (const metric of ['statements', 'branches', 'functions', 'lines']) {
      const value = candidate[thresholdSet]?.[metric];
      if (typeof value !== 'number' || !Number.isFinite(value) || value < 0 || value > 100) {
        fail(`coverage contract has an invalid ${thresholdSet}.${metric} threshold`);
      }
    }
  }
  if (
    !candidate.criticalDomains ||
    typeof candidate.criticalDomains !== 'object' ||
    Array.isArray(candidate.criticalDomains) ||
    Object.keys(candidate.criticalDomains).length === 0
  ) {
    fail('coverage contract has no critical domains');
  }
  const criticalSources = new Set();
  for (const [domain, sources] of Object.entries(candidate.criticalDomains)) {
    if (
      !domain ||
      !Array.isArray(sources) ||
      sources.length === 0 ||
      sources.some(
        (source) =>
          typeof source !== 'string' ||
          !source.startsWith('src/') ||
          !['.ts', '.svelte'].includes(extname(source)) ||
          criticalSources.has(source),
      )
    ) {
      fail(`coverage contract has an invalid or overlapping critical domain ${domain}`);
    }
    for (const source of sources) criticalSources.add(source);
  }
  if (typeof candidate.playwrightProject !== 'string' || !candidate.playwrightProject) {
    fail('coverage contract has no Playwright project');
  }
  try {
    const viteOrigin = new URL(candidate.viteOrigin);
    if (
      !['http:', 'https:'].includes(viteOrigin.protocol) ||
      viteOrigin.origin !== candidate.viteOrigin
    ) {
      fail('coverage contract has an invalid Vite origin');
    }
  } catch {
    fail('coverage contract has an invalid Vite origin');
  }
  for (const field of [
    'minimumFullE2ETests',
    'minimumInstrumentedE2ETests',
    'minimumE2EConvertedSourceFiles',
  ]) {
    if (!Number.isInteger(candidate[field]) || candidate[field] <= 0) {
      fail(`coverage contract has an invalid ${field}`);
    }
  }
  if (
    !Array.isArray(candidate.standaloneE2EFiles) ||
    candidate.standaloneE2EFiles.some((file) => typeof file !== 'string' || !file)
  ) {
    fail('coverage contract has invalid standaloneE2EFiles');
  }
}

function runNode(script, args, extraEnvironment, logPath) {
  mkdirSync(dirname(logPath), { recursive: true });
  const argv = [process.execPath, script, ...args];
  const descriptor = openSync(logPath, 'wx');
  let result;
  try {
    writeSync(
      descriptor,
      `${JSON.stringify({ argv, cwd: appRoot, environment: extraEnvironment })}\n`,
      undefined,
      'utf8',
    );
    result = spawnSync(argv[0], argv.slice(1), {
      cwd: appRoot,
      env: { ...process.env, ...extraEnvironment },
      stdio: ['ignore', descriptor, descriptor],
      shell: false,
    });
    writeSync(
      descriptor,
      `\n${JSON.stringify({ status: result.status, signal: result.signal, error: result.error?.message ?? null })}\n`,
      undefined,
      'utf8',
    );
    fsyncSync(descriptor);
  } finally {
    closeSync(descriptor);
  }
  if (!result) fail(`${script} produced no process result`);
  if (result.error) fail(`${script} could not start: ${result.error.message}`);
  if (result.signal) fail(`${script} terminated by ${result.signal}`);
  if (result.status !== 0) fail(`${script} exited ${String(result.status)}`);
  return {
    argv,
    cwd: '.',
    environment: extraEnvironment,
    logPath: relative(coverageRoot, logPath).replaceAll('\\', '/'),
    status: result.status,
    signal: result.signal,
  };
}

function assertDisposableTarget(target) {
  const rel = relative(coverageRoot, target);
  if (!rel || rel.startsWith('..') || isAbsolute(rel)) {
    fail(`refusing to clear non-disposable path ${target}`);
  }
}

async function resetArtifacts() {
  await mkdir(coverageRoot, { recursive: true });
  for (const target of [unitRoot, rawRoot, mergedRoot, rawAuthorityRoot]) {
    assertDisposableTarget(target);
    await rm(target, { recursive: true, force: true });
  }
  for (const target of publishedArtifacts) {
    assertDisposableTarget(target);
    await rm(target, { force: true });
  }
}

async function writeRawAuthorityBundle(sourceFiles) {
  const magic = Buffer.from('CORTEX_FRONTEND_COVERAGE_RAW_V1\n', 'utf8');
  const temporary = `${rawAuthorityBundlePath}.${process.pid}.${randomUUID()}.tmp`;
  const handle = await open(temporary, 'wx');
  const entries = [];
  try {
    await handle.write(magic);
    for (const source of [...sourceFiles].sort((left, right) =>
      left.path.localeCompare(right.path),
    )) {
      const bytes = await readFile(source.file);
      const header = {
        bytes: bytes.length,
        path: source.path,
        sha256: sha256(bytes),
      };
      const headerBytes = Buffer.from(stableJson(header), 'utf8');
      const headerLength = Buffer.alloc(4);
      headerLength.writeUInt32BE(headerBytes.length);
      const contentLength = Buffer.alloc(8);
      contentLength.writeBigUInt64BE(BigInt(bytes.length));
      await handle.write(headerLength);
      await handle.write(headerBytes);
      await handle.write(contentLength);
      await handle.write(bytes);
      entries.push(header);
    }
    await handle.sync();
    await handle.close();
    await rename(temporary, rawAuthorityBundlePath);
  } catch (error) {
    await handle.close().catch(() => undefined);
    await rm(temporary, { force: true }).catch(() => undefined);
    throw error;
  }
  const bundleBytes = await readFile(rawAuthorityBundlePath);
  return {
    entries,
    sha256: sha256(bundleBytes),
    bytes: bundleBytes.length,
  };
}

async function atomicWriteFile(path, bytes) {
  await mkdir(dirname(path), { recursive: true });
  const temporary = `${path}.${process.pid}.${randomUUID()}.tmp`;
  let handle;
  try {
    handle = await open(temporary, 'wx');
    await handle.writeFile(bytes);
    await handle.sync();
    await handle.close();
    handle = undefined;
    await rename(temporary, path);
  } finally {
    await handle?.close().catch(() => undefined);
    await rm(temporary, { force: true }).catch(() => undefined);
  }
}

async function walk(directory) {
  const entries = await readdir(directory, { withFileTypes: true });
  const files = [];
  for (const entry of entries) {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) files.push(...(await walk(path)));
    else if (entry.isFile()) files.push(path);
  }
  return files;
}

function canonicalSourcePath(rawPath) {
  if (typeof rawPath !== 'string' || !rawPath) return null;
  let candidate = rawPath;
  try {
    if (/^https?:\/\//u.test(candidate)) {
      const url = new URL(candidate);
      if (!url.pathname.startsWith('/src/')) return null;
      candidate = join(appRoot, decodeURIComponent(url.pathname.slice(1)));
    } else if (candidate.startsWith('file://')) {
      candidate = fileURLToPath(candidate);
    } else if (/^\/[A-Za-z]:\//u.test(candidate)) {
      candidate = candidate.slice(1);
    }
  } catch {
    return null;
  }
  const canonical = normalize(isAbsolute(candidate) ? candidate : resolve(appRoot, candidate));
  const rel = relative(sourceRoot, canonical);
  if (!rel || rel.startsWith('..') || isAbsolute(rel)) return null;
  if (!['.ts', '.svelte'].includes(extname(canonical))) return null;
  if (/\.d\.ts$/u.test(canonical) || /\.(?:test|spec)\.[^.]+$/u.test(canonical)) return null;
  return canonical;
}

function normalizedCoverageData(data) {
  const normalizedMap = createCoverageMap({});
  for (const [key, value] of Object.entries(data)) {
    const canonical = canonicalSourcePath(value?.path ?? key);
    if (!canonical) continue;
    normalizedMap.addFileCoverage({ ...value, path: canonical });
  }
  return normalizedMap.toJSON();
}

function stableJson(value) {
  if (Array.isArray(value)) return `[${value.map(stableJson).join(',')}]`;
  if (value && typeof value === 'object') {
    return `{${Object.keys(value)
      .sort()
      .map((key) => `${JSON.stringify(key)}:${stableJson(value[key])}`)
      .join(',')}}`;
  }
  return JSON.stringify(value);
}

function coverageCountWeight(value, description) {
  const values = Array.isArray(value) ? value : [value];
  if (
    values.length === 0 ||
    values.some((count) => !Number.isFinite(count) || count < 0 || !Number.isInteger(count))
  ) {
    fail(`${description} has malformed execution counts`);
  }
  return values.length;
}

function unionCount(base, incoming, description) {
  coverageCountWeight(base, description);
  coverageCountWeight(incoming, description);
  if (Array.isArray(base) !== Array.isArray(incoming)) {
    fail(`${description} changed count shape between unit and E2E coverage`);
  }
  if (Array.isArray(base)) {
    if (base.length !== incoming.length) {
      fail(`${description} changed branch arity between unit and E2E coverage`);
    }
    return base.map((count, index) => Math.max(count, incoming[index]));
  }
  return Math.max(base, incoming);
}

function projectMetric(baseMap, baseCounts, incomingMap, incomingCounts, description) {
  const baseIds = Object.keys(baseMap);
  const incomingIds = Object.keys(incomingMap);
  if (
    baseIds.some((id) => !Object.hasOwn(baseCounts, id)) ||
    Object.keys(baseCounts).some((id) => !Object.hasOwn(baseMap, id)) ||
    incomingIds.some((id) => !Object.hasOwn(incomingCounts, id)) ||
    Object.keys(incomingCounts).some((id) => !Object.hasOwn(incomingMap, id))
  ) {
    fail(`${description} map/count IDs do not agree`);
  }

  const baseBuckets = new Map();
  for (const id of baseIds) {
    const key = stableJson(baseMap[id]);
    const bucket = baseBuckets.get(key) ?? [];
    bucket.push(id);
    baseBuckets.set(key, bucket);
  }
  const incomingBuckets = new Map();
  let incomingItems = 0;
  for (const id of incomingIds) {
    const count = incomingCounts[id];
    const weight = coverageCountWeight(count, `${description} ${id}`);
    incomingItems += weight;
    const key = stableJson(incomingMap[id]);
    const bucket = incomingBuckets.get(key) ?? [];
    bucket.push(id);
    incomingBuckets.set(key, bucket);
  }
  let matchedItems = 0;
  for (const [key, incomingBucket] of incomingBuckets) {
    const baseBucket = baseBuckets.get(key);
    // Identical locations can legitimately occur more than once. Pair them only when the two
    // instrumentation lanes agree on multiplicity; ambiguous hits remain conservatively ignored.
    if (!baseBucket || baseBucket.length !== incomingBucket.length) continue;
    for (let index = 0; index < incomingBucket.length; index += 1) {
      const incomingId = incomingBucket[index];
      const baseId = baseBucket[index];
      const count = incomingCounts[incomingId];
      matchedItems += coverageCountWeight(count, `${description} ${incomingId}`);
      baseCounts[baseId] = unionCount(baseCounts[baseId], count, `${description} ${incomingId}`);
    }
  }
  return { incomingItems, matchedItems };
}

function branchLocations(branchMap, branchCounts, description) {
  const records = [];
  const mapIds = Object.keys(branchMap);
  if (
    mapIds.some((id) => !Object.hasOwn(branchCounts, id)) ||
    Object.keys(branchCounts).some((id) => !Object.hasOwn(branchMap, id))
  ) {
    fail(`${description} map/count IDs do not agree`);
  }
  for (const id of mapIds) {
    const locations = branchMap[id]?.locations;
    const counts = branchCounts[id];
    if (!Array.isArray(locations) || !Array.isArray(counts) || locations.length !== counts.length) {
      fail(`${description} ${id} has malformed branch locations/counts`);
    }
    coverageCountWeight(counts, `${description} ${id}`);
    for (let index = 0; index < locations.length; index += 1) {
      records.push({ id, index, key: stableJson(locations[index]), count: counts[index] });
    }
  }
  return records;
}

function projectBranches(baseMap, baseCounts, incomingMap, incomingCounts, description) {
  const baseBuckets = new Map();
  for (const record of branchLocations(baseMap, baseCounts, description)) {
    const bucket = baseBuckets.get(record.key) ?? [];
    bucket.push(record);
    baseBuckets.set(record.key, bucket);
  }
  const incoming = branchLocations(incomingMap, incomingCounts, description);
  const incomingBuckets = new Map();
  for (const record of incoming) {
    const bucket = incomingBuckets.get(record.key) ?? [];
    bucket.push(record);
    incomingBuckets.set(record.key, bucket);
  }
  let matchedItems = 0;
  for (const [key, incomingBucket] of incomingBuckets) {
    const baseBucket = baseBuckets.get(key);
    if (!baseBucket || baseBucket.length !== incomingBucket.length) continue;
    for (let index = 0; index < incomingBucket.length; index += 1) {
      const record = incomingBucket[index];
      const baseRecord = baseBucket[index];
      matchedItems += 1;
      baseCounts[baseRecord.id][baseRecord.index] = Math.max(
        baseCounts[baseRecord.id][baseRecord.index],
        record.count,
      );
    }
  }
  return { incomingItems: incoming.length, matchedItems };
}

export function projectBrowserCoverage(unitMap, browserMap, coverageContract = contract) {
  validateContract(coverageContract);
  const mergedData = unitMap.toJSON();
  const totals = {
    statements: { incomingItems: 0, matchedItems: 0 },
    functions: { incomingItems: 0, matchedItems: 0 },
    branches: { incomingItems: 0, matchedItems: 0 },
  };
  for (const file of browserMap.files()) {
    if (!Object.hasOwn(mergedData, file)) {
      fail(`browser coverage contains a shipped file outside the unit denominator: ${file}`);
    }
    const base = mergedData[file];
    const incoming = browserMap.fileCoverageFor(file).toJSON();
    for (const [metric, mapName, countName] of [
      ['statements', 'statementMap', 's'],
      ['functions', 'fnMap', 'f'],
    ]) {
      const result = projectMetric(
        base[mapName],
        base[countName],
        incoming[mapName],
        incoming[countName],
        `${relative(appRoot, file)} ${metric}`,
      );
      totals[metric].incomingItems += result.incomingItems;
      totals[metric].matchedItems += result.matchedItems;
    }
    const branches = projectBranches(
      base.branchMap,
      base.b,
      incoming.branchMap,
      incoming.b,
      `${relative(appRoot, file)} branches`,
    );
    totals.branches.incomingItems += branches.incomingItems;
    totals.branches.matchedItems += branches.matchedItems;
  }
  const semanticMatch = {};
  for (const [metric, value] of Object.entries(totals)) {
    const pct = value.incomingItems === 0 ? 100 : (value.matchedItems / value.incomingItems) * 100;
    semanticMatch[metric] = {
      ...value,
      unmatchedItems: value.incomingItems - value.matchedItems,
      pct: Number(pct.toFixed(4)),
    };
  }
  return { coverageMap: createCoverageMap(mergedData), semanticMatch };
}

async function shippedSourceFiles() {
  return (await walk(sourceRoot))
    .map(normalize)
    .filter((file) => canonicalSourcePath(file) === file)
    .sort();
}

async function fileSnapshot(files) {
  const entries = [];
  for (const file of [...new Set(files.map(normalize))].sort()) {
    entries.push({
      path: relative(appRoot, file).replaceAll('\\', '/'),
      sha256: sha256(await readFile(file)),
    });
  }
  return {
    entries,
    sha256: sha256(entries.map((entry) => `${entry.path}\0${entry.sha256}`).join('\n')),
  };
}

async function sourceTreeSnapshot() {
  return fileSnapshot(await shippedSourceFiles());
}

async function campaignInputSnapshot() {
  const candidates = [
    ...(await walk(sourceRoot)),
    ...(await walk(join(appRoot, 'e2e'))),
    ...(await walk(join(appRoot, 'tests'))),
    resolve(appRoot, 'package.json'),
    resolve(appRoot, 'package-lock.json'),
    resolve(appRoot, 'playwright.config.ts'),
    resolve(appRoot, 'svelte.config.js'),
    resolve(appRoot, 'tsconfig.json'),
    resolve(appRoot, 'vite.config.ts'),
    resolve(appRoot, 'vitest.config.ts'),
    resolve(appRoot, 'src-tauri', 'assets', 'couch.html'),
    resolve(appRoot, 'scripts', 'frontend_coverage_contract.v1.json'),
    fileURLToPath(import.meta.url),
  ];
  return fileSnapshot(candidates);
}

function snapshotDifference(expected, actual) {
  const before = new Map(expected.entries.map((entry) => [entry.path, entry.sha256]));
  const after = new Map(actual.entries.map((entry) => [entry.path, entry.sha256]));
  const added = [...after.keys()].filter((path) => !before.has(path));
  const removed = [...before.keys()].filter((path) => !after.has(path));
  const changed = [...before.keys()].filter(
    (path) => after.has(path) && before.get(path) !== after.get(path),
  );
  return { added, removed, changed };
}

async function assertSnapshotUnchanged(expected, readCurrent, phase) {
  const actual = await readCurrent();
  if (actual.sha256 !== expected.sha256) {
    const difference = snapshotDifference(expected, actual);
    fail(
      `campaign inputs changed ${phase}: ${JSON.stringify({
        added: difference.added.slice(0, 10),
        removed: difference.removed.slice(0, 10),
        changed: difference.changed.slice(0, 10),
      })}`,
    );
  }
}

function reportPath(value) {
  return typeof value === 'string' ? value.replaceAll('\\', '/').replace(/^\.\//u, '') : '';
}

function collectReportTests(suites, inheritedFile = '', output = []) {
  if (!Array.isArray(suites)) return output;
  for (const suite of suites) {
    const suiteFile = reportPath(suite?.file) || inheritedFile;
    for (const spec of suite?.specs ?? []) {
      const file = reportPath(spec?.file) || suiteFile;
      for (const test of spec?.tests ?? []) {
        output.push({ id: spec?.id, file, title: spec?.title, test });
      }
    }
    collectReportTests(suite?.suites, suiteFile, output);
  }
  return output;
}

export function validatePlaywrightReport(report, coverageContract = contract) {
  validateContract(coverageContract);
  if (!report || typeof report !== 'object') fail('Playwright report is not an object');
  if (!Array.isArray(report.errors) || report.errors.length !== 0) {
    fail(`Playwright report contains ${report.errors?.length ?? 'unknown'} runner errors`);
  }
  const projects = report.config?.projects;
  if (!Array.isArray(projects) || projects.length !== 1) {
    fail('Playwright report must contain exactly one selected project');
  }
  const project = projects[0];
  if (
    project?.name !== coverageContract.playwrightProject ||
    project?.id !== coverageContract.playwrightProject
  ) {
    fail(`Playwright report is not for ${coverageContract.playwrightProject}`);
  }
  if (project.retries !== 0) fail(`Playwright report allowed ${String(project.retries)} retries`);

  const tests = collectReportTests(report.suites);
  if (tests.length < coverageContract.minimumFullE2ETests) {
    fail(
      `Playwright selected only ${tests.length} tests; ` +
        `${coverageContract.minimumFullE2ETests} are required`,
    );
  }
  const ids = new Set();
  for (const record of tests) {
    if (typeof record.id !== 'string' || !record.id || ids.has(record.id)) {
      fail(`Playwright report has a missing or duplicate test ID: ${String(record.id)}`);
    }
    ids.add(record.id);
    if (!record.file) fail(`Playwright test ${record.id} has no source file`);
    const results = record.test?.results;
    if (
      record.test?.projectName !== coverageContract.playwrightProject ||
      record.test?.expectedStatus !== 'passed' ||
      record.test?.status !== 'expected' ||
      !Array.isArray(results) ||
      results.length !== 1 ||
      results[0]?.retry !== 0 ||
      results[0]?.status !== 'passed'
    ) {
      fail(`Playwright test ${record.id} lacks one clean zero-retry pass`);
    }
  }
  const stats = report.stats;
  if (
    stats?.expected !== tests.length ||
    stats?.skipped !== 0 ||
    stats?.unexpected !== 0 ||
    stats?.flaky !== 0
  ) {
    fail(`Playwright result is not a clean full pass: ${JSON.stringify(stats)}`);
  }

  const standaloneFiles = new Set(coverageContract.standaloneE2EFiles.map(reportPath));
  const reportFiles = new Set(tests.map((record) => record.file));
  const absentStandalone = [...standaloneFiles].filter((file) => !reportFiles.has(file));
  if (absentStandalone.length) {
    fail(`declared standalone E2E files were not run: ${absentStandalone.join(', ')}`);
  }
  const byId = new Map(tests.map((record) => [record.id, record]));
  const instrumented = tests.filter((record) => !standaloneFiles.has(record.file));
  if (instrumented.length < coverageContract.minimumInstrumentedE2ETests) {
    fail(
      `only ${instrumented.length} Vite-app E2E tests were selected; ` +
        `${coverageContract.minimumInstrumentedE2ETests} are required`,
    );
  }
  return { tests, byId, instrumented, standaloneFiles };
}

function validateRawEntry(entry) {
  if (!entry || typeof entry !== 'object') fail('raw E2E entry is not an object');
  if (
    typeof entry.url !== 'string' ||
    !entry.url ||
    typeof entry.source !== 'string' ||
    !entry.source
  ) {
    fail('raw E2E entry is missing URL/source');
  }
  if (!Array.isArray(entry.functions) || typeof entry.scriptId !== 'string') {
    fail(`raw E2E entry for ${entry.url} has malformed V8 coverage`);
  }
}

async function loadE2ECoverage({
  runToken,
  sourceTreeSha256,
  reportAuthority,
  rawDirectory = rawRoot,
}) {
  const rawFiles = (await readdir(rawDirectory)).filter((name) => name.endsWith('.json.gz')).sort();
  if (rawFiles.length < contract.minimumInstrumentedE2ETests) {
    fail(
      `only ${rawFiles.length} instrumented E2E records exist; ` +
        `${contract.minimumInstrumentedE2ETests} are required`,
    );
  }

  const byUrl = new Map();
  const rawTestIds = new Set();
  const rawHashes = [];
  for (const name of rawFiles) {
    const bytes = await readFile(join(rawDirectory, name));
    rawHashes.push(`${name}\0${sha256(bytes)}`);
    let document;
    try {
      document = JSON.parse(gunzipSync(bytes).toString('utf8'));
    } catch (error) {
      fail(`${name} is not valid compressed JSON: ${error.message}`);
    }
    if (
      document?.schema !== 2 ||
      document.runToken !== runToken ||
      document.sourceTreeSha256 !== sourceTreeSha256 ||
      document.projectName !== contract.playwrightProject ||
      document.retry !== 0 ||
      typeof document.testId !== 'string' ||
      !Array.isArray(document.entries) ||
      document.entries.length === 0
    ) {
      fail(`${name} is not a valid schema-2 campaign-bound coverage record`);
    }
    const expectedName = `${sha256(
      `${runToken}:${document.testId}:${document.projectName}:${document.retry}`,
    )}.json.gz`;
    if (name !== expectedName) fail(`${name} does not match its coverage-record identity`);
    if (rawTestIds.has(document.testId)) fail(`duplicate coverage for test ${document.testId}`);
    const reportTest = reportAuthority.byId.get(document.testId);
    if (!reportTest) fail(`${name} belongs to a test outside the Playwright report`);
    if (reportAuthority.standaloneFiles.has(reportTest.file)) {
      fail(`${name} unexpectedly attributes Vite coverage to standalone test ${reportTest.file}`);
    }
    rawTestIds.add(document.testId);
    for (const entry of document.entries) {
      validateRawEntry(entry);
      let entryOrigin;
      try {
        entryOrigin = new URL(entry.url).origin;
      } catch {
        fail(`raw E2E entry has an invalid URL: ${entry.url}`);
      }
      if (entryOrigin !== contract.viteOrigin) {
        fail(`raw E2E entry came from an untrusted origin: ${entry.url}`);
      }
      const sourceDigest = sha256(entry.source);
      const existing = byUrl.get(entry.url);
      if (existing && existing.sourceDigest !== sourceDigest) {
        fail(`the dev server returned two source bodies for ${entry.url}`);
      }
      const group = existing ?? { source: entry.source, sourceDigest, coverages: [] };
      group.coverages.push({
        scriptId: entry.scriptId,
        url: entry.url,
        functions: entry.functions,
      });
      byUrl.set(entry.url, group);
    }
  }

  const expectedTestIds = new Set(reportAuthority.instrumented.map((record) => record.id));
  const missing = [...expectedTestIds].filter((id) => !rawTestIds.has(id));
  const unexpected = [...rawTestIds].filter((id) => !expectedTestIds.has(id));
  if (missing.length || unexpected.length || rawTestIds.size !== expectedTestIds.size) {
    fail(
      `raw E2E identity mismatch: ${JSON.stringify({
        expected: expectedTestIds.size,
        actual: rawTestIds.size,
        missing: missing.slice(0, 10),
        unexpected: unexpected.slice(0, 10),
      })}`,
    );
  }

  const e2eMap = createCoverageMap({});
  const convertedFiles = new Set();
  for (const [url, group] of [...byUrl.entries()].sort(([left], [right]) =>
    left.localeCompare(right),
  )) {
    const merged = mergeScriptCovs(group.coverages);
    if (!merged) fail(`could not merge V8 ranges for ${url}`);
    const executedPath = canonicalSourcePath(url);
    if (!executedPath) fail(`could not bind executed URL to a shipped source: ${url}`);
    const converted = await convert({
      ast: parseAstAsync(group.source),
      code: group.source,
      wrapperLength: 0,
      // The converter accepts file URLs only. Bind Chromium's validated Vite transport URL
      // to the corresponding source path before it resolves the inline source map.
      coverage: { ...merged, url: pathToFileURL(executedPath).href },
    });
    const normalizedData = normalizedCoverageData(converted);
    for (const file of Object.keys(normalizedData)) convertedFiles.add(file);
    e2eMap.merge(normalizedData);
  }
  if (convertedFiles.size < contract.minimumE2EConvertedSourceFiles) {
    fail(
      `E2E coverage converted only ${convertedFiles.size} shipped sources; ` +
        `${contract.minimumE2EConvertedSourceFiles} are required`,
    );
  }
  return {
    e2eMap,
    rawFiles,
    convertedFiles,
    rawCoverageSha256: sha256(rawHashes.join('\n')),
  };
}

async function readRawAuthorityBundle(manifest, bundlePath = rawAuthorityBundlePath) {
  const bundle = await readFile(bundlePath);
  if (
    manifest?.bundle?.format !== 'CORTEX_FRONTEND_COVERAGE_RAW_V1' ||
    manifest.bundle.sha256 !== sha256(bundle) ||
    manifest.bundle.bytes !== bundle.length ||
    !Array.isArray(manifest.bundle.entries) ||
    manifest.bundle.entries.length === 0
  ) {
    fail('raw authority bundle identity is malformed or substituted');
  }
  const magic = Buffer.from('CORTEX_FRONTEND_COVERAGE_RAW_V1\n', 'utf8');
  if (!bundle.subarray(0, magic.length).equals(magic)) {
    fail('raw authority bundle has the wrong format marker');
  }
  let offset = magic.length;
  const entries = [];
  const contents = new Map();
  while (offset < bundle.length) {
    if (offset + 4 > bundle.length) fail('raw authority bundle truncates an entry header');
    const headerLength = bundle.readUInt32BE(offset);
    offset += 4;
    if (headerLength <= 0 || offset + headerLength + 8 > bundle.length) {
      fail('raw authority bundle has an impossible entry header length');
    }
    let header;
    const headerBytes = bundle.subarray(offset, offset + headerLength);
    try {
      header = JSON.parse(headerBytes.toString('utf8'));
    } catch (error) {
      fail(`raw authority bundle entry header is not JSON: ${error.message}`);
    }
    offset += headerLength;
    const contentLength = bundle.readBigUInt64BE(offset);
    offset += 8;
    if (contentLength > BigInt(Number.MAX_SAFE_INTEGER)) {
      fail('raw authority bundle entry is too large to validate safely');
    }
    const length = Number(contentLength);
    if (
      !header ||
      typeof header !== 'object' ||
      Array.isArray(header) ||
      !headerBytes.equals(Buffer.from(stableJson(header), 'utf8')) ||
      stableJson(header) !==
        stableJson({ bytes: header.bytes, path: header.path, sha256: header.sha256 }) ||
      !Number.isInteger(header.bytes) ||
      header.bytes < 0 ||
      header.bytes !== length ||
      typeof header.path !== 'string' ||
      !header.path ||
      header.path.includes('\\') ||
      isAbsolute(header.path) ||
      header.path.split('/').some((part) => !part || part === '.' || part === '..') ||
      typeof header.sha256 !== 'string' ||
      !/^[0-9a-f]{64}$/u.test(header.sha256) ||
      contents.has(header.path) ||
      offset + length > bundle.length
    ) {
      fail('raw authority bundle entry is malformed, duplicated, or unsafe');
    }
    const bytes = bundle.subarray(offset, offset + length);
    offset += length;
    if (sha256(bytes) !== header.sha256) fail(`raw authority entry ${header.path} is corrupted`);
    entries.push(header);
    contents.set(header.path, bytes);
  }
  if (stableJson(entries) !== stableJson(manifest.bundle.entries)) {
    fail('raw authority manifest does not enumerate the exact bundle entries');
  }
  return contents;
}

export function criticalCoverageSummaries(coverageMap, coverageContract = contract) {
  validateContract(coverageContract);
  const summaries = {};
  for (const [domain, relativeSources] of Object.entries(coverageContract.criticalDomains)) {
    const domainMap = createCoverageMap({});
    for (const relativeSource of relativeSources) {
      const source = resolve(appRoot, relativeSource);
      if (!coverageMap.files().map(normalize).includes(normalize(source))) {
        fail(
          `critical ${domain} source is absent from the coverage denominator: ${relativeSource}`,
        );
      }
      domainMap.addFileCoverage(coverageMap.fileCoverageFor(source));
    }
    const summary = domainMap.getCoverageSummary().toJSON();
    for (const metric of ['statements', 'branches', 'functions', 'lines']) {
      if (!Number.isInteger(summary[metric]?.total) || summary[metric].total <= 0) {
        fail(`critical ${domain} ${metric} has a zero denominator`);
      }
    }
    summaries[domain] = summary;
  }
  return summaries;
}

function enforceThresholds(summary, criticalSummaries) {
  const failures = [];
  for (const [metric, minimum] of Object.entries(contract.thresholds)) {
    const actual = summary[metric]?.pct;
    if (typeof actual !== 'number' || !Number.isFinite(actual) || actual < minimum) {
      failures.push(`${metric} ${String(actual)}% < ${minimum}%`);
    }
  }
  for (const [domain, domainSummary] of Object.entries(criticalSummaries)) {
    for (const [metric, minimum] of Object.entries(contract.criticalThresholds)) {
      const actual = domainSummary[metric]?.pct;
      if (typeof actual !== 'number' || !Number.isFinite(actual) || actual < minimum) {
        failures.push(`critical ${domain} ${metric} ${String(actual)}% < ${minimum}%`);
      }
    }
  }
  if (failures.length) fail(`locked thresholds failed: ${failures.join(', ')}`);
}

async function readJson(path, description) {
  try {
    return JSON.parse((await readFile(path)).toString('utf8'));
  } catch (error) {
    fail(`${description} is not valid JSON: ${error.message}`);
  }
}

function parseBundledJson(contents, path, description) {
  const bytes = contents.get(path);
  if (!bytes) fail(`raw authority bundle omits ${path}`);
  try {
    return JSON.parse(bytes.toString('utf8'));
  } catch (error) {
    fail(`${description} in the raw authority bundle is not JSON: ${error.message}`);
  }
}

export async function replayPublishedCoverage({
  manifestPath = rawAuthorityManifestPath,
  bundlePath = rawAuthorityBundlePath,
  temporaryParent = coverageRoot,
} = {}) {
  validateContract();
  const manifestBytes = await readFile(manifestPath);
  const manifest = await readJson(manifestPath, 'raw coverage authority manifest');
  const expectedManifestKeys = [
    'schema',
    'type',
    'runToken',
    'sourceTree',
    'campaignInputs',
    'authorities',
    'runtime',
    'commands',
    'bundle',
  ].sort();
  if (
    !manifest ||
    typeof manifest !== 'object' ||
    Array.isArray(manifest) ||
    Object.keys(manifest).sort().join('\0') !== expectedManifestKeys.join('\0') ||
    manifest.schema !== 1 ||
    manifest.type !== 'FrontendCoverageRawAuthorityV1' ||
    typeof manifest.runToken !== 'string' ||
    !/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/u.test(
      manifest.runToken,
    )
  ) {
    fail('raw coverage authority manifest has a non-canonical envelope');
  }
  const authorityPaths = {
    contract: resolve(appRoot, 'scripts', 'frontend_coverage_contract.v1.json'),
    runner: fileURLToPath(import.meta.url),
    packageLock: resolve(appRoot, 'package-lock.json'),
    vitestConfig: resolve(appRoot, 'vitest.config.ts'),
    playwrightConfig: resolve(appRoot, 'playwright.config.ts'),
  };
  const expectedRelative = {
    contract: 'scripts/frontend_coverage_contract.v1.json',
    runner: 'scripts/run_merged_frontend_coverage.mjs',
    packageLock: 'package-lock.json',
    vitestConfig: 'vitest.config.ts',
    playwrightConfig: 'playwright.config.ts',
  };
  if (
    !manifest.authorities ||
    typeof manifest.authorities !== 'object' ||
    Array.isArray(manifest.authorities) ||
    Object.keys(manifest.authorities).sort().join('\0') !==
      Object.keys(authorityPaths).sort().join('\0')
  ) {
    fail('raw coverage authority omits a committed producer authority');
  }
  for (const [name, path] of Object.entries(authorityPaths)) {
    const row = manifest.authorities[name];
    if (
      !row ||
      Object.keys(row).sort().join('\0') !== 'path\0sha256' ||
      row.path !== expectedRelative[name] ||
      row.sha256 !== sha256(await readFile(path))
    ) {
      fail(`raw coverage authority substituted ${name}`);
    }
  }
  const currentSources = await sourceTreeSnapshot();
  const currentInputs = await campaignInputSnapshot();
  if (
    stableJson(manifest.sourceTree) !== stableJson(currentSources) ||
    stableJson(manifest.campaignInputs) !== stableJson(currentInputs)
  ) {
    fail('raw coverage authority is stale for the exact source or campaign-input tree');
  }
  if (
    stableJson(manifest.runtime) !==
    stableJson({ node: process.version, platform: process.platform, architecture: process.arch })
  ) {
    fail('raw coverage authority runtime identity is substituted');
  }
  const contents = await readRawAuthorityBundle(manifest, bundlePath);
  const playwrightReport = parseBundledJson(
    contents,
    'e2e/playwright-report.json',
    'Playwright report',
  );
  const reportAuthority = validatePlaywrightReport(playwrightReport);
  const unitDocument = parseBundledJson(contents, 'unit/coverage-final.json', 'unit coverage');
  const unitMap = createCoverageMap(normalizedCoverageData(unitDocument));
  const temporaryRoot = await mkdtemp(join(temporaryParent, 'cortex-frontend-coverage-replay-'));
  try {
    const recordEntries = manifest.bundle.entries.filter((entry) =>
      entry.path.startsWith('e2e/records/'),
    );
    for (const entry of recordEntries) {
      const name = entry.path.slice('e2e/records/'.length);
      if (!name || name.includes('/')) fail('raw authority has a non-canonical E2E record name');
      await writeFile(join(temporaryRoot, name), contents.get(entry.path), { flag: 'wx' });
    }
    const { e2eMap, rawFiles, convertedFiles } = await loadE2ECoverage({
      runToken: manifest.runToken,
      sourceTreeSha256: currentSources.sha256,
      reportAuthority,
      rawDirectory: temporaryRoot,
    });
    if (rawFiles.length !== recordEntries.length) {
      fail('raw authority replay did not consume every E2E record');
    }
    const browserDocument = parseBundledJson(
      contents,
      'e2e/browser-coverage-final.json',
      'browser coverage',
    );
    const replayedBrowser = JSON.parse(JSON.stringify(e2eMap.toJSON()));
    if (stableJson(replayedBrowser) !== stableJson(browserDocument)) {
      const allFiles = [
        ...new Set([...Object.keys(replayedBrowser), ...Object.keys(browserDocument)]),
      ].sort();
      const firstMismatch = allFiles.find(
        (file) => stableJson(replayedBrowser[file]) !== stableJson(browserDocument[file]),
      );
      const firstField = firstMismatch
        ? [
            ...new Set([
              ...Object.keys(replayedBrowser[firstMismatch] ?? {}),
              ...Object.keys(browserDocument[firstMismatch] ?? {}),
            ]),
          ].find(
            (field) =>
              stableJson(replayedBrowser[firstMismatch]?.[field]) !==
              stableJson(browserDocument[firstMismatch]?.[field]),
          )
        : undefined;
      fail(
        `browser coverage map is not reproducible from the raw per-test V8 records` +
          (firstMismatch
            ? `; first mismatch ${firstMismatch}${firstField ? ` field ${firstField}` : ''}`
            : ''),
      );
    }
    const { coverageMap: mergedMap } = projectBrowserCoverage(
      createCoverageMap(JSON.parse(JSON.stringify(unitMap.toJSON()))),
      createCoverageMap(browserDocument),
    );
    const mergedDocument = parseBundledJson(
      contents,
      'merged/coverage-final.json',
      'merged coverage',
    );
    if (stableJson(JSON.parse(JSON.stringify(mergedMap.toJSON()))) !== stableJson(mergedDocument)) {
      fail('merged coverage map is not reproducible from its unit and raw browser authorities');
    }
    const summaryDocument = parseBundledJson(
      contents,
      'merged/coverage-summary.json',
      'merged coverage summary',
    );
    const summary = mergedMap.getCoverageSummary().toJSON();
    if (
      stableJson(summaryDocument.total) !==
      stableJson({
        ...summary,
        branchesTrue: { total: 0, covered: 0, skipped: 0, pct: 100 },
      })
    ) {
      fail('merged coverage summary is not derivable from the replayed map');
    }
    const criticalDomains = criticalCoverageSummaries(mergedMap);
    enforceThresholds(summary, criticalDomains);
    if (convertedFiles.size < contract.minimumE2EConvertedSourceFiles) {
      fail('raw authority replay converted too few shipped E2E sources');
    }
    return {
      schema: 1,
      type: 'FrontendCoverageReplayV1',
      certificationEligible: true,
      runToken: manifest.runToken,
      sourceTreeSha256: currentSources.sha256,
      campaignInputsSha256: currentInputs.sha256,
      manifestSha256: sha256(manifestBytes),
      bundleSha256: sha256(await readFile(bundlePath)),
      fullE2ETests: reportAuthority.tests.length,
      instrumentedE2ETests: reportAuthority.instrumented.length,
      e2eRawFiles: rawFiles.length,
      e2eConvertedSourceFiles: convertedFiles.size,
      summary,
      criticalDomains,
    };
  } finally {
    await rm(temporaryRoot, { recursive: true, force: true });
  }
}

function replayCliOptions(argv) {
  if (argv.length === 0) return {};
  const expectedFlags = ['--manifest', '--bundle', '--temporary-parent'];
  if (argv.length !== expectedFlags.length * 2) {
    fail('replay requires exactly --manifest, --bundle, and --temporary-parent');
  }
  const values = {};
  for (let index = 0; index < expectedFlags.length; index += 1) {
    const flag = argv[index * 2];
    const value = argv[index * 2 + 1];
    if (
      flag !== expectedFlags[index] ||
      typeof value !== 'string' ||
      !value ||
      value.includes('\0') ||
      !isAbsolute(value)
    ) {
      fail('replay path arguments are missing, reordered, relative, or malformed');
    }
    values[flag] = resolve(value);
  }
  return {
    manifestPath: values['--manifest'],
    bundlePath: values['--bundle'],
    temporaryParent: values['--temporary-parent'],
  };
}

async function main() {
  validateContract();
  const initialSources = await sourceTreeSnapshot();
  const initialInputs = await campaignInputSnapshot();
  const contractPath = resolve(appRoot, 'scripts', 'frontend_coverage_contract.v1.json');
  const contractBytes = await readFile(contractPath);
  const runToken = randomUUID();
  await resetArtifacts();

  const unitCommand = runNode(
    resolve(appRoot, 'node_modules', 'vitest', 'vitest.mjs'),
    ['run', '--coverage'],
    {
      CORTEX_MERGED_COVERAGE: '1',
    },
    join(rawAuthorityRoot, 'unit.log'),
  );
  await assertSnapshotUnchanged(initialSources, sourceTreeSnapshot, 'during unit coverage');
  await assertSnapshotUnchanged(initialInputs, campaignInputSnapshot, 'during unit coverage');

  const playwrightCommand = runNode(
    resolve(appRoot, 'node_modules', '@playwright', 'test', 'cli.js'),
    [
      'test',
      `--project=${contract.playwrightProject}`,
      '--workers=1',
      '--retries=0',
      '--reporter=line,json',
    ],
    {
      CORTEX_E2E_COVERAGE: '1',
      CORTEX_E2E_COVERAGE_RUN_TOKEN: runToken,
      CORTEX_E2E_SOURCE_TREE_SHA256: initialSources.sha256,
      CORTEX_E2E_COVERAGE_ORIGIN: contract.viteOrigin,
      CORTEX_GATE: '1',
      PLAYWRIGHT_JSON_OUTPUT_FILE: playwrightReportPath,
    },
    join(rawAuthorityRoot, 'playwright.log'),
  );
  await assertSnapshotUnchanged(initialSources, sourceTreeSnapshot, 'during E2E coverage');
  await assertSnapshotUnchanged(initialInputs, campaignInputSnapshot, 'during E2E coverage');

  const playwrightBytes = await readFile(playwrightReportPath);
  const playwrightReport = await readJson(playwrightReportPath, 'Playwright report');
  const reportAuthority = validatePlaywrightReport(playwrightReport);
  const unitCoveragePath = join(unitRoot, 'coverage-final.json');
  const unitBytes = await readFile(unitCoveragePath);
  const unitDocument = await readJson(unitCoveragePath, 'unit coverage');
  const unitMap = createCoverageMap(normalizedCoverageData(unitDocument));
  const { e2eMap, rawFiles, convertedFiles, rawCoverageSha256 } = await loadE2ECoverage({
    runToken,
    sourceTreeSha256: initialSources.sha256,
    reportAuthority,
  });
  const browserCoverageBytes = Buffer.from(`${JSON.stringify(e2eMap.toJSON())}\n`, 'utf8');
  await atomicWriteFile(join(rawRoot, 'coverage-final.json'), browserCoverageBytes);
  // Incremental Istanbul merges retain insertion-order state that is not a stable evidence
  // boundary (most visibly for duplicate branch locations). Project only the canonical JSON
  // representations that the proof bundle can independently replay.
  const canonicalUnitMap = createCoverageMap(JSON.parse(JSON.stringify(unitMap.toJSON())));
  const canonicalBrowserMap = createCoverageMap(JSON.parse(browserCoverageBytes.toString('utf8')));
  // The unit map is the complete, static source denominator. Browser conversion can add
  // development-runtime wrapper nodes; project only semantically identical source entities onto
  // the unit map so one source statement is never counted twice under incompatible map hashes.
  const { coverageMap: mergedMap, semanticMatch } = projectBrowserCoverage(
    canonicalUnitMap,
    canonicalBrowserMap,
  );

  const sources = initialSources.entries.map((entry) => normalize(resolve(appRoot, entry.path)));
  const coveredFiles = new Set(mergedMap.files().map(normalize));
  const missing = sources.filter((file) => !coveredFiles.has(file));
  if (missing.length) {
    fail(
      `coverage map omitted shipped sources: ${missing.map((file) => relative(appRoot, file)).join(', ')}`,
    );
  }

  await mkdir(mergedRoot, { recursive: true });
  const reportContext = createContext({ dir: mergedRoot, coverageMap: mergedMap });
  for (const reporter of ['text', 'json-summary', 'json', 'html']) {
    createReport(reporter).execute(reportContext);
  }
  const summary = mergedMap.getCoverageSummary().toJSON();
  const criticalDomains = criticalCoverageSummaries(mergedMap);
  enforceThresholds(summary, criticalDomains);
  await assertSnapshotUnchanged(initialSources, sourceTreeSnapshot, 'before coverage publication');
  await assertSnapshotUnchanged(
    initialInputs,
    campaignInputSnapshot,
    'before coverage publication',
  );
  if (sha256(await readFile(contractPath)) !== sha256(contractBytes)) {
    fail('coverage contract changed during the campaign');
  }

  const mergedCoveragePath = join(mergedRoot, 'coverage-final.json');
  const mergedBytes = await readFile(mergedCoveragePath);
  const rawBundle = await writeRawAuthorityBundle([
    { path: 'unit/coverage-final.json', file: unitCoveragePath },
    { path: 'e2e/playwright-report.json', file: playwrightReportPath },
    { path: 'e2e/browser-coverage-final.json', file: join(rawRoot, 'coverage-final.json') },
    ...rawFiles.map((name) => ({
      path: `e2e/records/${name}`,
      file: join(rawRoot, name),
    })),
    { path: 'logs/unit.log', file: join(rawAuthorityRoot, 'unit.log') },
    { path: 'logs/playwright.log', file: join(rawAuthorityRoot, 'playwright.log') },
    { path: 'merged/coverage-final.json', file: mergedCoveragePath },
    { path: 'merged/coverage-summary.json', file: join(mergedRoot, 'coverage-summary.json') },
  ]);
  const runnerPath = fileURLToPath(import.meta.url);
  const packageLockPath = resolve(appRoot, 'package-lock.json');
  const vitestConfigPath = resolve(appRoot, 'vitest.config.ts');
  const playwrightConfigPath = resolve(appRoot, 'playwright.config.ts');
  const rawAuthority = {
    schema: 1,
    type: 'FrontendCoverageRawAuthorityV1',
    runToken,
    sourceTree: initialSources,
    campaignInputs: initialInputs,
    authorities: {
      contract: {
        path: 'scripts/frontend_coverage_contract.v1.json',
        sha256: sha256(contractBytes),
      },
      runner: {
        path: 'scripts/run_merged_frontend_coverage.mjs',
        sha256: sha256(await readFile(runnerPath)),
      },
      packageLock: {
        path: 'package-lock.json',
        sha256: sha256(await readFile(packageLockPath)),
      },
      vitestConfig: {
        path: 'vitest.config.ts',
        sha256: sha256(await readFile(vitestConfigPath)),
      },
      playwrightConfig: {
        path: 'playwright.config.ts',
        sha256: sha256(await readFile(playwrightConfigPath)),
      },
    },
    runtime: {
      node: process.version,
      platform: process.platform,
      architecture: process.arch,
    },
    commands: [unitCommand, playwrightCommand],
    bundle: {
      format: 'CORTEX_FRONTEND_COVERAGE_RAW_V1',
      sha256: rawBundle.sha256,
      bytes: rawBundle.bytes,
      entries: rawBundle.entries,
    },
  };
  const rawAuthorityBytes = Buffer.from(`${JSON.stringify(rawAuthority, null, 2)}\n`, 'utf8');
  await atomicWriteFile(rawAuthorityManifestPath, rawAuthorityBytes);
  const evidence = {
    schema: 1,
    runToken,
    contractSha256: sha256(contractBytes),
    sourceTreeSha256: initialSources.sha256,
    campaignInputsSha256: initialInputs.sha256,
    unitCoverageSha256: sha256(unitBytes),
    playwrightReportSha256: sha256(playwrightBytes),
    rawCoverageSha256,
    browserCoverageSha256: sha256(browserCoverageBytes),
    mergedCoverageSha256: sha256(mergedBytes),
    rawAuthorityManifestSha256: sha256(rawAuthorityBytes),
    rawAuthorityBundleSha256: rawBundle.sha256,
    shippedSourceFiles: sources.length,
    fullE2ETests: reportAuthority.tests.length,
    instrumentedE2ETests: reportAuthority.instrumented.length,
    e2eRawFiles: rawFiles.length,
    e2eConvertedSourceFiles: convertedFiles.size,
    semanticMapMatch: semanticMatch,
    summary,
    criticalDomains,
  };
  const evidenceBytes = Buffer.from(`${JSON.stringify(evidence, null, 2)}\n`, 'utf8');
  await atomicWriteFile(join(mergedRoot, 'frontend-coverage-evidence.json'), evidenceBytes);
  await atomicWriteFile(join(coverageRoot, 'coverage-final.json'), mergedBytes);
  await atomicWriteFile(
    join(coverageRoot, 'coverage-summary.json'),
    await readFile(join(mergedRoot, 'coverage-summary.json')),
  );
  // Publish the attestation last. Its absence makes an interrupted publication non-authoritative.
  await atomicWriteFile(join(coverageRoot, 'frontend-coverage-evidence.json'), evidenceBytes);
  console.log(
    `Merged frontend coverage PASS: ${summary.lines.pct}% lines, ${summary.statements.pct}% statements, ` +
      `${summary.branches.pct}% branches, ${summary.functions.pct}% functions`,
  );
}

const directRun = process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url);
if (directRun) {
  const replay = process.argv[2] === '--replay';
  const operation = replay
    ? Promise.resolve().then(() => replayPublishedCoverage(replayCliOptions(process.argv.slice(3))))
    : main();
  operation
    .then((result) => {
      if (replay) console.log(stableJson(result));
    })
    .catch((error) => {
      console.error(error instanceof Error ? error.message : String(error));
      process.exitCode = 1;
    });
}

export {
  canonicalSourcePath,
  readRawAuthorityBundle,
  replayCliOptions,
  snapshotDifference,
  validateContract,
};
