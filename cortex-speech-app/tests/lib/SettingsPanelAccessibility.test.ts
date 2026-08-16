import { cleanup, fireEvent, render, screen, within } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import SettingsPanel from '../../src/lib/SettingsPanel.svelte';
import { locale } from '../../src/lib/i18n';
import { ckb } from '../../src/lib/i18n/ckb';
import {
  defaultSettings,
  settings,
  settingsTab,
  showSettings,
} from '../../src/lib/stores/settingsStore';

describe('SettingsPanel autonomy accessibility', () => {
  beforeEach(() => {
    delete window.__TAURI__;
    delete window.__TAURI_INTERNALS__;
    locale.set('en');
    settings.set({ ...defaultSettings, juryAutonomyLevel: 'act_confirm' });
    settingsTab.set('jury');
    showSettings.set(true);
  });

  afterEach(() => {
    cleanup();
    settings.set(defaultSettings);
    settingsTab.set('general');
    showSettings.set(false);
    locale.set('ckb');
  });

  it('announces and updates the selected autonomy level', async () => {
    render(SettingsPanel);

    const group = await screen.findByRole('group', { name: 'Autonomy level' });
    const actConfirm = within(group).getByRole('button', { name: /Act\+Confirm/ });
    const actAuto = within(group).getByRole('button', { name: /Act Auto/ });
    expect(actConfirm).toHaveAttribute('aria-pressed', 'true');
    expect(actAuto).toHaveAttribute('aria-pressed', 'false');

    await fireEvent.click(actAuto);
    expect(actConfirm).toHaveAttribute('aria-pressed', 'false');
    expect(actAuto).toHaveAttribute('aria-pressed', 'true');
  });

  it('renders the AI settings copy in Sorani', async () => {
    locale.set('ckb');
    settings.set({ ...defaultSettings, llmMode: 'Gemini' });
    settingsTab.set('ai');

    render(SettingsPanel);

    expect(await screen.findByText(ckb['settings.aiTitle'])).toBeInTheDocument();
    expect(
      screen.getByRole('option', { name: ckb['settings.llmDisabledOption'] }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole('option', { name: ckb['settings.llmCloudOption'] }),
    ).toBeInTheDocument();
    expect(screen.getByTestId('settings-panel')).toHaveTextContent(ckb['settings.apiKeyMissing']);
    expect(screen.getByRole('button', { name: ckb['settings.saveKey'] })).toBeInTheDocument();
    expect(screen.getByText(ckb['settings.systemPromptHint'])).toBeInTheDocument();
  });

  it('renders Jury policy, connection, and key guidance in Sorani', async () => {
    locale.set('ckb');
    settings.set({
      ...defaultSettings,
      juryCloudOptIn: true,
      juryProvider: 'openrouter',
    });
    settingsTab.set('jury');

    render(SettingsPanel);

    expect(
      await screen.findByRole('heading', { name: ckb['settings.juryTitle'] }),
    ).toBeInTheDocument();
    expect(screen.getByText(ckb['settings.juryT1Threshold'])).toBeInTheDocument();
    expect(screen.getByText(ckb['settings.juryModelLabel'])).toBeInTheDocument();
    expect(
      screen.getByRole('option', { name: ckb['settings.juryConnectionOpenRouter'] }),
    ).toBeInTheDocument();
    expect(screen.getByTestId('settings-panel')).toHaveTextContent(
      ckb['settings.juryPolicyDetail'],
    );
    expect(screen.getByText(ckb['settings.openRouterKeyHint'])).toBeInTheDocument();
  });
});
