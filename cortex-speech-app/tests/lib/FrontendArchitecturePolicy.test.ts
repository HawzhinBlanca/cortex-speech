import { readdirSync, readFileSync } from 'node:fs';
import { join, relative, resolve } from 'node:path';
import ts from 'typescript';
import { describe, expect, it } from 'vitest';

const root = resolve(import.meta.dirname, '../..');
const sourceRoot = join(root, 'src');

function filesUnder(directory: string): string[] {
  return readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    const path = join(directory, entry.name);
    return entry.isDirectory() ? filesUnder(path) : [path];
  });
}

function sourcePath(path: string): string {
  return relative(sourceRoot, path).replaceAll('\\', '/');
}

function lines(path: string): number {
  return readFileSync(path, 'utf8').split(/\r?\n/).length;
}

const productionSources = filesUnder(sourceRoot).filter(
  (path) =>
    (path.endsWith('.ts') || path.endsWith('.svelte')) &&
    !path.endsWith('.test.ts') &&
    !path.endsWith('.spec.ts'),
);
const components = productionSources.filter((path) => path.endsWith('.svelte'));

const controllerComponents = new Set([
  'Workstation.svelte',
  'lib/ModelDownload.svelte',
  'lib/ModelRegistry.svelte',
  'lib/ReviewInbox.svelte',
  'lib/ReviewMode.svelte',
  'lib/SettingsPanel.svelte',
  'lib/StatsDashboard.svelte',
  'lib/WslConsolePanel.svelte',
]);

// Existing debt is explicit and monotonic: these are ceilings, not exemptions from the target.
// A file may shrink below its target and disappear from this list without changing the gate.
const knownOversizeCeilings = new Map<string, number>();

