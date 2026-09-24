# Jev decisioning model

> **Historical record.** This is a feasibility study, kept for context. It was
> not built and is not currently planned. Nothing described here reflects how
> ssf behaves now.

Feasibility study for [#467](https://github.com/mikekelly/simple-software-factory/issues/467),
2026-09-23. Code links point to the commit that was studied, `0256a7a`.

**Conclusion:** yes, it's feasible. The best fit is triaging which GitHub activity is worth a full agent turn. Jev would be an optional, fail-open layer, with no key meaning no change in behaviour. Several other decisions should stay deterministic.

## Jev in brief ([docs](https://docs.typesafe.ai/introduction), [models](https://docs.typesafe.ai/models))
| | |
|---|---|
| API | `POST https://api.typesafe.ai/v1/systemone` with a Bearer key. It takes a `state` plus typed questions: `choice` (enum with probabilities), `score`, and `noul` (yes/no probability) |
| Context | **64k tokens** per request, and 32k for the state plus the longest question |
| Price | $0.042 per million input tokens; output is free |
| Rate | 1,200 requests/min. There's no latency SLA; the one cookbook example ran 13 questions over about 54k characters in 0.27s |
| Rust | No SDK. ssf already depends on `reqwest`, so it's plain JSON over HTTPS, and we'd write the 429/529 backoff ourselves |
| Caveats | [jev-1.13 weak spots](https://docs.typesafe.ai/model-jaggedness/jev-1.13): negation, counting, dates, long states, and **no prompt-injection resistance**. Issue text is attacker-controllable, so Jev must never be the security gate |

**Context size is not the binding constraint.** A triage decision needs the new events (about 100–2k tokens) plus the issue title and body (about 500), which fits easily. Only a whole-timeline decision would come near 32k, and we don't need one.

## Where ssf decides today, ranked by value against risk
| # | Candidate | Today | Jev question | Cost of a wrong answer |
|---|---|---|---|---|
| 1 | **Owner wake triage** | [`follow_up`](https://github.com/mikekelly/simple-software-factory/blob/0256a7a8cdd0f75c4634c81dc06cb497fef0a28e/src/engine/implementation/onboarding.rs#L673): every allowed, non-echo event wakes the session for a full turn, and a "+1", a "thanks" or a label change each cost one | `choice`: wake now / batch until the next wake / FYI | Dropping an event is dangerous, so **never drop**. Only defer, and fail open to waking |
| 2 | **Subscriber FYI relevance** | [`fan_out`](https://github.com/mikekelly/simple-software-factory/blob/0256a7a8cdd0f75c4634c81dc06cb497fef0a28e/src/engine/implementation/delivery.rs#L148) with the fixed kind list in [`state_change`](https://github.com/mikekelly/simple-software-factory/blob/0256a7a8cdd0f75c4634c81dc06cb497fef0a28e/src/prompt/timeline.rs#L356). It misses "this is now blocked on X" but wakes on renames | `noul`: is this relevant to *my* task? | Cheap, since it's only an FYI |
| 3 | Dashboard card summary | Cards show a truncated latest message | Jev doesn't generate text, so it can only *classify*, e.g. working / waiting on human / blocked / done | Cheap, display only |
| 4 | Is a mention really addressed to the bot | [`allow::mentions`](https://github.com/mikekelly/simple-software-factory/blob/0256a7a8cdd0f75c4634c81dc06cb497fef0a28e/src/allow.rs#L135) counts quoted and fenced mentions, which keeps items "ours" forever | `noul` | Use it only to narrow retirement, never the onboarding gate |
| 5 | Is the harness screen blocked | Substring checks in [`trust_dialog`](https://github.com/mikekelly/simple-software-factory/blob/0256a7a8cdd0f75c4634c81dc06cb497fef0a28e/src/driver.rs#L128), which then send keystrokes | `choice` | Advisory only; keystrokes still need a deterministic match |

**These stay deterministic:** the allow-list and authorization, own-echo and origin tags, and release and workspace safety.

## Shape if it is picked up
- `[jev]` in the config (enabled, model pinned to `jev-1.13.0`, thresholds). The key lives in `~/.config/ssf/jev-token` (0600), mirroring the GitHub token.
- Candidate 1 only: send events Jev labels *batch* or *FYI* with low confidence **as normal**. High-confidence *batch* events are held and delivered with the next wake, or after a maximum delay such as 30 minutes, so nothing is lost. Log every decision, with its probabilities and model version, so we can measure how many turns it saves and review mistakes.
- A timeout or error means delivering as today.

## Open questions

Neither question has been measured: how fast Jev responds from a factory host,
and how accurate it is on real ssf events. A prototype of candidate 1 should
log every decision, with its probabilities and model version, and measure both
before anything is switched on by default. The maintainer closed #467 after
this study with the idea shelved for now.
