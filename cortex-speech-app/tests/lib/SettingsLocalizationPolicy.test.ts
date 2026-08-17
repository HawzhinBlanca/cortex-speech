import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';

const root = resolve(import.meta.dirname, '../..');

describe('settings and playback localization policy', () => {
  it('keeps AI and Jury settings prose behind translation keys', () => {
    const source = readFileSync(resolve(root, 'src/lib/SettingsPanel.svelte'), 'utf8');
    expect(source).toContain("labelKey: 'settings.aiTab'");
    expect(source).toContain("labelKey: 'settings.juryTab'");
    const start = source.indexOf("{:else if activeTab === 'ai'}");
    const end = source.indexOf("{:else if activeTab === 'diagnostics'}", start);
    expect(start).toBeGreaterThan(-1);
    expect(end).toBeGreaterThan(start);

    const section = source.slice(start, end).replace(/<!--[\s\S]*?-->/g, '');
    const visibleLatinText = [...section.matchAll(/>([^<{]*[A-Za-z][^<{]*)</g)]
      .map((match) => match[1].trim())
      .filter(Boolean);

    // Product/model identifiers are intentionally not translated. Every other visible text node in
    // these two sections must be rendered through $t(...).
    expect(visibleLatinText).toEqual(['heretic-final:latest', 'qwen2.5-coder:7b']);

    const technicalPlaceholders = [
      ...section.matchAll(/(?:title|aria-label|placeholder)="([^"]*[A-Za-z][^"]*)"/g),
    ].map((match) => match[1]);
    expect(technicalPlaceholders).toEqual([
      '/mnt/c/path/to/provider_refine.py',
      'http://127.0.0.1:11434/v1/chat/completions',
      'heretic-final:latest',
      'AIzaSy...',
      'gemini-2.5-pro',
      'gemini-2.5-pro',
      'gemini-2.5-pro, gemini-2.5-flash',
      'AIzaSy…',
      'sk-or-…',
    ]);

    const requiredKeys = [
      'settings.aiTitle',
      'settings.aiDescription',
      'settings.externalAsrScriptHint',
      'settings.llmDisabledOption',
      'settings.llmLocalOption',
      'settings.llmCloudOption',
      'settings.localEndpointHint',
      'settings.quickSelect',
      'settings.apiKeySaved',
      'settings.apiKeyMissing',
      'settings.savingKey',
      'settings.saveKey',
      'settings.apiKeyStorageHint',
      'settings.systemPromptHint',
      'settings.juryTitle',
      'settings.juryDescription',
      'settings.autonomyHint',
      'settings.juryT1Threshold',
      'settings.juryT1ThresholdHint',
      'settings.juryModelLabel',
      'settings.selfConsistencyLabel',
      'settings.selfConsistencyHint',
      'settings.juryCloudDisabled',
      'settings.jurySharedKeyHint',
      'settings.juryConnection',
      'settings.juryConnectionGemini',
      'settings.juryConnectionOpenRouter',
      'settings.juryPolicyLead',
      'settings.juryPolicyModel',
      'settings.juryPolicyDetail',
      'settings.openRouterApiKey',
      'settings.openRouterKeyHint',
    ];
    for (const key of requiredKeys) expect(section).toContain(`$t('${key}')`);

    for (const hardcodedNotice of [
      "'OpenRouter key saved to secrets.env'",
      "'OpenRouter key cleared'",
      "'Failed to save OpenRouter key'",
      "'Gemini key saved to secrets.env'",
      "'Gemini key cleared'",
      "'Failed to save Gemini key'",
    ]) {
      expect(source).not.toContain(hardcodedNotice);
    }
    expect(source).toContain("$t('settings.apiKeySavedToast'");
    expect(source).toContain("$t('settings.apiKeySaveFailedToast'");
  });

  it('keeps the Inbox local-only notice behind translation keys', () => {
    const source = readFileSync(resolve(root, 'src/lib/ReviewInbox.svelte'), 'utf8');
    expect(source).toContain("$t('inbox.localOnly')");
    expect(source).toContain("$t('inbox.localOnlyTitle')");
    expect(source).not.toContain('>🔒 Local only<');
    expect(source).not.toContain('title="Cloud T2 (Gemini)');
  });

  it('keeps AudioPlayer controls and user-facing failures behind translation keys', () => {
    const source = readFileSync(resolve(root, 'src/lib/AudioPlayer.svelte'), 'utf8');
    const requiredKeys = [
      'audio.controls',
      'audio.play',
      'audio.pause',
      'audio.playbackSpeed',
      'audio.loopToggle',
      'audio.loopOn',
      'audio.loopOff',
      'audio.loopFailed',
      'audio.playbackFailed',
      'audio.loadFailed',
      'retry',
    ];
    for (const key of requiredKeys) {
      const expected =
        key === 'audio.loopOn' || key === 'audio.loopOff' ? `'${key}'` : `$t('${key}')`;
      expect(source).toContain(expected);
    }

    for (const hardcodedCopy of [
      'aria-label="Audio player controls"',
      "aria-label={playing ? 'Pause' : 'Play'}",
      'aria-label="Playback Speed"',
      'aria-label="Toggle Loop Playback"',
      "attemptPlay('Loop playback failed')",
      "attemptPlay('Playback blocked or file not found')",
      "error = 'Failed to load audio file'",
    ]) {
      expect(source).not.toContain(hardcodedCopy);
    }
  });
});
