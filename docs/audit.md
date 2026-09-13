# Bounded project guidance audit

Use this guide when asked to assess a project's working instructions, not as
an extra gate on every change. Audit the requested scope in one pass, using
its `SSF.md`, `AGENTS.md`, relevant templates/setup guidance and one recent workflow
example. Cite file sections or issue/PR links; read more only to resolve a
specific uncertainty. Do not build a score, audit engine or recurring process.

Ask these qualitative questions:

- **Completion ownership:** Do instructions say what agents may complete,
  close and merge, with the owning issue agent normally responsible rather
  than relying on a separate project-manager issue? When authority ends,
  must they tag/request an appropriate human with a concrete next action?
  Does the workflow follow that rule?
- **KISS and YAGNI:** Do instructions favor the smallest useful outcome and
  defer speculative abstractions, automation and unrelated improvements?
  Is any observed complexity required by the actual acceptance criteria?
- **Proportional validation:** Are checks and independent review matched to
  risk, with lightweight documentation checks and stronger checks for unsafe
  behavior? Are repeated builds/reviews justified by substantive changes?
- **Bounded review:** Is there a stopping rule, a distinction between confirmed
  defects and optional polish, and a path to simplify or ask a maintainer
  when substantive defects remain?

Report only the material findings: evidence, whether each is a confirmed gap,
uncertainty or optional preference, and the smallest warranted action. A missing
slogan alone is not a gap if equivalent guidance exists. Do not infer policy
violations from an unverified suspicion or turn preferences into blockers.

Stop after the report and, if fixes are authorized, one focused check of those
fixes. Do not recursively audit the audit or restart review over wording.
Escalate unresolved material questions to a named human with the decision
needed; leave optional ideas as non-blocking notes rather than creating work
by default. Finish with the outcome and next-action owner: close out within
explicit authority, or link the PR and tag/request the human who must act.
An audit does not grant merge authority or waive unresolved defects.

For this repository, [SSF.md](../SSF.md) defines the SSF session's ownership,
communication, review and completion contract; [AGENTS.md](../AGENTS.md)
defines repository-wide validation and implementation policy. [The
template](../SSF.example.md) and [setup skill](../skills/ssf-setup/SKILL.md)
carry that boundary to new projects. See also [review
boundaries](sessions.md#second-opinions-the-gauntlet).
