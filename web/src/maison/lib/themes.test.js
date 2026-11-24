import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { BASE, BUILTIN, readTheme, themeList, pickTheme, resolveTokens, applyTheme, contrast, parseHex, toHex, themeName } from './themes.js';

// The tokens maison.css gives `.maison` (day) and `.maison[data-theme='night']`.
function cssTokens() {
  const css = readFileSync(new URL('../maison.css', import.meta.url), 'utf8');
  const block = (selector) => {
    const at = css.indexOf(`${selector} {`);
    return css.slice(at, css.indexOf('}', at));
  };
  const read = (text) => {
    const out = {};
    for (const [, key, value] of text.matchAll(/--([a-z0-9-]+):\s*([^;]+);/g)) out[key] = value.trim();
    return out;
  };
  return { day: read(block('.maison')), night: read(block(".maison[data-theme='night']")) };
}

const rgbToHex = (value) => {
  const m = value.match(/^rgb\((\d+) (\d+) (\d+) \/ (\d+)%\)$/);
  return m ? toHex([+m[1], +m[2], +m[3], +m[4] / 100]) : value;
};

test('the base tokens are those of maison.css', () => {
  const css = cssTokens();
  for (const mode of ['day', 'night']) {
    for (const [key, value] of Object.entries(BASE[mode])) {
      const written = css[mode][key] ?? css.day[key];
      assert.ok(written, `${mode} ${key} is in maison.css`);
      assert.equal(toHex(parseHex(rgbToHex(written)) ?? [0, 0, 0]), value, `${mode} ${key}`);
    }
  }
});

test("Moli's own themes need no correction, day or night", () => {
  for (const raw of BUILTIN) {
    const { theme, problems } = readTheme(raw);
    assert.deepEqual(problems, [], raw.id);
    for (const mode of ['day', 'night']) assert.deepEqual(resolveTokens(theme, mode).problems, [], `${raw.id} ${mode}`);
  }
});

test('Porcelaine is the stylesheet itself: nothing inline', () => {
  const porcelaine = readTheme(BUILTIN[0]).theme;
  for (const mode of ['day', 'night']) {
    const out = applyTheme(porcelaine, mode);
    assert.equal(out.style, '');
    assert.equal(out.wallpaper, null);
    assert.equal(out.bg, BASE[mode].bg);
  }
});

test('only plain hex colours get through', () => {
  const { theme, problems } = readTheme({
    id: 'essai',
    day: { ink: 'red; background: url(x)', surface: '#FFF', cool: 'rgb(1 2 3)', nope: '#000' },
  });
  assert.deepEqual(theme.day, { surface: '#ffffff' });
  assert.deepEqual(
    problems.map((p) => [p.code, p.key]),
    [['color', 'day.ink'], ['color', 'day.cool'], ['unknown', 'day.nope']],
  );
});

test('a theme without a usable id is not a theme', () => {
  assert.equal(readTheme({ name: 'Sans id' }).theme, null);
  assert.equal(readTheme({ id: 'Avec Espace' }).theme, null);
  assert.equal(readTheme(null).theme, null);
});

test('a wallpaper is an image Moli keeps, never an address elsewhere nor CSS', () => {
  const ok = readTheme({ id: 'a', background: { image: '/api/plan/images/0123456789abcdef.jpg', dim: 0.3 } });
  assert.deepEqual(ok.theme.background, { image: '/api/plan/images/0123456789abcdef.jpg', dim: 0.3 });
  for (const image of [
    'https://example.com/fond.jpg', '//example.com/fond.jpg', '/\\example.com/fond.jpg', 'http://example.com/a.jpg',
    '/api/plan/images/../../x.jpg', '/api/plan/images/0123456789ABCDEF.jpg', '/api/plan/images/fond.jpg',
    '/api/plan/images/0123456789abcdef.jpg?x', '/x.jpg") ; color: red', 'javascript:alert(1)', '/a b.jpg', "/a'.jpg",
  ]) {
    const { theme, problems } = readTheme({ id: 'c', background: { image } });
    assert.equal(theme.background, null, image);
    assert.equal(problems[0].code, 'image', image);
  }
  assert.equal(readTheme({ id: 'd', background: { dim: 2, gradient: ['#fff'] } }).problems.length, 2);
});

