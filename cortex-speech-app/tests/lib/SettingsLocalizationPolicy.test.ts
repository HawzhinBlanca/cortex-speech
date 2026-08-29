import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';

const root = resolve(import.meta.dirname, '../..');

describe('settings and playback localization policy', () => {
  it('keeps AI and Jury settings prose behind translation keys', () => {
    const owner = readFileSync(resolve(root, 'src/lib/SettingsPanel.svelte'), 'utf8');
    const ai = readFileSync(resolve(root, 'src/lib/SettingsAiTab.svelte'), 'utf8');
    const jury = readFileSync(resolve(root, 'src/lib/SettingsJuryTab.svelte'), 'utf8');
    const keyController = readFileSync(
      resolve(root, 'src/lib/settingsKeyController.svelte.ts'),
      'utf8',
    );
    const source = `${owner}\n${ai}\n${jury}\n${keyController}`;
    const apiKeyField = readFileSync(resolve(root, 'src/lib/ApiKeyField.svelte'), 'utf8');
    expect(owner).toContain("labelKey: 'settings.aiTab'");
    expect(owner).toContain("labelKey: 'settings.juryTab'");
    expect(owner).toContain("import SettingsAiTab from './SettingsAiTab.svelte'");
    expect(owner).toContain("import SettingsJuryTab from './SettingsJuryTab.svelte'");
    expect(ai).toContain("import ApiKeyField from './ApiKeyField.svelte'");
    expect(jury).toContain("import ApiKeyField from './ApiKeyField.svelte'");

    const markup = (component: string) => component.slice(component.indexOf('</script>') + 9);
    const section = `${markup(ai)}\n${markup(jury)}`
      .replace(/<!--[\s\S]*?-->/g, '')
      .replaceAll('=>', '⇒');
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
      'settings.systemPromptHint',
      'settings.juryTitle',
      'settings.juryDescription',
      'settings.autonomyHint',
      'settings.juryT1Threshold',
      'settings.juryT1ThresholdHint',
      'settings.juryModelLabel',
      'settings.advisoryModelName',
      'settings.modelFixedByPolicy',
      'settings.sourceReferenceFixedHint',
      'settings.selfConsistencyLabel',
      'settings.selfConsistencyHint',
      'settings.juryCloudDisabled',
      'settings.juryConnection',
      'settings.juryConnectionGemini',
      'settings.juryConnectionOpenRouter',
      'settings.juryPolicyLead',
      'settings.juryPolicyModel',
      'settings.juryPolicyDetail',
    ];
    for (const key of requiredKeys) expect(section).toContain(`$t('${key}')`);

    for (const key of [
      'settings.apiKeySaved',
      'settings.apiKeyMissing',
      'settings.savingKey',
      'settings.saveKey',
    ]) {
      expect(apiKeyField).toContain(`$t('${key}')`);
    }
    expect(apiKeyField).toContain('{$t(labelKey)}');
    expect(apiKeyField).toContain('{$t(hintKey)}');
    for (const binding of [
      'labelKey="settings.geminiApiKey"',
      'labelKey="settings.openRouterApiKey"',
      'hintKey="settings.apiKeyStorageHint"',
      'hintKey="settings.jurySharedKeyHint"',
      'hintKey="settings.openRouterKeyHint"',
    ]) {
      expect(section).toContain(binding);
    }

    for (const hardcodedNotice of [
      "'OpenRouter key saved to secrets.env'",
      "'OpenRouter key cleared'",
      "'Failed to save OpenRouter key'",
      "'Gemini key saved to secrets.env'",
      "'Gemini key cleared'",
      "'Failed to save Gemini key'",
    ]) {
      expect(source).not.toContain(hardcodedNotice);
      expect(apiKeyField).not.toContain(hardcodedNotice);
    }
    expect(keyController).toContain("translate('settings.apiKeySavedToast'");
    expect(keyController).toContain("get(t)('settings.apiKeySaveFailedToast'");
  });

  it('keeps the Inbox local-only notice behind translation keys', () => {
    const owner = readFileSync(resolve(root, 'src/lib/ReviewInbox.svelte'), 'utf8');
    const header = readFileSync(resolve(root, 'src/lib/ReviewInboxHeader.svelte'), 'utf8');
    expect(owner).toContain("import ReviewInboxHeader from './ReviewInboxHeader.svelte'");
    expect(owner).toContain('<ReviewInboxHeader');
    expect(header).toContain("$t('inbox.localOnly')");
    expect(header).toContain("$t('inbox.localOnlyTitle')");
    expect(header).not.toContain('>🔒 Local only<');
    expect(header).not.toContain('title="Cloud T2 (Gemini)');
  });

  it('keeps AudioPlayer controls and user-facing failures behind translation keys', () => {
    const source = readFileSync(resolve(root, 'src/lib/AudioPlayer.svelte'), 'utf8');
    const controller = readFileSync(resolve(root, 'src/lib/audioPlayerController.ts'), 'utf8');
    const playbackSurface = `${source}\n${controller}`;
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
      'audio.proofFailed',
      'retry',
    ];
    for (const key of requiredKeys) {
      const expected = [
        'audio.loopFailed',
        'audio.playbackFailed',
        'audio.loadFailed',
        'audio.proofFailed',
      ].includes(key)
        ? `translate('${key}')`
        : key === 'audio.loopOn' || key === 'audio.loopOff'
          ? `'${key}'`
          : `$t('${key}')`;
      expect(playbackSurface).toContain(expected);
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
      expect(playbackSurface).not.toContain(hardcodedCopy);
    }
  });
});
