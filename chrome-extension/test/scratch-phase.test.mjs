// How the scratch list reads a session: `node --test chrome-extension/test/`.
import { test } from "node:test";
import assert from "node:assert/strict";
import "../writes-form.js";

const { scratchPhase } = globalThis.ssfWrites;

test("the factory's own state is taken as it is", () => {
  for (const state of ["live", "off", "releasing", "released"]) {
    assert.equal(scratchPhase({ state, active: true, agent_live: true }), state);
  }
});

test("a factory that sends no state is read from active and agent_live", () => {
  assert.equal(scratchPhase({ active: true, agent_live: true }), "live");
  assert.equal(scratchPhase({ active: true, agent_live: false }), "off");
  assert.equal(scratchPhase({ active: false, agent_live: false }), "released");
  assert.equal(scratchPhase({ state: "nonsense", active: false }), "released");
  assert.equal(scratchPhase(undefined), "released");
});