test('the wallpaper layers: veil, image, gradient, then the colour', () => {
  const { theme } = readTheme({ id: 'a', background: { image: '/api/plan/images/00000000000000ff.jpg', dim: 0.5 }, night: { background: { gradient: ['#000', '#111'] } } });
  const day = applyTheme(theme, 'day');
  assert.equal(day.wallpaper, 'linear-gradient(#edf0f580, #edf0f580), url("/api/plan/images/00000000000000ff.jpg") center / cover no-repeat, #edf0f5');
  assert.equal(applyTheme(theme, 'night').wallpaper, 'linear-gradient(160deg, #000000, #111111), #0d1016');
});

test('unreadable text is replaced, and said', () => {
  // A pale grey text on white, in day.
  const { theme } = readTheme({ id: 'pale', day: { ink: '#dddddd' } });
  const { tokens, problems } = resolveTokens(theme, 'day');
  assert.equal(tokens.ink, BASE.day.ink);
  assert.deepEqual(problems[0], { code: 'contrast', key: 'ink', against: 'surface', value: '#dddddd', now: BASE.day.ink });
});

test('a dark surface by day takes the light ink', () => {
  const { theme } = readTheme({ id: 'sombre', day: { bg: '#101010', surface: '#202020' } });
  const { tokens } = resolveTokens(theme, 'day');
  assert.ok(contrast(tokens.ink, tokens.surface) >= 4.5);
  assert.ok(contrast(tokens['ink-2'], tokens.surface) >= 4.5);
  assert.ok(contrast(tokens['ink-3'], tokens.surface) >= 4);
});

test('an accent brings its soft side and the text that sits on it', () => {
  const { theme } = readTheme({ id: 'jaune', day: { cool: '#ffe14d' } });
  const { tokens, problems } = resolveTokens(theme, 'day');
  assert.deepEqual(problems, []);
  assert.equal(tokens['on-cool'], BASE.day.ink, 'dark text on a light yellow button');
  assert.notEqual(tokens['cool-soft'], BASE.day['cool-soft']);
  assert.equal(resolveTokens(readTheme({ id: 'bleu', day: { cool: '#1f4fd0' } }).theme, 'day').tokens['on-cool'], '#ffffff');
});

test('font and corners are words from a short list', () => {
  const { theme, problems } = readTheme({ id: 'a', font: 'serif', corners: 'square' });
  assert.deepEqual(problems, []);
  const { style } = applyTheme(theme, 'day');
  assert.match(style, /--font: 'Iowan Old Style'/);
  assert.match(style, /--r-xl: 10px/);
  assert.deepEqual(readTheme({ id: 'b', font: 'Comic Sans; color: red', corners: 3 }).problems.map((p) => p.code), ['font', 'corners']);
});

test("the house's themes follow Moli's, and replace one of the same id", () => {
  const { list, problems } = themeList({
    themes: [{ id: 'maison', name: { fr: 'Chez nous', en: 'Our home' }, day: { warm: 'orange' } }, { id: 'ocean', name: 'Notre océan' }, { name: 'sans id' }],
  });
  assert.deepEqual(list.map((t) => t.id), ['porcelaine', 'sauge', 'ocean', 'terracotta', 'maison']);
  assert.equal(list[2].name, 'Notre océan');
  assert.equal(list[2].builtin, undefined);
  assert.deepEqual(Object.keys(problems), ['maison']);
  assert.equal(themeName(list[4], 'en'), 'Our home');
  assert.equal(themeName(list[4], 'de'), 'Chez nous');
});

test("a screen's choice, else the house's, else Porcelaine", () => {
  const { list } = themeList({ themes: [{ id: 'maison' }] });
  assert.equal(pickTheme(list, 'sauge', 'maison').id, 'sauge');
  assert.equal(pickTheme(list, '', 'maison').id, 'maison');
  assert.equal(pickTheme(list, 'disparu', 'disparu').id, 'porcelaine');
});
