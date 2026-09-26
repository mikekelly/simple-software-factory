// To be removed with the rest of the pane mirror (`ssf __pane watch|send`)
// once the extension moves item panes to the live terminal (#563).
// The pane mirror's renderer (#477): a pane's ANSI text, drawn as styled text
// in a <pre>, not as a terminal emulator.
//
// Ported to plain JavaScript from collie (https://github.com/AltanS/collie,
// commit 733235df: web/src/lib/ansi.ts, blocks.ts, table-run.ts,
// cell-glyphs.ts, rule-glyphs.ts and components/ansi-output.tsx), which is
// MIT licensed:
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
// Why not a terminal: herdr's `pane read --format ansi` is a rendered grid
// with only SGR colour sequences in it, so there is nothing to emulate. What
// the pane printed at its own width is drawn at the reader's: long lines wrap,
// a row that is a box's border is clipped rather than wrapped, and a table
// pans on its own. Pane text only ever becomes text nodes (`textContent`,
// never HTML), and no pane byte composes a CSS value: colours are built from
// the numbers an SGR sequence carries, and a painted glyph picks one of a
// fixed set of class names.

/// The ground inverse video swaps with when it names no colours of its own:
/// the mirror's own background and foreground (terminal.css).
const MIRROR_BG = "#0d1117";
const MIRROR_FG = "#e6edf3";

/// The 16 indexed colours are CSS variables (terminal.css), so `ESC[31m` and
/// `ESC[38;5;1m` land on the same slot, as they do in a terminal.
const ansiVar = (n) => `var(--ansi-${n})`;

/// One axis of xterm's 6x6x6 colour cube: 0, then 95 up to 255 in steps of 40.
const cubeLevel = (x) => (x === 0 ? 0 : 55 + x * 40);

function color256(n) {
  if (n < 16) return ansiVar(n);
  if (n >= 232) {
    const v = 8 + (n - 232) * 10;
    return `rgb(${v},${v},${v})`;
  }
  const i = n - 16;
  return `rgb(${cubeLevel(Math.floor(i / 36))},${cubeLevel(Math.floor((i % 36) / 6))},${cubeLevel(i % 6)})`;
}

/// A byte of a colour component: an SGR parameter is any number, a colour is not.
const byte = (n) => Math.max(0, Math.min(255, n ?? 0));

function applySgr(state, codes) {
  for (let i = 0; i < codes.length; i++) {
    const c = codes[i];
    if (c === 0) {
      state.fg = state.bg = undefined;
      state.bold = state.dim = state.italic = state.underline = state.strike = state.inverse = false;
    } else if (c === 1) state.bold = true;
    else if (c === 2) state.dim = true;
    else if (c === 3) state.italic = true;
    else if (c === 4) state.underline = true;
    else if (c === 7) state.inverse = true;
    else if (c === 9) state.strike = true;
    else if (c === 22) state.bold = state.dim = false;
    else if (c === 23) state.italic = false;
    else if (c === 24) state.underline = false;
    else if (c === 27) state.inverse = false;
    else if (c === 29) state.strike = false;
    else if (c >= 30 && c <= 37) state.fg = ansiVar(c - 30);
    else if (c === 39) state.fg = undefined;
    else if (c >= 40 && c <= 47) state.bg = ansiVar(c - 40);
    else if (c === 49) state.bg = undefined;
    else if (c >= 90 && c <= 97) state.fg = ansiVar(8 + c - 90);
    else if (c >= 100 && c <= 107) state.bg = ansiVar(8 + c - 100);
    else if (c === 38 || c === 48) {
      const mode = codes[i + 1];
      let col;
      if (mode === 5) {
        col = color256(byte(codes[i + 2]));
        i += 2;
      } else if (mode === 2) {
        col = `rgb(${byte(codes[i + 2])},${byte(codes[i + 3])},${byte(codes[i + 4])})`;
        i += 4;
      } else continue;
      if (c === 38) state.fg = col;
      else state.bg = col;
    }
  }
}