describe('frontend architecture contract', () => {
  it('keeps the composition root within its locked 350-line ceiling', () => {
    expect(lines(join(sourceRoot, 'App.svelte'))).toBeLessThanOrEqual(350);
  });

  it('allows no new oversized components and no growth in the explicit debt', () => {
    const violations = components
      .map((path) => {
        const name = sourcePath(path);
        const limit = controllerComponents.has(name) ? 500 : 350;
        return { name, actual: lines(path), limit };
      })
      .filter(({ actual, limit }) => actual > limit);

    const regressions = violations.flatMap(({ name, actual, limit }) => {
      const ceiling = knownOversizeCeilings.get(name);
      if (ceiling === undefined)
        return [`${name}: ${actual} lines exceeds ${limit} (new violation)`];
      if (actual > ceiling) return [`${name}: ${actual} lines exceeds debt ceiling ${ceiling}`];
      return [];
    });
    expect(regressions, regressions.join('\n')).toEqual([]);
  });

  it('keeps Tauri package imports inside the handwritten adapter and generated bindings', () => {
    const allowed = new Set([
      'lib/adapters/desktop.ts',
      'lib/adapters/legacyIpc.ts',
      'lib/generated/ipc.ts',
    ]);
    const violations = productionSources.flatMap((path) => {
      const name = sourcePath(path);
      if (allowed.has(name)) return [];
      return readFileSync(path, 'utf8').includes('@tauri-apps') ? [name] : [];
    });
    expect(violations).toEqual([]);
  });

  it('keeps components on services instead of adapter or generated IPC imports', () => {
    const violations = components.flatMap((path) => {
      const text = readFileSync(path, 'utf8');
      return text.includes('/adapters/') || text.includes('generated/ipc')
        ? [sourcePath(path)]
        : [];
    });
    expect(violations).toEqual([]);
  });

  it('keeps raw Tauri invoke capability inside generated bindings and the closed adapter', () => {
    const violations: string[] = [];
    for (const path of productionSources.filter((candidate) => candidate.endsWith('.ts'))) {
      const name = sourcePath(path);
      const node = ts.createSourceFile(
        path,
        readFileSync(path, 'utf8'),
        ts.ScriptTarget.Latest,
        true,
        ts.ScriptKind.TS,
      );
      node.forEachChild((child) => {
        if (
          !ts.isImportDeclaration(child) ||
          !ts.isStringLiteral(child.moduleSpecifier) ||
          child.moduleSpecifier.text !== '@tauri-apps/api/core'
        ) {
          return;
        }
        const named = child.importClause?.namedBindings;
        if (!named || !ts.isNamedImports(named)) return;
        for (const specifier of named.elements) {
          const imported = specifier.propertyName?.text ?? specifier.name.text;
          if (imported !== 'invoke') continue;
          const allowedLegacy =
            name === 'lib/adapters/legacyIpc.ts' && specifier.name.text === 'invokeDesktop';
          const allowedGenerated =
            name === 'lib/generated/ipc.ts' && specifier.name.text === '__TAURI_INVOKE';
          if (!allowedLegacy && !allowedGenerated) {
            violations.push(`${name}: raw Tauri invoke import escaped its audited boundary`);
          }
        }
      });
    }
    expect(violations).toEqual([]);
  });

  it('keeps the heavy statistics workspace out of the initial JavaScript graph', () => {
    const workstation = readFileSync(join(sourceRoot, 'Workstation.svelte'), 'utf8');
    const consumers = ['lib/WorkstationCenter.svelte', 'lib/WorkstationStatsPanel.svelte']
      .map((path) => readFileSync(join(sourceRoot, path), 'utf8'))
      .join('\n');
    expect(workstation).not.toContain("import StatsDashboard from './lib/StatsDashboard.svelte'");
    expect(workstation).toContain(
      "const loadStatsDashboard = () => import('./lib/StatsDashboard.svelte')",
    );
    expect(workstation.match(/\{loadStatsDashboard\}/g)).toHaveLength(2);
    expect(consumers.match(/load=\{loadStatsDashboard\}/g)).toHaveLength(2);
  });

  it('separates generated contracts from literal, allow-listed handwritten IPC', () => {
    const generatedCalls: Array<{ file: string; command: string }> = [];
    const legacyCalls: Array<{ file: string; command: string }> = [];
    const criticalCalls: Array<{ file: string; command: string }> = [];
    let rawLegacyBridgeCalls = 0;
    const violations: string[] = [];

    for (const path of productionSources.filter((candidate) => candidate.endsWith('.ts'))) {
      const name = sourcePath(path);
      const node = ts.createSourceFile(
        path,
        readFileSync(path, 'utf8'),
        ts.ScriptTarget.Latest,
        true,
        ts.ScriptKind.TS,
      );
      const visit = (child: ts.Node): void => {
        if (ts.isCallExpression(child) && ts.isIdentifier(child.expression)) {
          const callee = child.expression.text;
          if (
            callee === 'invokeLegacy' ||
            callee === 'invokeCritical' ||
            callee === 'invokeDesktop' ||
            callee === 'invoke' ||
            callee === '__TAURI_INVOKE'
          ) {
            const first = child.arguments[0];
            const literal =
              first && (ts.isStringLiteral(first) || ts.isNoSubstitutionTemplateLiteral(first));
            if (!literal && callee !== 'invokeDesktop') {
              violations.push(
                `${name}:${node.getLineAndCharacterOfPosition(child.getStart()).line + 1} dynamic ${callee}`,
              );
            }
            if (callee === 'invoke') {
              violations.push(`${name}: raw invoke call escaped the closed legacy adapter`);
            } else if (callee === 'invokeLegacy') {
              if (literal) legacyCalls.push({ file: name, command: first.text });
              if (name !== 'lib/commands.ts') {
                violations.push(`${name}: legacy IPC call outside commands service`);
              }
              if (child.typeArguments?.length !== 1) {
                violations.push(
                  `${name}:${node.getLineAndCharacterOfPosition(child.getStart()).line + 1} untyped legacy IPC`,
                );
              }
            } else if (callee === 'invokeCritical') {
              if (literal) criticalCalls.push({ file: name, command: first.text });
              if (name !== 'lib/commands.ts') {
                violations.push(`${name}: critical IPC call outside commands service`);
              }
              if (child.typeArguments?.length) {
                violations.push(
                  `${name}:${node.getLineAndCharacterOfPosition(child.getStart()).line + 1} critical IPC bypassed its command-specific result type`,
                );
              }
            } else if (callee === 'invokeDesktop') {
              rawLegacyBridgeCalls += 1;
              let owner: ts.Node | undefined = child.parent;
              while (owner && !ts.isFunctionDeclaration(owner)) owner = owner.parent;
              if (
                name !== 'lib/adapters/legacyIpc.ts' ||
                !owner ||
                owner.name?.text !== 'invokeRegistered' ||
                !first ||
                !ts.isIdentifier(first) ||
                first.text !== 'command' ||
                child.typeArguments?.length !== 1
              ) {
                violations.push(`${name}: raw Tauri IPC escaped the closed legacy bridge`);
              }
            } else {
              if (literal) generatedCalls.push({ file: name, command: first.text });
              const parent = child.parent;
              if (
                name !== 'lib/generated/ipc.ts' ||
                !ts.isCallExpression(parent) ||
                !ts.isIdentifier(parent.expression) ||
                parent.expression.text !== 'typedError'
              ) {
                violations.push(`${name}: generated invoke escaped its typedError contract`);
              }
            }
          }
        }
        ts.forEachChild(child, visit);
      };
      visit(node);
    }

    const generatedNames = new Set(generatedCalls.map(({ command }) => command));
    const handwrittenNames = new Set(
      [...legacyCalls, ...criticalCalls].map(({ command }) => command),
    );
    const generatedThroughLegacy = [...generatedNames].filter((command) =>
      handwrittenNames.has(command),
    );

    expect(generatedCalls.length).toBeGreaterThanOrEqual(10);
    expect(criticalCalls.length).toBeGreaterThan(0);
    // Transitional debt may shrink or move to generated bindings, but it cannot grow unnoticed.
    expect(legacyCalls.length).toBeLessThanOrEqual(49);
    expect(rawLegacyBridgeCalls).toBe(1);
    expect(generatedThroughLegacy).toEqual([]);
    expect(violations, violations.join('\n')).toEqual([]);
  });
});
