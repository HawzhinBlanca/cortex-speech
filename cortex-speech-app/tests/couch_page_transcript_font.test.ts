import { readFileSync } from 'fs';
import path from 'path';
import { describe, it, expect } from 'vitest';

/**
 * The reviewer reads Central Kurdish out of this textarea and judges the clip by eye. That makes the
 * font a correctness concern, not a cosmetic one.
 *
 * A `<textarea>` does NOT inherit font-family, and every browser's UA stylesheet defaults it to
 * monospace. Monospaced metrics force cursive Arabic-script letters — whose natural widths vary
 * enormously and which JOIN to their neighbours — into identical advance cells. The joins then render
 * stretched and gappy, so the text looks padded with tatweel (ـ) that is not in it.
 *
 * Measured on the live page 2026-08-10: `fontFamily` computed to "monospace" while the transcript's
 * tatweel count was 0 and its codepoints were clean Sorani (ک U+06A9, ی U+06CC). The owner saw the
 * rendering and asked whether the transcript was corrupt. It was not — the stylesheet was.
 *
 * Pinned at the source: jsdom does not apply UA-stylesheet defaults, so it cannot reproduce the very
 * default this guards against, and a computed-style assertion there would pass against broken CSS.
 */
const PAGE = path.resolve(__dirname, '..', 'src-tauri', 'assets', 'couch.html');

/** The body of the first CSS rule whose selector list contains `selector`. */
function cssRuleBody(source: string, selector: string): string {
  const style = source.slice(source.indexOf('<style'), source.indexOf('</style>'));
  const re = new RegExp(`(^|[},;\\s])${selector}\\s*\\{([^}]*)\\}`, 'm');
  const match = style.match(re);
  if (!match) throw new Error(`no '${selector}' CSS rule found in couch.html — this test would pass vacuously`);
  return match[2];
}

describe('couch.html transcript font', () => {
  it('sets an explicit font-family on the textarea, which never inherits one', () => {
    const body = cssRuleBody(readFileSync(PAGE, 'utf8'), 'textarea');
    expect(
      /font-family\s*:/.test(body),
      'the transcript textarea has no font-family, so it falls back to the UA default (monospace), '
        + 'which renders joined Kurdish script stretched and gappy',
    ).toBe(true);
  });

  it('inherits from a body font that is not monospace', () => {
    const source = readFileSync(PAGE, 'utf8');
    const textarea = cssRuleBody(source, 'textarea');
    const bodyRule = cssRuleBody(source, 'body');

    // `inherit` is only correct while the thing it inherits FROM is a proportional face.
    if (/font-family\s*:\s*inherit/.test(textarea)) {
      expect(
        /font-family\s*:/.test(bodyRule),
        'the textarea inherits its font-family but body declares none, so both fall back to the UA default',
      ).toBe(true);
      expect(
        /font-family\s*:[^;]*monospace/.test(bodyRule),
        'body resolves to a monospace face, which is what the textarea then inherits',
      ).toBe(false);
    } else {
      expect(
        /font-family\s*:[^;]*monospace/.test(textarea),
        'the transcript must not be rendered in a monospace face',
      ).toBe(false);
    }
  });
});