/// ANSI text as styled runs: `{text, fg, bg, bold, dim, italic, underline,
/// strike}`, inverse video already resolved into fg and bg. Only SGR is
/// read; any other CSI, and OSC, is skipped whole. A CR with more text after
/// it on its line overwrites the line (a progress bar's last frame wins); one
/// before LF or at the end is just a line ending.
export function parseAnsi(input) {
  const segs = [];
  const state = {};
  let buf = "";
  let lineStart = 0;

  const flush = () => {
    if (!buf) return;
    const fg = state.inverse ? (state.bg ?? MIRROR_BG) : state.fg;
    const bg = state.inverse ? (state.fg ?? MIRROR_FG) : state.bg;
    segs.push({
      text: buf,
      fg,
      bg,
      bold: state.bold,
      dim: state.dim,
      italic: state.italic,
      underline: state.underline,
      strike: state.strike,
    });
    buf = "";
  };

  for (let i = 0; i < input.length; i++) {
    const ch = input[i];
    if (ch === "\r") {
      const next = input[i + 1];
      if (next === undefined || next === "\n") continue;
      flush();
      segs.splice(lineStart);
      continue;
    }
    if (ch === "\n") {
      buf += "\n";
      flush();
      lineStart = segs.length;
      continue;
    }
    if (ch === "\x1b") {
      const next = input[i + 1];
      if (next === "[") {
        // ESC [ private* params* intermediates* final
        let j = i + 2;
        let isPrivate = false;
        while (j < input.length && /[<=>?]/.test(input[j])) {
          isPrivate = true;
          j++;
        }
        while (j < input.length && /[0-9;:]/.test(input[j])) j++;
        while (j < input.length && input.charCodeAt(j) >= 0x20 && input.charCodeAt(j) <= 0x2f) j++;
        if (input[j] === "m" && !isPrivate) {
          flush();
          const codes = input
            .slice(i + 2, j)
            .split(/[;:]/)
            .filter((s) => s !== "")
            .map((p) => Number.parseInt(p, 10) || 0);
          applySgr(state, codes.length ? codes : [0]);
        }
        i = j;
        continue;
      }
      if (next === "]") {
        // OSC, to BEL or ST.
        let j = i + 2;
        while (j < input.length && input[j] !== "\x07" && !(input[j] === "\x1b" && input[j + 1] === "\\")) j++;
        if (input[j] === "\x1b") j++;
        i = j;
        continue;
      }
      i += 1;
      continue;
    }
    buf += ch;
  }
  flush();
  return segs;
}

// Glyph classes (collie's rule-glyphs.ts), as regular-expression class bodies.
const BOX_DRAWING = "─-╿";
const BLOCK_EIGHTHS = "▁-▔";
const UNICODE_DASHES = "‒-―";
const HORIZONTAL_RULE = "─━┄┅┈┉╌╍═╴╶╸╺╼╾" + BLOCK_EIGHTHS + UNICODE_DASHES;
/// Frame edges. Collie's set, and the rounded corners (`╭╮╰╯`) of the boxes
/// Claude and many TUIs draw, which collie lifts out by grammar instead.
const FRAME_EDGE = "│┌└├┏┗┣╔╚╠╟╞┐┘┤┓┛┫╗╝╣╢╡╭╮╰╯";
/// A cross is a column boundary crossing a row boundary: only a table draws one.
const BOX_CROSS = "┼-╋╪-╬";
/// Every junction that carries a column boundary: the crosses and the T-pieces.
const BOX_COLUMN_JUNCTION = "┬-╋╤-╬";
const BOX_VERTICAL = "│┃┆┇┊┋╎╏║";

/// Twenty identical rule glyphs: nothing in prose or code runs that long, and a
/// shorter rule fits the narrowest mirror anyway.
const PURE_BORDER = new RegExp(`^([${HORIZONTAL_RULE}])\\1{19,}$`);
/// A short rule, one label, then a rule to the row's end (`── Title ─────`).
const LABELLED_RULE = new RegExp(
  `^\\s*([${HORIZONTAL_RULE}])\\1{0,3} +[^${HORIZONTAL_RULE}\\s](?:[^${HORIZONTAL_RULE}]*[^${HORIZONTAL_RULE}\\s])? +([${HORIZONTAL_RULE}])\\2{19,}\\s*$`,
);
/// A framed row: its first and last glyphs are both frame edges. A leading
/// edge alone is `tree` output.
const FRAME_ROW = new RegExp(`^\\s*[${FRAME_EDGE}].*[${FRAME_EDGE}]\\s*$`);

const lineOf = (segments) => {
  const text = segments.map((s) => s.text).join("");
  return PURE_BORDER.test(text.trim()) || LABELLED_RULE.test(text) || FRAME_ROW.test(text)
    ? { segments, noWrap: true }
    : { segments };
};

