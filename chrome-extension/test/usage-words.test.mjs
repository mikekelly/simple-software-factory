// How a HUD harness row reads provider usage: `node --test chrome-extension/test/`.
import { test } from "node:test";
import assert from "node:assert/strict";
import "../writes-form.js";

const { usageWords } = globalThis.ssfWrites;
const at = () => "16:10";

test("plan windows and balances read as the row's words", () => {
  const claude = {
    harness: "claude",
    accounts: [
      {
        provider: "anthropic",
        state: "ok",
        windows: [
          { label: "5h", used_percent: 42, resets_at: "2026-09-25T16:10:00Z" },
          { label: "week", used_percent: 18.2, resets_at: null },
        ],
        balances: [],
      },
    ],
  };
  assert.deepEqual(usageWords(claude, at), ["5h 42% (resets 16:10)", "week 18%"]);
  const deepseek = { accounts: [{ provider: "deepseek", state: "ok", balances: [{ currency: "USD", amount: "12.40" }] }] };
  assert.deepEqual(usageWords(deepseek, at), ["$12.40"]);
});

test("several accounts are named, and stale or missing numbers say so", () => {
  const omp = {
    accounts: [
      { provider: "chatgpt", state: "stale", note: "refreshes on the harness's next run", windows: [{ label: "week", used_percent: 3 }] },
      { provider: "deepseek", state: "unavailable", note: "the provider answered 500" },
    ],
  };
  assert.deepEqual(usageWords(omp, at), [
    "chatgpt week 3%",
    "stale, refreshes on the harness's next run",
    "deepseek unavailable",
    "the provider answered 500",
  ]);
  assert.deepEqual(usageWords({ accounts: [], note: "no usage data" }), ["no usage data"]);
  assert.deepEqual(usageWords(undefined), []);
  assert.deepEqual(usageWords({ accounts: [{ windows: [{ label: "5h" }] }] }), ["5h unavailable"]);
});
