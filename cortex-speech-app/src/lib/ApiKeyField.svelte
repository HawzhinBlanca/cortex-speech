<script lang="ts">
  import { t, type TranslationKey } from './i18n';

  interface Props {
    labelKey: TranslationKey;
    hintKey: TranslationKey;
    placeholder: string;
    configured: boolean;
    value?: string;
    saving: boolean;
    disabled: boolean;
    onSave: () => Promise<void>;
  }

  let {
    labelKey,
    hintKey,
    placeholder,
    configured,
    value = $bindable(''),
    saving,
    disabled,
    onSave,
  }: Props = $props();

  function save() {
    void onSave().catch(() => {
      // The owning Settings panel keeps the field mounted, surfaces the localized failure, and
      // preserves the exact secret for retry. Consume only the already-reported rejection here.
    });
  }
</script>

<label class="flex flex-col gap-1">
  <span class="text-sm text-muted">
    {$t(labelKey)}
    {#if configured}
      <span class="ms-2 text-[10px] text-emerald-400">{$t('settings.apiKeySaved')}</span>
    {:else}
      <span class="ms-2 text-[10px] text-amber-400">{$t('settings.apiKeyMissing')}</span>
    {/if}
  </span>
  <div class="flex gap-2">
    <input
      type="password"
      class="input flex-1"
      bind:value
      {placeholder}
      dir="ltr"
      autocomplete="off"
      {disabled}
      onkeydown={(event) => {
        if (event.key === 'Enter') save();
      }}
    />
    <button type="button" class="btn-secondary text-xs px-3" {disabled} onclick={save}>
      {saving ? $t('settings.savingKey') : $t('settings.saveKey')}
    </button>
  </div>
  <span class="text-[10px] text-subtle">{$t(hintKey)}</span>
</label>
