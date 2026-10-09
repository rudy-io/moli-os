#!/usr/bin/env node
// Garde de la traduction (chantier 3).
//
//   node scripts/i18n-check.mjs            catalogues + textes en dur ; code 1 au moindre écart
//   node scripts/i18n-check.mjs --report   le décompte des textes en dur par fichier, sans échouer
//
// Catalogues : locales/<langue>/<zone>.json, objet plat ; chaque clé commence par « <zone>. » ;
// le français est la référence : toute clé existe dans chaque langue, avec les mêmes {paramètres}.
// Textes en dur : une ligne d'interface (web/src, hors commentaires et tests) qui porte du français
// visible. Exceptions motivées : scripts/i18n-allow.txt (« chemin  expression »).

import { readFileSync, readdirSync, existsSync } from 'node:fs';
import { join, relative } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = join(fileURLToPath(new URL('.', import.meta.url)), '..');
const report = process.argv.includes('--report');
const problems = [];

// ---- catalogues ---------------------------------------------------------------
const locales = join(root, 'locales');
const catalogues = {};
for (const language of readdirSync(locales)) {
  catalogues[language] = {};
  for (const file of readdirSync(join(locales, language)).filter((f) => f.endsWith('.json'))) {
    const area = file.replace(/\.json$/, '');
    let words;
    try {
      words = JSON.parse(readFileSync(join(locales, language, file), 'utf8'));
    } catch (e) {
      problems.push(`locales/${language}/${file} : JSON illisible (${e.message})`);
      continue;
    }
    for (const [key, text] of Object.entries(words)) {
      if (!key.startsWith(`${area}.`)) problems.push(`locales/${language}/${file} : « ${key} » ne commence pas par « ${area}. »`);
      if (typeof text !== 'string') problems.push(`locales/${language}/${file} : « ${key} » n'est pas un texte`);
      if (catalogues[language][key] != null) problems.push(`locales/${language}/${file} : « ${key} » en double`);
      catalogues[language][key] = text;
    }
  }
}
const params = (text) => [...String(text).matchAll(/\{(\w+)\}/g)].map((m) => m[1]).sort().join(',');
const reference = catalogues.fr ?? {};
for (const [language, words] of Object.entries(catalogues)) {
  if (language === 'fr') continue;
  for (const [key, text] of Object.entries(reference)) {
    if (words[key] == null) problems.push(`${language} : il manque « ${key} »`);
    else if (params(words[key]) !== params(text)) problems.push(`${language} : « ${key} » n'a pas les mêmes paramètres que le français`);
  }
  for (const key of Object.keys(words)) if (reference[key] == null) problems.push(`${language} : « ${key} » n'existe pas en français`);
}

// ---- textes en dur dans l'interface -------------------------------------------------
const allow = existsSync(join(root, 'scripts/i18n-allow.txt'))
  ? readFileSync(join(root, 'scripts/i18n-allow.txt'), 'utf8')
      .split('\n')
      .map((l) => l.trim())
      .filter((l) => l && !l.startsWith('#'))
      .map((l) => {
        const [path, ...rest] = l.split(/\s+/);
        return { path, re: new RegExp(rest.join(' ')) };
      })
  : [];
const FRENCH = /[àâçéèêëîïôûùüœÀÂÇÉÈÊ’«»]|\b(le|la|les|des|une|du|pour|avec|sans|pas|est|sont|aucun|cette|ici|encore|déjà)\b/i;
const VISIBLE = />[^<{}]*[A-Za-zÀ-ÿ]{2}[^<{}]*</; // text between tags
const STRING = /(['"`])((?:(?!\1)[^\\]|\\.){3,})\1/g;
const hard = new Map();

function walk(dir) {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) walk(path);
    else if (/\.(svelte|js)$/.test(entry.name) && !/\.test\.|\.check\./.test(entry.name)) scan(path);
  }
}

function scan(path) {
  const rel = relative(root, path).replaceAll('\\', '/');
  let inComment = false;
  let inStyle = false;
  readFileSync(path, 'utf8')
    .split('\n')
    .forEach((line, i) => {
      const s = line.trim();
      if (s.startsWith('<style')) inStyle = true;
      if (inStyle) {
        if (s.startsWith('</style>')) inStyle = false;
        return;
      }
      if (inComment) {
        if (s.includes('*/') || s.includes('-->')) inComment = false;
        return;
      }
      if (s.startsWith('/*') || s.startsWith('<!--')) {
        if (!s.includes('*/') && !s.includes('-->')) inComment = true;
        return;
      }
      if (s.startsWith('//') || s.startsWith('*') || s.startsWith('import ')) return;
      // Translation keys (`t('zone.cle')`) and dotted identifiers are not text.
      const code = s.replace(/\/\/ .*$/, '').replace(/\bt\(\s*(['"`])[\w.-]+\1/g, 't(').replace(/(['"])[a-z_]+(\.[a-z0-9_]+)+\1/g, "''");
      const texts = [...code.matchAll(STRING)].map((m) => m[2]);
      const visible = VISIBLE.test(code) ? [code.match(VISIBLE)[0]] : [];
      const french = [...texts, ...visible].filter((t) => FRENCH.test(t) && /[a-zà-ÿ]{3}/i.test(t));
      if (!french.length) return;
      if (allow.some((a) => rel.startsWith(a.path) && a.re.test(code))) return;
      if (!hard.has(rel)) hard.set(rel, []);
      hard.get(rel).push(`${i + 1}: ${s.slice(0, 110)}`);
    });
}
walk(join(root, 'web/src'));

// ---- verdict -----------------------------------------------------------------------
const total = [...hard.values()].reduce((n, l) => n + l.length, 0);
if (report) {
  for (const [file, lines] of [...hard].sort((a, b) => b[1].length - a[1].length)) console.log(`${String(lines.length).padStart(4)}  ${file}`);
}
console.log(`i18n-check : ${Object.keys(reference).length} clés, langues ${Object.keys(catalogues).sort().join(', ')} ; ${total} ligne(s) de texte en dur dans ${hard.size} fichier(s).`);
for (const p of problems) console.error(`  ${p}`);
if (!report && total) for (const [file, lines] of hard) for (const l of lines) console.error(`  ${file}:${l}`);
process.exit(problems.length || (!report && total) ? 1 : 0);
