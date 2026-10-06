// Which ⌘ shortcuts stay with the app inside a remote desktop on a Mac:
//
//   pnpm test

import assert from 'node:assert/strict';
import { test } from 'node:test';
import { macAppShortcut } from '../src/lib/mac-keys.ts';

const key = (code, mods = {}) => ({
  code,
  metaKey: false,
  ctrlKey: false,
  altKey: false,
  shiftKey: false,
  ...mods,
});
const cmd = (code, mods = {}) => key(code, { metaKey: true, ...mods });

test('quit, close tab and settings stay with the app', () => {
  for (const code of ['KeyQ', 'KeyW', 'Comma']) assert.equal(macAppShortcut(cmd(code)), true, code);
});

test('tab switching and full screen stay with the app', () => {
  for (const digit of [1, 5, 9]) {
    assert.equal(macAppShortcut(cmd(`Digit${digit}`, { shiftKey: true })), true);
  }
  assert.equal(macAppShortcut(cmd('Enter', { shiftKey: true })), true);
  assert.equal(macAppShortcut(cmd('KeyF', { ctrlKey: true })), true);
});

test('everything else reaches the server as Windows+key', () => {
  for (const code of [
    'KeyC',
    'KeyV',
    'KeyX',
    'KeyZ',
    'KeyA',
    'KeyL',
    'KeyR',
    'KeyE',
    'KeyN',
    'Tab',
  ]) {
    assert.equal(macAppShortcut(cmd(code)), false, code);
  }
  assert.equal(macAppShortcut(cmd('Digit1')), false);
  assert.equal(macAppShortcut(cmd('Digit0', { shiftKey: true })), false);
  assert.equal(macAppShortcut(cmd('KeyO', { shiftKey: true })), false);
  assert.equal(macAppShortcut(cmd('KeyD', { shiftKey: true })), false);
  assert.equal(macAppShortcut(cmd('KeyQ', { altKey: true })), false);
  assert.equal(macAppShortcut(cmd('KeyW', { shiftKey: true })), false);
});

test('keys without ⌘ never count', () => {
  assert.equal(macAppShortcut(key('KeyQ')), false);
  assert.equal(macAppShortcut(key('KeyW', { ctrlKey: true })), false);
});
