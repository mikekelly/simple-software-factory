// The pane mirror's renderer, below the DOM: `node --test chrome-extension/test/`.
// Expected values are written out by hand from the ANSI/ECMA-48 SGR codes and
// from collie's documented rules, never computed the way the code computes them.
import { test } from "node:test";
import assert from "node:assert/strict";
import { cellPieces, parseAnsi, render, splitLines, tableRuns } from "../pane-render.js";

test("SGR colours and weights become styled runs of plain text", () => {
  const segs = parseAnsi("plain \x1b[1;31mbold red\x1b[0m \x1b[38;5;196mcube\x1b[39m \x1b[48;2;1;2;3mtrue\x1b[m");
  assert.deepEqual(
    segs.map(({ text, fg, bg, bold }) => ({ text, fg, bg, bold: !!bold })),
    [
      { text: "plain ", fg: undefined, bg: undefined, bold: false },
      { text: "bold red", fg: "var(--ansi-1)", bg: undefined, bold: true },
      { text: " ", fg: undefined, bg: undefined, bold: false },
      // 196 = 16 + 36*5: the cube's red corner, level 5 = 55 + 5*40.
      { text: "cube", fg: "rgb(255,0,0)", bg: undefined, bold: false },
      { text: " ", fg: undefined, bg: undefined, bold: false },
      { text: "true", fg: undefined, bg: "rgb(1,2,3)", bold: false },
    ],
  );
});

test("inverse video swaps colours, onto the mirror's own ground when none are named", () => {
  const segs = parseAnsi("\x1b[7mcursor\x1b[27;32;44m swapped\x1b[7m");
  assert.deepEqual(segs.map(({ text, fg, bg }) => ({ text, fg, bg })), [
    { text: "cursor", fg: "#0d1117", bg: "#e6edf3" },
    { text: " swapped", fg: "var(--ansi-2)", bg: "var(--ansi-4)" },
  ]);
});

test("escapes other than SGR leave no text behind, and a CR redraws its line", () => {
  const segs = parseAnsi("a\x1b[?25l\x1b[2Jb\x1b]0;title\x07c\x1b]8;;http://x\x1b\\d\r\n10%\r50%\rdone\r\n");
  assert.equal(segs.map((s) => s.text).join(""), "abcd\ndone\n");
});

/// The text of each line, and whether it is kept to one row.
const shape = (text) =>
  splitLines(parseAnsi(text)).map((line) => [line.segments.map((s) => s.text).join(""), !!line.noWrap]);

test("a style that runs across a line break styles both lines", () => {
  const lines = splitLines(parseAnsi("\x1b[31mone\ntwo\x1b[0m\n\nfour"));
  assert.deepEqual(
    lines.map((line) => line.segments.map(({ text, fg }) => ({ text, fg }))),
    [[{ text: "one", fg: "var(--ansi-1)" }], [{ text: "two", fg: "var(--ansi-1)" }], [], [{ text: "four", fg: undefined }]],
  );
});

test("a box's border rows and rules are kept to one row; prose and tree output wrap", () => {
  const rule = "─".repeat(30);
  assert.deepEqual(shape(`${rule}\n${"─".repeat(10)}\n│ > prompt text      │\n│ tree child\n── Title ${rule}\nplain prose`), [
    [rule, true],
    ["─".repeat(10), false], // too short to wrap anywhere, so nothing to clip
    ["│ > prompt text      │", true], // opens and closes on a frame edge
    ["│ tree child", false], // a leading edge alone is tree output
    [`── Title ${rule}`, true], // a labelled rule
    ["plain prose", false],
  ]);
});

const runs = (text) => tableRuns(splitLines(parseAnsi(text)));

test("a markdown table is a run from its header to its last row, ended by a blank line", () => {
  const text = ["Results:", "| a | b |", "|---|---|", "| 1 | 2 |", "| 3 | 4 |", "", "| not | table |"].join("\n");
  assert.deepEqual(runs(text), [{ start: 1, end: 4 }]);
});

