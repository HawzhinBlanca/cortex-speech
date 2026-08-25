import { readFileSync } from 'node:fs';
import { gzipSync } from 'node:zlib';
import { join, resolve } from 'node:path';

const distDir = resolve(process.argv[2] ?? 'dist');
const manifestPath = join(distDir, '.vite', 'manifest.json');
const manifest = JSON.parse(readFileSync(manifestPath, 'utf8'));

const entry = Object.entries(manifest).find(([, value]) => value.isEntry);
if (!entry) {
  throw new Error(`No entry was found in ${manifestPath}`);
}

const initialKeys = new Set();
const visit = (key) => {
  if (initialKeys.has(key)) return;
  const item = manifest[key];
  if (!item) throw new Error(`Manifest import ${key} is missing`);
  initialKeys.add(key);
  for (const dependency of item.imports ?? []) visit(dependency);
};
visit(entry[0]);

const javascriptFiles = new Set();
const cssFiles = new Set();
for (const key of initialKeys) {
  const item = manifest[key];
  if (item.file.endsWith('.js')) javascriptFiles.add(item.file);
  for (const cssFile of item.css ?? []) cssFiles.add(cssFile);
}

const gzipBytes = (relativePath) =>
  gzipSync(readFileSync(join(distDir, relativePath)), { level: 9 }).byteLength;
const sumGzip = (files) => [...files].reduce((total, file) => total + gzipBytes(file), 0);
const javascriptGzipBytes = sumGzip(javascriptFiles);
const cssGzipBytes = sumGzip(cssFiles);

const JAVASCRIPT_LIMIT_BYTES = 125_000;
const CSS_LIMIT_BYTES = 15_000;
const kb = (bytes) => (bytes / 1000).toFixed(2);

console.log(
  `Initial JavaScript: ${kb(javascriptGzipBytes)} KB gzip / ${kb(JAVASCRIPT_LIMIT_BYTES)} KB`,
);
for (const file of [...javascriptFiles].sort()) {
  console.log(`  ${file}: ${kb(gzipBytes(file))} KB gzip`);
}
console.log(`Initial CSS: ${kb(cssGzipBytes)} KB gzip / ${kb(CSS_LIMIT_BYTES)} KB`);
for (const file of [...cssFiles].sort()) {
  console.log(`  ${file}: ${kb(gzipBytes(file))} KB gzip`);
}

const failures = [];
if (javascriptGzipBytes > JAVASCRIPT_LIMIT_BYTES) {
  failures.push(
    `initial JavaScript exceeds its limit by ${kb(javascriptGzipBytes - JAVASCRIPT_LIMIT_BYTES)} KB`,
  );
}
if (cssGzipBytes > CSS_LIMIT_BYTES) {
  failures.push(`initial CSS exceeds its limit by ${kb(cssGzipBytes - CSS_LIMIT_BYTES)} KB`);
}
if (failures.length > 0) {
  throw new Error(`Bundle budget failed: ${failures.join('; ')}`);
}

console.log('Bundle budget passed.');
