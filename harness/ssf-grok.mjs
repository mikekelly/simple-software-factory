/**
 * SSF's out-of-band delivery bridge for Grok: the mailbox contract
 * `ssf-delivery.ts` keeps for Pi and Oh My Pi, as a sidecar process.
 *
 * `ssf-pi-launch` starts the Grok TUI in leader mode on a socket of its own
 * (`SSF_GROK_SOCKET`) and this bridge beside it, told the TUI's pid
 * (`SSF_GROK_PID`).  The bridge attaches to the same leader with
 * `grok agent --leader --leader-socket <socket> stdio` and speaks ACP to it:
 * `initialize`, `session/load` of the conversation the TUI has open (its
 * entry in `$GROK_HOME/active_sessions.json`), then one `session/prompt` per
 * event file the daemon writes.  A prompt sent while the agent is at work is
 * queued by Grok and runs after the current turn; nothing interrupts it
 * (`_x.ai/queue/interject` would cancel the running turn, so it is not
 * used).  Nothing passes through the terminal, so a draft in the composer is
 * left alone.
 *
 * Each prompt's text block carries the file's name in `_meta.ssfDeliveryId`,
 * which Grok keeps in the session's `updates.jsonl`.  A file is acknowledged
 * (renamed to `.ack`) only once that record exists; until then it stays
 * pending, which is what the daemon retries and what a relaunch reconciles
 * (#390): a new bridge acknowledges an id already recorded and submits only
 * what is not.
 *
 * `ready.json` is this process's attestation, written only after the ACP
 * handshake and the session load succeed -- the startup probe.  A Grok whose
 * protocol has changed fails one of them, and the bridge then never attests:
 * the daemon sees no bridge and delivers through the terminal, as it did
 * before.  Verified against grok 1.0.41.
 *
 * The bridge lives as long as the TUI.  When the TUI exits it removes its
 * marker and stops the leader the TUI started, which Grok leaves running
 * (`--no-exit-on-disconnect`), so a relaunch starts a fresh one.
 */
import { spawn, spawnSync } from "node:child_process";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import * as readline from "node:readline";

const mailbox = process.env.SSF_DELIVERY_MAILBOX;
const socket = process.env.SSF_GROK_SOCKET;
const tui = Number(process.env.SSF_GROK_PID);
const grok = process.env.SSF_GROK_COMMAND || "grok";
const home = process.env.GROK_HOME || path.join(os.homedir(), ".grok");
if (!mailbox || !socket || !Number.isInteger(tui) || tui <= 0) process.exit(2);

const ready = path.join(mailbox, "ready.json");
const pinned = path.join(mailbox, "session", "grok-session");
const TICK = 250;
/** How long the TUI gets to open its leader and its conversation. */
const START_TIMEOUT = 120_000;
/** How long one ACP request may take before the protocol counts as changed. */
const REQUEST_TIMEOUT = 30_000;

function log(message) {
	try {
		fs.appendFileSync(
			path.join(mailbox, "session", "grok-bridge.log"),
			`${new Date().toISOString()} ${message}\n`,
		);
	} catch {
		// Logging is best effort.
	}
}

function alive(pid) {
	try {
		process.kill(pid, 0);
		return true;
	} catch (e) {
		return e.code === "EPERM";
	}
}

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/** The conversation the TUI has open, from Grok's own registry. */
function tuiSession() {
	try {
		const entries = JSON.parse(fs.readFileSync(path.join(home, "active_sessions.json"), "utf8"));
		const mine = Array.isArray(entries) ? entries.filter((e) => e && e.pid === tui) : [];
		const last = mine[mine.length - 1];
		return typeof last?.session_id === "string" ? { id: last.session_id, cwd: last.cwd } : undefined;
	} catch {
		return undefined;
	}
}

/** The leader's pid, from the lock beside its socket, when it is running. */
function leaderPid() {
	try {
		const pid = Number(fs.readFileSync(socket.replace(/\.sock$/, ".lock"), "utf8").trim());
		if (!Number.isInteger(pid) || pid <= 0 || !alive(pid)) return undefined;
		const command = spawnSync("ps", ["-o", "command=", "-p", String(pid)], { encoding: "utf8" });
		return /\bagent\s+leader\b/.test(command.stdout ?? "") ? pid : undefined;
	} catch {
		return undefined;
	}
}

/** Whether a live process holds the leader's lock, whatever it is. */
function lockHeld() {
	try {
		const pid = Number(fs.readFileSync(socket.replace(/\.sock$/, ".lock"), "utf8").trim());
		return Number.isInteger(pid) && pid > 0 && alive(pid);
	} catch {
		return false;
	}
}

