// Every German string the app translates has an English one.
//
//   node scripts/check-i18n.mjs
//
// Finds the string literals passed to t() and N_() in the desktop app and
// checks the English catalogue (apps/desktop/src/i18n/en/*.json)
// has each of them. Also reports a German string with two different English
// translations in different catalogue files. Exits non-zero on a problem.

import { readdirSync, readFileSync, statSync } from 'node:fs';
import { dirname, join, relative } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const sources = [join(root, 'apps/desktop/src')];
const catalogue = join(root, 'apps/desktop/src/i18n/en');

const files = (dir) =>
  readdirSync(dir).flatMap((name) => {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) return name === 'i18n' ? [] : files(path);
    return /\.(ts|tsx)$/.test(name) ? [path] : [];
  });

const english = new Map();
const problems = [];
for (const name of readdirSync(catalogue).filter((n) => n.endsWith('.json'))) {
  const entries = JSON.parse(readFileSync(join(catalogue, name), 'utf8'));
  for (const [german, translated] of Object.entries(entries)) {
    if (typeof translated !== 'string' || !translated.trim()) {
      problems.push(`${name}: empty translation for ${JSON.stringify(german)}`);
      continue;
    }
    const known = english.get(german);
    if (known && known.text !== translated) {
      problems.push(
        `${JSON.stringify(german)} is ${JSON.stringify(known.text)} in ${known.file} but ${JSON.stringify(translated)} in ${name}`,
      );
    }
    english.set(german, { text: translated, file: name });
  }
}

// t('…'), t("…"), t(`…`) without ${}, and the same for N_().
const call = /\b(?:t|N_)\(\s*(?:'((?:\\.|[^'\\])*)'|"((?:\\.|[^"\\])*)"|`((?:\\.|[^`\\$])*)`)/g;
const unescape = (text) =>
  text.replace(/\\(u\{[0-9a-fA-F]+\}|u[0-9a-fA-F]{4}|.)/g, (_, escape) => {
    if (escape.startsWith('u{')) return String.fromCodePoint(parseInt(escape.slice(2, -1), 16));
    if (escape.startsWith('u') && escape.length === 5)
      return String.fromCharCode(parseInt(escape.slice(1), 16));
    return { n: '\n', t: '\t' }[escape] ?? escape;
  });

let used = 0;
const seen = new Set();
for (const dir of sources) {
  for (const file of files(dir)) {
    const text = readFileSync(file, 'utf8');
    for (const match of text.matchAll(call)) {
      const german = unescape(match[1] ?? match[2] ?? match[3] ?? '');
      used += 1;
      seen.add(german);
      if (!english.has(german)) {
        const line = text.slice(0, match.index).split('\n').length;
        problems.push(`${relative(root, file)}:${line} has no English: ${JSON.stringify(german)}`);
      }
    }
  }
}

const unused = [...english.keys()].filter((german) => !seen.has(german));
if (unused.length) {
  console.warn(`${unused.length} English entries are not used any more:`);
  for (const german of unused.slice(0, 20)) console.warn(`  ${JSON.stringify(german)}`);
}

if (problems.length) {
  console.error(problems.join('\n'));
  console.error(`\n✗ ${problems.length} translation problems`);
  process.exit(1);
}
console.log(`✓ ${seen.size} strings, ${used} uses, all with English`);
