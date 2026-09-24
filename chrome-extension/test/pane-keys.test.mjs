// What typing into the pane mirror sends, as herdr `pane send-keys` names:
// `node --test chrome-extension/test/`. The names are herdr's, checked against
// a scratch herdr 0.9 pane (a literal character is itself; Space, Tab, Enter,
// Backspace, Escape and the arrows by name).
import { test } from "node:test";
import assert from "node:assert/strict";
import { fitsWrite, keyForInputType, keyForKeyDown, pasteBody, textToKeys } from "../pane-keys.js";

test("typed text is one key per character, whitespace by name, and never a trailing Enter", () => {
  assert.deepEqual(textToKeys("ls -a"), ["l", "s", "Space", "-", "a"]);
  assert.deepEqual(textToKeys("é👍\tx\r\ny\rz"), ["é", "👍", "Tab", "x", "Enter", "y", "Enter", "z"]);
  assert.deepEqual(textToKeys(""), []);
});

test("any whitespace is typed as Space, and control characters not at all, as the factory takes no other", () => {
  // No-break space (macOS Option+Space), ideographic space (a Japanese IME),
  // em space, vertical tab: the factory refuses a whitespace character as a
  // literal key, and would turn typing off.
  assert.deepEqual(textToKeys("a\u00a0b\u3000c\u2003d\ve"), ["a", "Space", "b", "Space", "c", "Space", "d", "Space", "e"]);
  // A control character is not a key the factory takes either.
  assert.deepEqual(textToKeys("\u0007x\u007f\u009by"), ["x", "y"]);
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
  assert.deepEqual(pasteBody("o/r~1", "one\r\ntwo\nthree"), {
    session: "o/r~1",
    text: "\x1b[200~one\rtwo\rthree\x1b[201~",
  });
  // Control characters could end the bracket early and type keys; they are dropped.
  assert.deepEqual(pasteBody("o/r~1", "a\x1b[201~\x03b\tc").text, "\x1b[200~a[201~b\tc\x1b[201~");
  assert.equal(pasteBody("o/r~1", ""), null);
});

test("a long paste is still one write, never pieces a stopped typing could cut short", () => {
  const text = "x".repeat(3000);
  assert.deepEqual(pasteBody("o/r~1", text).text, `\x1b[200~${text}\x1b[201~`);
});

test("a write fits when its JSON body is within the factory's 4096 bytes", () => {
  // {"session":"s","text":""} is 25 bytes, before the text.
  const body = (text) => ({ session: "s", text });
  assert.equal(fitsWrite(body("x".repeat(4071))), true);
  assert.equal(fitsWrite(body("x".repeat(4072))), false);
  // Counted as UTF-8 bytes: é is two.
  assert.equal(fitsWrite(body("é".repeat(2035))), true);
  assert.equal(fitsWrite(body("é".repeat(2036))), false);
  // Counted as sent: an ESC is written \u001b, six bytes.
  assert.equal(fitsWrite(body(`\x1b${"x".repeat(4065)}`)), true);
  assert.equal(fitsWrite(body(`\x1b${"x".repeat(4066)}`)), false);
});
