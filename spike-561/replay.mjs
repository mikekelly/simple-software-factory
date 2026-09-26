import { spawn, execFileSync } from "node:child_process";
import pkg from "@xterm/headless"; const { Terminal } = pkg;
import readline from "node:readline";
const env = Object.fromEntries(Object.entries(process.env).filter(([k]) => !k.startsWith("HERDR_")));
const P = "wG:p1", C = 100, R = 30;
const term = new Terminal({ cols: C, rows: R, scrollback: 0, allowProposedApi: true });
const h = spawn("herdr", ["terminal","session","control",P,"--cols",String(C),"--rows",String(R)], { env });
let frames = 0, sync = 0;
readline.createInterface({ input: h.stdout }).on("line", (l) => {
  const m = JSON.parse(l);
  if (m.type !== "terminal.frame") return;
  frames++; const b = Buffer.from(m.bytes, "base64");
  if (b.includes("\x1b[?2026h") && b.includes("\x1b[?2026l")) sync++;
  term.write(b);
});
const send = (o) => h.stdin.write(JSON.stringify(o) + "\n");
setTimeout(() => send({ type: "terminal.input", text: "for i in $(seq 1 8000); do printf '\\e[3%dm%06d %s\\e[0m\\n' $((i%7)) $i $(head -c 30 /dev/urandom | base64 | tr -d /+); done\r" }), 500);
let last = Date.now(); h.stdout.on("data", () => (last = Date.now()));
const check = setInterval(() => { if (frames > 5 && Date.now() - last > 2500) { clearInterval(check); done(); } }, 200);
function done() {
  term.write("", () => {
    const mine = []; for (let y = 0; y < R; y++) mine.push(term.buffer.active.getLine(y).translateToString(true).trimEnd());
    const theirs = execFileSync("herdr", ["pane","read",P,"--source","visible","--format","text"], { env }).toString().split("\n").map(s=>s.trimEnd()).filter((_, i) => i < R);
    const a = mine.join("\n").trim(), b = theirs.join("\n").trim();
    console.log({ frames, sync, match: a === b });
    if (a !== b) { const am=a.split("\n"), bm=b.split("\n"); console.log("lines",am.length,bm.length,"first diff",am.findIndex((l,i)=>l!==bm[i]), JSON.stringify(am.slice(0,2)), JSON.stringify(bm.slice(0,2))); console.log("MINE\n" + a.slice(-600)); console.log("THEIRS\n" + b.slice(-600)); }
    h.stdin.end(); setTimeout(() => process.exit(0), 500);
  });
}
