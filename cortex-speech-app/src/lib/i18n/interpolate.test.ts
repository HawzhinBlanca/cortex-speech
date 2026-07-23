import { describe, it, expect } from 'vitest';
import { get } from 'svelte/store';
import { t } from './index';

describe('t() interpolation', () => {
  it('substitutes EVERY occurrence of a repeated placeholder, not just the first', () => {
    // speaker.mergeConfirm (both locales) uses {target} twice; the write was `replace`, which left the
    // second "{target}" literal in a destructive rename/merge confirmation dialog. Guard the root fix.
    const translate = get(t);
    const msg = translate('speaker.mergeConfirm', { source: 'Alice', target: 'Bob', n: '3' });
    expect(msg).not.toContain('{target}');
    expect(msg).not.toContain('{'); // no placeholder of any kind left unsubstituted
    // {target} appears twice in the template → 'Bob' must appear twice in the output
    expect(msg.split('Bob').length - 1).toBe(2);
  });
});