/// Styled runs as lines, `{segments, noWrap}`, the newlines dropped. A run
/// that spans a line break keeps its style on each side. `noWrap` marks a
/// terminal-wide border -- a rule, or a row that opens and closes on a frame
/// edge -- which wrapping would scramble, so it is clipped to one row instead.
export function splitLines(segments) {
  const lines = [];
  let current = [];
  for (const seg of segments) {
    const t = seg.text;
    if (!t.includes("\n")) {
      current.push(seg);
      continue;
    }
    let start = 0;
    for (;;) {
      const idx = t.indexOf("\n", start);
      const end = idx === -1 ? t.length : idx;
      if (end > start) current.push({ ...seg, text: t.slice(start, end) });
      if (idx === -1) break;
      lines.push(lineOf(current));
      current = [];
      start = idx + 1;
    }
  }
  lines.push(lineOf(current));
  return lines;
}

const lineText = (line) => line.segments.map((s) => s.text).join("");
const isBlank = (text) => text.trim().length === 0;

const MD_DELIMITER = /^\|?\s*:?-+:?\s*(?:\|\s*:?-+:?\s*)+\|?$/;
const PIPE = /\|/g;
const ASCII_DELIMITER = /^\+(?:-+\+)+$/;
const PLUS = /\+/g;
const ASCII_SEPARATOR = /^[|+]$/;
const BOX_FRAME_ROW = new RegExp(`^[${BOX_DRAWING}\\s]+$`);
const BOX_CROSS_RE = new RegExp(`[${BOX_CROSS}]`);
const BOX_JUNCTION = new RegExp(`[${BOX_COLUMN_JUNCTION}]`, "g");
const BOX_SEPARATOR = new RegExp(`^[${BOX_VERTICAL}${BOX_COLUMN_JUNCTION}]$`);

/// Where a row's separators stand, in terminal columns.
function offsetsOf(row, separator) {
  const at = [];
  separator.lastIndex = 0;
  for (let m = separator.exec(row); m; m = separator.exec(row)) at.push(m.index);
  return at;
}

/// A markdown row with the delimiter's number of pipes.
const markdownMember = (pipes) => (row) => {
  const text = row.trimStart();
  return !isBlank(text) && (text.match(PIPE)?.length ?? 0) === pipes;
};

/// A row with a separator at every one of the anchor's columns. A row the
/// pane itself wrapped, a plain rule, or a single-column box below the table
/// has none there, so the run stops short of it.
const offsetMember = (offsets, separator) => (row) =>
  !isBlank(row) && offsets.every((at) => separator.test(row.charAt(at)));

/// The tables among `lines`, as inclusive `{start, end}` line ranges of at
/// least two lines: the one shape wrapping destroys, since a table's meaning
/// is the column a character sits in. Each is anchored on a row nothing else
/// prints -- a markdown delimiter row, a `+---+` rule, or a box row with a
/// cross in it -- and grown over its neighbours that divide into the same
/// columns. A blank line ends a run.
export function tableRuns(lines) {
  const grid = lines.map((line) => lineText(line).trimEnd());
  const runs = [];
  let floor = 0;
  for (let i = 0; i < lines.length; i++) {
    if (i < floor) continue;
    const row = grid[i];
    if (isBlank(row)) continue;
    const text = row.trimStart();
    const member = MD_DELIMITER.test(text)
      ? markdownMember(text.match(PIPE)?.length ?? 0)
      : ASCII_DELIMITER.test(text)
        ? offsetMember(offsetsOf(row, PLUS), ASCII_SEPARATOR)
        : BOX_FRAME_ROW.test(text) && BOX_CROSS_RE.test(text)
          ? offsetMember(offsetsOf(row, BOX_JUNCTION), BOX_SEPARATOR)
          : null;
    if (!member) continue;
    let start = i;
    while (start > floor && member(grid[start - 1])) start--;
    let end = i;
    while (end + 1 < lines.length && member(grid[end + 1])) end++;
    floor = end + 1;
    if (end > start) runs.push({ start, end });
  }
  return runs;
}

