// The scratch terminal's address and wire: `node --test chrome-extension/test/`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { termUrl } from "../factory-url.js";
import { fromBase64, resizeMessage, termSend, toBase64 } from "../term-wire.js";

test("the terminal's socket follows the factory's scheme and escapes the session", () => {
  assert.equal(
    termUrl("http://100.64.0.1:7777/s3cret/", "owner/repo~ab12"),
    "ws://100.64.0.1:7777/s3cret/api/term/owner%2Frepo~ab12",
  );
  assert.equal(
    termUrl("https://factory.example/s3cret/", "o/r~1"),
    "wss://factory.example/s3cret/api/term/o%2Fr~1",
  );
});

test("bytes survive the port as base64", () => {
  const bytes = new Uint8Array([0, 27, 91, 65, 13, 255, 128]);
  assert.equal(toBase64(bytes), "ABtbQQ3/gA==");
  assert.deepEqual(fromBase64("ABtbQQ3/gA=="), bytes);
  const big = new Uint8Array(100000).map((_, i) => i % 256);
  assert.deepEqual(fromBase64(toBase64(big)), big);
});

test("a resize is the factory's JSON, for sizes it takes", () => {
  assert.equal(resizeMessage(120, 40), '{"type":"resize","cols":120,"rows":40}');
  assert.equal(resizeMessage(0, 40), null);
  assert.equal(resizeMessage(80, 1001), null);
  assert.equal(resizeMessage(80.5, 24), null);
});

test("typing and resizing reach the socket only while Writes is on", () => {
  const input = { type: "input", data: "bHMN" };
  const resize = { type: "resize", data: '{"type":"resize","cols":80,"rows":24}' };
  assert.deepEqual(termSend(input, true), new Uint8Array([108, 115, 13]));
  assert.equal(termSend(resize, true), resize.data);
  assert.equal(termSend(input, false), null);
  assert.equal(termSend(resize, false), null);
  assert.equal(termSend({ type: "ping" }, true), null);
});

test("an item terminal's asks need Writes, except a release", () => {
  const ask = (type) => ({ type: "ask", data: JSON.stringify({ type }) });
  assert.equal(termSend(ask("control"), true), '{"type":"control"}');
  assert.equal(termSend(ask("scroll"), true), '{"type":"scroll"}');
  assert.equal(termSend(ask("control"), false), null);
  assert.equal(termSend(ask("scroll"), false), null);
  assert.equal(termSend(ask("release"), false), '{"type":"release"}');
  assert.equal(termSend(ask("takeover"), true), null);
  assert.equal(termSend({ type: "ask", data: "not json" }, true), null);
});
