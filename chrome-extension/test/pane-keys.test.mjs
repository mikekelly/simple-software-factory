// What typing into the pane mirror sends, as herdr `pane send-keys` names:
// `node --test chrome-extension/test/`. The names are herdr's, checked against
// a scratch herdr 0.9 pane (a literal character is itself; Space, Tab, Enter,
// Backspace, Escape and the arrows by name).
import { test } from "node:test";
import assert from "node:assert/strict";
import { keyForInputType, keyForKeyDown, pasteChunks, textToKeys } from "../pane-keys.js";

test("typed text is one key per character, whitespace by name, and never a trailing Enter", () => {
  assert.deepEqual(textToKeys("ls -a"), ["l", "s", "Space", "-", "a"]);
  assert.deepEqual(textToKeys("é👍\tx\r\ny\rz"), ["é", "👍", "Tab", "x", "Enter", "y", "Enter", "z"]);
  assert.deepEqual(textToKeys(""), []);
});

test("keys a text field does not type are named from keydown", () => {
  const key = (key, mods = {}) => keyForKeyDown({ key, ctrlKey: false, altKey: false, metaKey: false, shiftKey: false, ...mods });
  assert.equal(key("Escape"), "Escape");
  assert.equal(key("Tab"), "Tab");
  assert.equal(key("Tab", { shiftKey: true }), "shift+Tab");
  assert.equal(key("ArrowUp"), "Up");
  assert.equal(key("ArrowDown"), "Down");
  assert.equal(key("ArrowLeft"), "Left");
  assert.equal(key("ArrowRight"), "Right");
  assert.equal(key("Backspace"), "Backspace");
  assert.equal(key("Enter"), "Enter");
  // Ctrl with a letter is the terminal's chord, except Ctrl+V, which is the
  // browser's paste and arrives as a paste.
  assert.equal(key("c", { ctrlKey: true }), "ctrl+c");
  assert.equal(key("v", { ctrlKey: true }), undefined);
  // Printable keys are typed by the field; the browser's own chords are left alone.
  assert.equal(key("a"), undefined);
  assert.equal(key("r", { metaKey: true }), undefined);
  assert.equal(key("ArrowLeft", { altKey: true }), undefined);
  // Inherited names are not keys.
  assert.equal(key("constructor"), undefined);
});

test("a virtual keyboard's Backspace and Enter are named from beforeinput", () => {
  assert.equal(keyForInputType("deleteContentBackward"), "Backspace");
  assert.equal(keyForInputType("insertLineBreak"), "Enter");
  assert.equal(keyForInputType("insertParagraph"), "Enter");
  assert.equal(keyForInputType("insertText"), null);
});

test("a paste is sent as text, one bracketed paste, its line breaks as a terminal sends them", () => {
  assert.deepEqual(pasteChunks("one\r\ntwo\nthree", 500), ["\x1b[200~one\rtwo\rthree\x1b[201~"]);
  // Control characters could end the bracket early and type keys; they are dropped.
  assert.deepEqual(pasteChunks("a\x1b[201~\x03b\tc", 500), ["\x1b[200~a[201~b\tc\x1b[201~"]);
  assert.deepEqual(pasteChunks("", 500), []);
});

test("a long paste goes in pieces, never splitting a character", () => {
  const chunks = pasteChunks("ab👍cd", 2);
  assert.deepEqual(chunks, ["\x1b[200~ab", "👍c", "d\x1b[201~"]);
});