/// The characters whose whole job is to fill their cell, and the shape each
/// fills (terminal.css paints one per `data-cell` name). The font cannot do
/// it: a glyph fills its em box, and the mirror's rows are 1.25em, so a block
/// or a Powerline cap would come out a quarter of a row short. Box-drawing
/// strokes, the thin Powerline variants and the shades are left to the font.
const CELL_FILL = {
  "▁": "lower-1",
  "▂": "lower-2",
  "▃": "lower-3",
  "▄": "lower-4",
  "▅": "lower-5",
  "▆": "lower-6",
  "▇": "lower-7",
  "█": "full",
  "▉": "left-7",
  "▊": "left-6",
  "▋": "left-5",
  "▌": "left-4",
  "▍": "left-3",
  "▎": "left-2",
  "▏": "left-1",
  "▀": "upper-4",
  "▐": "right-4",
  "▔": "upper-1",
  "▕": "right-1",
  "▖": "quad-ll",
  "▗": "quad-lr",
  "▘": "quad-ul",
  "▙": "quad-ul-ll-lr",
  "▚": "quad-ul-lr",
  "▛": "quad-ul-ur-ll",
  "▜": "quad-ul-ur-lr",
  "▝": "quad-ur",
  "▞": "quad-ur-ll",
  "▟": "quad-ur-ll-lr",
  "": "wedge-right",
  "": "wedge-left",
  "": "round-right",
  "": "round-left",
};
const PAINTED = /[▀-▐▔-▟]/g;

/// `text` as plain stretches and single painted characters, `{text, cell?}`,
/// or null when it holds none (almost every run). One character per piece: a
/// run of them could wrap across two rows, and its paint would not follow.
export function cellPieces(text) {
  PAINTED.lastIndex = 0;
  let match = PAINTED.exec(text);
  if (match === null) return null;
  const pieces = [];
  let plain = 0;
  while (match !== null) {
    if (match.index > plain) pieces.push({ text: text.slice(plain, match.index) });
    pieces.push({ text: match[0], cell: CELL_FILL[match[0]] });
    plain = PAINTED.lastIndex;
    match = PAINTED.exec(text);
  }
  if (plain < text.length) pieces.push({ text: text.slice(plain) });
  return pieces;
}

/// A run's text, its block and Powerline glyphs each in a painted span.
function cells(doc, text) {
  const pieces = cellPieces(text);
  if (pieces === null) return [doc.createTextNode(text)];
  return pieces.map((piece) => {
    if (!piece.cell) return doc.createTextNode(piece.text);
    const span = doc.createElement("span");
    span.className = "cell-glyph";
    span.dataset.cell = piece.cell;
    span.append(doc.createTextNode(piece.text));
    return span;
  });
}

/// One styled run: its text, in a span only where it carries a style.
function segment(doc, seg) {
  const style = {};
  if (seg.fg) style.color = seg.fg;
  if (seg.bg) style.backgroundColor = seg.bg;
  if (seg.bold) style.fontWeight = "600";
  if (seg.italic) style.fontStyle = "italic";
  if (seg.dim) style.opacity = "0.6";
  const deco = [seg.underline && "underline", seg.strike && "line-through"].filter(Boolean).join(" ");
  if (deco) style.textDecoration = deco;
  const nodes = cells(doc, seg.text);
  if (Object.keys(style).length === 0) return nodes;
  const span = doc.createElement("span");
  Object.assign(span.style, style);
  span.append(...nodes);
  return [span];
}

function lineNodes(doc, line, inRun) {
  const nodes = line.segments.flatMap((seg) => segment(doc, seg));
  // A table's own rows are never clipped: clipping each would hide the
  // columns the table's box exists to pan to.
  if (!line.noWrap || inRun) return nodes;
  const clip = doc.createElement("span");
  clip.className = "clip";
  clip.append(...nodes);
  return [clip];
}

/// `text`, a pane's ANSI output, as nodes for a <pre>: lines separated by
/// newline text nodes, a border row clipped (`clip`), and each table in a box
/// that pans (`table-run`, keyed by `data-run` -- its height and first row --
/// so a caller can keep the box's pan across frames). `doc` makes the nodes.
export function render(doc, text) {
  const lines = splitLines(parseAnsi(text));
  const runs = tableRuns(lines);
  const out = doc.createDocumentFragment();
  let next = 0;
  for (let li = 0; li < lines.length; ) {
    if (li > 0) out.append(doc.createTextNode("\n"));
    const run = runs[next];
    if (!run || run.start !== li) {
      out.append(...lineNodes(doc, lines[li], false));
      li++;
      continue;
    }
    next++;
    const box = doc.createElement("span");
    box.className = "table-run";
    box.dataset.run = `${run.end - run.start}:${lineText(lines[run.start])}`;
    for (let k = run.start; k <= run.end; k++) {
      if (k > run.start) box.append(doc.createTextNode("\n"));
      box.append(...lineNodes(doc, lines[k], true));
    }
    out.append(box);
    li = run.end + 1;
  }
  return out;
}