function ownedByThisProcess() {
	try {
		return JSON.parse(fs.readFileSync(ready, "utf8"))?.pid === process.pid;
	} catch {
		return false;
	}
}

function attest() {
	if (ownedByThisProcess()) return;
	fs.mkdirSync(mailbox, { recursive: true, mode: 0o700 });
	fs.writeFileSync(`${ready}.tmp-${process.pid}`, JSON.stringify({ pid: process.pid }), { mode: 0o600 });
	fs.renameSync(`${ready}.tmp-${process.pid}`, ready);
}

let agent;
/** The poll timer, and whether the ACP connection has gone. */
let ticker;
let lost = false;
let cleaned = false;
function cleanup() {
	if (cleaned) return;
	cleaned = true;
	if (ownedByThisProcess()) {
		try {
			fs.unlinkSync(ready);
		} catch {
			// Already gone.
		}
	}
	try {
		agent?.kill();
	} catch {
		// Already gone.
	}
	// The leader outlives its clients; once the TUI is gone, nothing else
	// uses this one.
	if (!alive(tui)) {
		const pid = leaderPid();
		if (pid) {
			try {
				process.kill(pid, "SIGTERM");
			} catch {
				// Already gone.
			}
		}
		// A leader killed leaves its socket and lock behind.
		if (pid || !lockHeld()) {
			for (const file of [socket, socket.replace(/\.sock$/, ".lock")]) {
				try {
					fs.unlinkSync(file);
				} catch {
					// Already gone.
				}
			}
		}
	}
}
process.on("exit", cleanup);
// A closed pane signals the TUI and the bridge together: give the TUI a
// moment to go, so that its leader is stopped with it.
let stopping = false;
for (const signal of ["SIGTERM", "SIGINT", "SIGHUP"]) {
	process.on(signal, async () => {
		if (stopping) return;
		stopping = true;
		for (let i = 0; i < 50 && alive(tui); i++) await sleep(100);
		process.exit(0);
	});
}

/** Wait for the TUI to go, then clean up: what a bridge that cannot serve does. */
async function outlive() {
	while (alive(tui)) await sleep(1000);
	process.exit(0);
}

// ---- ACP over the leader --------------------------------------------------

let nextId = 0;
const waiting = new Map();

function request(method, params, timeout = REQUEST_TIMEOUT) {
	const id = ++nextId;
	return new Promise((resolve, reject) => {
		const timer =
			timeout > 0
				? setTimeout(() => {
						waiting.delete(id);
						reject(new Error(`${method} timed out`));
					}, timeout)
				: undefined;
		waiting.set(id, (message) => {
			clearTimeout(timer);
			if (message.error) reject(new Error(`${method}: ${JSON.stringify(message.error)}`));
			else resolve(message.result);
		});
		agent.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", id, method, params })}\n`);
	});
}

function connect() {
	agent = spawn(grok, ["agent", "--leader", "--leader-socket", socket, "stdio"], {
		stdio: ["pipe", "pipe", "ignore"],
	});
	agent.on("exit", (code) => {
		log(`grok agent stdio exited (${code})`);
		for (const resolve of waiting.values()) resolve({ error: "the ACP connection closed" });
		waiting.clear();
		// Without the connection nothing can be submitted: stop attesting and
		// leave the pending files to a relaunch, as a gone session does.
		lost = true;
		clearInterval(ticker);
		if (ownedByThisProcess()) {
			try {
				fs.unlinkSync(ready);
			} catch {
				// Already gone.
			}
		}
		outlive();
	});
	agent.stdin.on("error", () => {});
	readline.createInterface({ input: agent.stdout }).on("line", (line) => {
		let message;
		try {
			message = JSON.parse(line);
		} catch {
			return;
		}
		// Only responses matter: a load replays the conversation as
		// notifications, and a queued prompt answers when its turn ends.
		if (message.id !== undefined && !message.method && waiting.has(message.id)) {
			const resolve = waiting.get(message.id);
			waiting.delete(message.id);
			resolve(message);
		}
	});
}

// ---- the session's record ---------------------------------------------------

/** Delivery ids each session's `updates.jsonl` records, read incrementally. */
const records = new Map();

function sessionFile(id) {
	try {
		for (const group of fs.readdirSync(path.join(home, "sessions"))) {
			const file = path.join(home, "sessions", group, id, "updates.jsonl");
			if (fs.existsSync(file)) return file;
		}
	} catch {
		// No sessions yet.
	}
	return undefined;
}

