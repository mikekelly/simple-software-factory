// What typing into the pane mirror sends (#477): herdr `pane send-keys` names,
// a keystroke at a time, the way a person types into a terminal.
//
// Ported to plain JavaScript from collie (https://github.com/AltanS/collie,
// commit 733235df: web/src/lib/key-queue.ts and
// web/src/hooks/use-direct-typing.ts), which is MIT licensed:
//
//   Copyright (c) 2026 Altan Sarisin
//
//   Permission is hereby granted, free of charge, to any person obtaining a
//   copy of this software and associated documentation files (the
//   "Software"), to deal in the Software without restriction, including
//   without limitation the rights to use, copy, modify, merge, publish,
//   distribute, sublicense, and/or sell copies of the Software, and to permit
//   persons to whom the Software is furnished to do so, subject to the
//   following conditions: The above copyright notice and this permission
//   notice shall be included in all copies or substantial portions of the
//   Software. THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND,
//   EXPRESS OR IMPLIED.
//
// Collie maps no Ctrl chords from the keyboard (its phones have none); this
// adds Ctrl with a letter, and Shift+Tab, for a desktop keyboard. The paste
// framing (pasteChunks) is ssf's own, not collie's.

/// Typed text as keys: each character literal and case-kept, whitespace by
/// name. A line break the person typed is Enter; none is ever added.
export function textToKeys(text) {
  return Array.from(text.replace(/\r\n?/g, "\n"), (char) => {
    if (char === " ") return "Space";
    if (char === "\t") return "Tab";
    if (char === "\n") return "Enter";
    return char;
  });
}

/// Keys a text field does not type, by `KeyboardEvent.key`. A Map, since an
/// object would answer for inherited names such as "constructor".
const SPECIAL_KEYS = new Map([
  ["Escape", "Escape"],
  ["Tab", "Tab"],
  ["ArrowUp", "Up"],
  ["ArrowDown", "Down"],
  ["ArrowLeft", "Left"],
  ["ArrowRight", "Right"],
  ["Backspace", "Backspace"],
  ["Enter", "Enter"],
]);

/// The key a keydown sends, or undefined for one the field types (or the
/// browser keeps): Esc, Tab, the arrows, Backspace and Enter; Shift+Tab; and
/// Ctrl with a letter, except Ctrl+V, the browser's paste.
export function keyForKeyDown(event) {
  if (event.altKey || event.metaKey) return undefined;
  if (event.ctrlKey) {
    return /^[a-z]$/i.test(event.key) && event.key.toLowerCase() !== "v"
      ? `ctrl+${event.key.toLowerCase()}`
      : undefined;
  }
  if (event.key === "Tab" && event.shiftKey) return "shift+Tab";
  return SPECIAL_KEYS.get(event.key);
}

/// A virtual keyboard's Backspace and Enter can arrive as `beforeinput` with
/// no useful keydown.
export function keyForInputType(inputType) {
  if (inputType === "deleteContentBackward") return "Backspace";
  if (inputType === "insertLineBreak" || inputType === "insertParagraph") return "Enter";
  return null;
}

/// A paste, as the text writes that send it (`api/pane/input`'s `text`, herdr
/// `pane send-text`): one bracketed paste, so an agent reads it as a paste
/// rather than as typing, with each line break a CR as a terminal sends it.
/// Other control characters are dropped: an ESC in pasted text could end the
/// bracket early and type the rest as keys. The body of a write is bounded, so
/// the paste goes in pieces of at most `size` characters and a marker each.
export function pasteChunks(text, size) {
  const body = Array.from(
    text.replace(/\r\n?|\n/g, "\r").replace(/[\x00-\x08\x0b\x0c\x0e-\x1f\x7f]/g, ""),
  );
  if (body.length === 0) return [];
  const chunks = [];
  for (let at = 0; at < body.length; at += size) chunks.push(body.slice(at, at + size).join(""));
  chunks[0] = `\x1b[200~${chunks[0]}`;
  chunks[chunks.length - 1] += "\x1b[201~";
  return chunks;
}