test("a box-drawn table is a run; Claude's single-column input box is not", () => {
  const table = ["┌────┬────┐", "│ a  │ b  │", "├────┼────┤", "│ 1  │ 2  │", "└────┴────┘"];
  const inputBox = ["╭──────────╮", "│ > hello  │", "╰──────────╯"];
  assert.deepEqual(runs([...table, ...inputBox].join("\n")), [{ start: 0, end: 4 }]);
  assert.deepEqual(runs(inputBox.join("\n")), []);
});

test("an ASCII +---+ table from a shell tool is a run", () => {
  const text = ["+----+----+", "| id | nm |", "+----+----+", "| 1  | x  |", "+----+----+", "done"].join("\n");
  assert.deepEqual(runs(text), [{ start: 0, end: 4 }]);
});

test("block and Powerline glyphs are picked out one by one to be painted to the cell", () => {
  assert.equal(cellPieces("no glyphs here │─"), null); // box drawing is a stroke, left to the font
  assert.deepEqual(cellPieces("ab██▄c░"), [
    { text: "ab" },
    { text: "█", cell: "full" },
    { text: "█", cell: "full" },
    { text: "▄", cell: "lower-4" },
    { text: "", cell: "wedge-right" },
    { text: "c░" }, // a shade is a texture, left to the font
  ]);
});

/// Just enough of a document to render into, and to print what was built.
/// It has no HTML parser: setting `innerHTML` throws.
const fakeDocument = {
  createElement(tag) {
    return {
      tag,
      className: "",
      dataset: {},
      style: {},
      childNodes: [],
      append(...nodes) {
        this.childNodes.push(...nodes);
      },
      set innerHTML(_) {
        throw new Error("pane text must never be parsed as HTML");
      },
    };
  },
  createTextNode: (text) => ({ text }),
  createDocumentFragment() {
    return this.createElement("#fragment");
  },
};

/// A rendered node as compact markup, attributes in a fixed order.
function markup(node) {
  if ("text" in node) return node.text;
  const inner = node.childNodes.map(markup).join("");
  if (node.tag === "#fragment") return inner;
  const attrs = [
    node.className && `class="${node.className}"`,
    ...Object.entries(node.dataset).map(([k, v]) => `data-${k}="${v}"`),
    ...Object.entries(node.style).map(([k, v]) => `${k}="${v}"`),
  ].filter(Boolean);
  return `<${node.tag}${attrs.map((a) => ` ${a}`).join("")}>${inner}</${node.tag}>`;
}

test("styled runs are spans with their colours; plain text and pane markup stay text", () => {
  const built = render(fakeDocument, "\x1b[1;31mred\x1b[0m <b>plain</b>\n\x1b[2;3;4;9;44mall\x1b[0m");
  assert.equal(
    markup(built),
    '<span color="var(--ansi-1)" fontWeight="600">red</span> <b>plain</b>\n' +
      '<span backgroundColor="var(--ansi-4)" fontStyle="italic" opacity="0.6" textDecoration="underline line-through">all</span>',
  );
});

test("a border row is clipped, a table pans in its own box, and a block glyph is painted", () => {
  const text = ["│ menu row │", "| a | b |", "|---|---|", "| 1 | 2 |", "bar █"].join("\n");
  assert.equal(
    markup(render(fakeDocument, text)),
    '<span class="clip">│ menu row │</span>\n' +
      '<span class="table-run" data-run="2:| a | b |">| a | b |\n|---|---|\n| 1 | 2 |</span>\n' +
      'bar <span class="cell-glyph" data-cell="full">█</span>',
  );
});

test("a rounded box's top and bottom borders are kept to one row too", () => {
  const lid = `╭${"─".repeat(40)}╮`;
  const floor = `╰${"─".repeat(40)}╯`;
  assert.deepEqual(shape(`${lid}\n│ > hi │\n${floor}\n╭ not a frame`), [
    [lid, true],
    ["│ > hi │", true],
    [floor, true],
    ["╭ not a frame", false],
  ]);
});