function recorded(id) {
	let entry = records.get(id);
	if (!entry) {
		entry = { file: undefined, offset: 0, rest: "", ids: new Set() };
		records.set(id, entry);
	}
	entry.file ??= sessionFile(id);
	if (!entry.file) return entry.ids;
	try {
		const size = fs.statSync(entry.file).size;
		if (size < entry.offset) Object.assign(entry, { offset: 0, rest: "" });
		if (size > entry.offset) {
			const fd = fs.openSync(entry.file, "r");
			const buffer = Buffer.alloc(size - entry.offset);
			fs.readSync(fd, buffer, 0, buffer.length, entry.offset);
			fs.closeSync(fd);
			entry.offset = size;
			const text = entry.rest + buffer.toString("utf8");
			const lines = text.split("\n");
			entry.rest = lines.pop() ?? "";
			for (const line of lines) {
				if (!line.includes("ssfDeliveryId")) continue;
				for (const match of line.matchAll(/"ssfDeliveryId":"([^"]+)"/g)) entry.ids.add(match[1]);
			}
		}
	} catch {
		// Read again next tick.
	}
	return entry.ids;
}

function eventText(body) {
	const parsed = JSON.parse(body);
	return typeof parsed?.text === "string" && parsed.text.length > 0 ? parsed.text : undefined;
}

// ---- main -----------------------------------------------------------------

async function main() {
	fs.mkdirSync(path.join(mailbox, "session"), { recursive: true, mode: 0o700 });
	// The TUI's leader and conversation, once it has opened them.
	const deadline = Date.now() + START_TIMEOUT;
	let open;
	while (!(open = tuiSession()) || !fs.existsSync(socket) || !leaderPid()) {
		if (!alive(tui)) process.exit(0);
		if (Date.now() > deadline) {
			// A TUI that refused leader mode (a sandbox profile) never opens the
			// socket; its events go through the terminal.
			log("the TUI opened no leader session; not attesting (terminal delivery)");
			return outlive();
		}
		await sleep(TICK);
	}
	connect();
	let session;
	try {
		const init = await request("initialize", { protocolVersion: 1, clientCapabilities: {} });
		if (init?.protocolVersion !== 1 || !init?.agentCapabilities?.loadSession) {
			throw new Error(`unexpected initialize result: ${JSON.stringify(init).slice(0, 300)}`);
		}
		await request("session/load", { sessionId: open.id, cwd: open.cwd, mcpServers: [] });
		session = open.id;
	} catch (e) {
		// The startup probe: a Grok whose protocol changed is not attested.
		log(`ACP probe failed, not attesting (terminal delivery): ${e.message}`);
		agent.removeAllListeners("exit");
		agent.kill();
		return outlive();
	}
	log(`serving ${session} for pid ${tui}`);
	pin(session);

	// Files submitted and not recorded yet, each with the session it went to:
	// never submitted again, since that conversation may still record it.
	const handed = new Map();
	let polling = false;
	const poll = async () => {
		if (polling || lost) return;
		polling = true;
		try {
			if (!alive(tui)) process.exit(0);
			attest();
			// A `/new` or a resume in the TUI moves the conversation events go to.
			const now = tuiSession();
			if (now && now.id !== session) {
				await request("session/load", { sessionId: now.id, cwd: now.cwd, mcpServers: [] });
				session = now.id;
				pin(session);
				log(`now serving ${session}`);
			}
			const pending = fs
				.readdirSync(mailbox)
				.filter((name) => name.endsWith(".json") && name !== "ready.json")
				.sort();
			for (const name of pending) {
				const file = path.join(mailbox, name);
				const owner = handed.get(name) ?? session;
				if (recorded(owner).has(name)) {
					fs.renameSync(file, `${file}.ack`);
					handed.delete(name);
					continue;
				}
				if (handed.has(name)) continue;
				const text = eventText(fs.readFileSync(file, "utf8"));
				if (text === undefined) continue;
				handed.set(name, session);
				// Answered when the prompt's turn ends; the record is what counts.
				request(
					"session/prompt",
					{ sessionId: session, prompt: [{ type: "text", text, _meta: { ssfDeliveryId: name } }] },
					0,
				).catch((e) => {
					log(`prompt ${name}: ${e.message}`);
					// Refused outright: submit it again unless it was recorded.
					if (!recorded(handed.get(name) ?? session).has(name)) handed.delete(name);
				});
			}
		} catch (e) {
			// A partial write or a transient error: the file stays pending.
			log(`poll: ${e.message}`);
		} finally {
			polling = false;
		}
	};
	ticker = setInterval(poll, TICK);
	await poll();
}

function pin(id) {
	try {
		fs.writeFileSync(`${pinned}.tmp`, `${id}\n`, { mode: 0o600 });
		fs.renameSync(`${pinned}.tmp`, pinned);
	} catch {
		// Only a relaunch without `--resume` loses it.
	}
}

main().catch((e) => {
	log(`bridge failed: ${e?.stack ?? e}`);
	outlive();
});
