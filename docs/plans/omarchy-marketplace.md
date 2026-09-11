# Omarchy marketplace installation

## Intended outcome

The repository is ready to submit to the Omarchy plugin marketplace. A user
installs it with `omarchy plugin add <repository-url> --enable`, completes a
visible first-run installation, and gets both the Factory widget and an SSF
background service. Once configured, the service starts at boot and survives
logout. Installing an Arch package separately is not the primary Omarchy path.

## Constraints

- Omarchy's plugin installer clones, validates, discovers and enables plugins;
  it does not execute install/remove hooks or install system dependencies.
- The repository root needs a valid plugin manifest and no forbidden symlinks.
- First-run setup must work without an existing SSF executable and must show
  build progress, failures and any system authentication requirement.
- A graphical-session service alone does not satisfy boot/logout persistence.
- Existing package installs, project data and marketplace checkouts must not
  be overwritten or removed accidentally.
- Plugin removal and persistent daemon removal have distinct lifecycles;
  expose and document the supported complete removal procedure.
- Preserve the other Linux and macOS installation routes.
- Do not install into the developer's live desktop, change its services, use
  its bot credentials, publish releases, or submit a marketplace listing as
  part of implementation verification.

## Work allocation

Root owns acceptance criteria, interface decisions, integration and review.
Implementation is delegated to three Sol subagents:

1. Runtime: bootstrap, service ownership, boot/logout persistence, update and
   removal, and necessary Rust integration.
2. Marketplace: root manifest/resources, first-run widget flow, package
   compatibility and validation wiring.
3. Validation: isolated tests, disposable-VM smoke-test entry point, setup
   documentation and setup skill.

The runtime and marketplace agents agree the initial status/setup interface
before coupling their implementations. Agents coordinate shared files; root
resolves architectural questions.

## Acceptance checks

1. A fresh clone passes the real Omarchy plugin validator at repository root.
2. The marketplace install command discovers and enables the correct widget.
3. First-run setup works without SSF, and failures leave a visible retry path.
4. The installed executable, resources and service correspond to the intended
   source version; an old unrelated release is not silently substituted.
5. Exactly one service is registered, and successful setup confirms both boot
   startup and logout persistence. Missing credentials are distinguished from
   a broken installation.
6. Repeated setup and upgrades preserve user configuration and project data.
7. Complete removal unregisters the owned runtime/service and preserves work;
   reinstall succeeds. Plugin-only removal behaviour is explicit.
8. The disposable-VM test exercises the actual marketplace entry point and
   records service, reboot/logout, widget and removal evidence.
9. Required unit/integration checks, formatting, lint and package build pass.
10. Report separately what was executed, what is automated but requires VM
    prerequisites, and what remains before public marketplace submission.

## Review

Installation, startup and removal changes receive deep review by fresh Sol
reviewers. Resolve substantive findings, then require two consecutive clean
rounds. Reviewers assess the final diff against this plan and the tests, with
particular attention to data preservation, failure recovery and ownership.

## References

- <https://plugins.omarchy.org/develop.html>
- <https://plugins.omarchy.org/publish.html>
- <https://github.com/omacom/omarchy/blob/quattro/shell/README.md>
- <https://github.com/omacom/omarchy-iso>

## Verification record

Implementation and review were performed by Sol subagents, coordinated by
the root agent. Review rounds 1–3 found and corrected bootstrap state,
upgrade/restart, checkout preservation, disabled-marker, custom-XDG,
runtime ownership and credential-classification failures. Fresh rounds 4
and 5 independently found no remaining material defects.

Later review and live-VM rounds exposed concurrent setup, stale recovery-command
and systemd condition-path parsing failures. The fixes now have isolated race,
installed-helper recovery and parser-level service-unit regressions. The unit
test uses a path containing spaces, `%e`, a backslash and a quote; it verifies
the exact emitted condition with systemd's parser and proves that the disabled
marker changes the evaluated condition. The unusual path also guards the
`/usr/bin/env` executable indirection required by systemd.

Final lifecycle review found two further retry hazards. An earlier generic
cleanup failure could remove the marketplace executable before reporting the
failure, and a completed visible setup retained its lifecycle lock while its
terminal waited to be closed. Marketplace self-removal is now gated on all
earlier cleanup succeeding, with an integration regression that forces a real
token-removal failure, proves the executable and helper remain, and completes
on retry. Visible setup releases its installer-owned lock as soon as the
installation mutation returns, before displaying its final acknowledgement.
A pseudo-terminal regression proves the lock is held during the build, can be
acquired independently while the success prompt remains open, and an early
failure does not unlock an unrelated inherited descriptor.
Two fresh focused reviewers independently found the resulting final lifecycle
and prompt-lock changes clean.

The final uninstall review found that the Rust command reached the helper's
ownership checks only after ordinary teardown. Marketplace uninstall now takes
the shared lifecycle lock in Rust and validates the runtime, unit and command
link before reporting, purging or changing any service, credentials, VM or
data. A process-level regression pauses inside teardown and proves an
independent lock contender is excluded until removal finishes; it also proves
an unowned unit is rejected without service or data changes even with a stale
lock environment variable. The same live-VM work exposed an unusable `cargo`
mise shim on repeat setup. Bootstrap now probes `cargo --version` and falls
back to `mise exec rust@stable` when the bare command is unusable, with an
idempotent-install regression for that case.

The integrated source passes 365 executed tests (four existing live tests
remain ignored), formatting, shell syntax, the upstream Omarchy plugin
validator and clippy. Clippy reports existing warnings. An isolated Arch
package build passes its checks; the packaged manifest, panel, bootstrap
helper and unit template are checked against the final source byte for byte.
The resulting local package artifact is
`packaging/ssf-0.1.0.r424.gfc82870-1-x86_64.pkg.tar.zst`.

The real disposable-VM smoke test ran against the official Omarchy 4.0.3 ISO
(`omarchy-4.0.3.iso`, 6,260,654,080 bytes). Its SHA-256 was verified as
`03d60bc74306dca51f96e1a84b690871d8d606826b260edd0208962da8507d14`.
QEMU 11.1.0 and OVMF were extracted into `/var/tmp/ssf-vm-smoke`; no package,
plugin, runtime or service was installed on the developer's live desktop.

The upstream `omarchy-iso` integration runner installed the ISO unattended
to a reusable base and booted disposable qcow2 overlays with KVM. In the
fresh default 8 GiB guest, `mise`, `cc`, `gcc` and `clang` were present while
`cargo` and `rustc` were initially absent. The actual command
`omarchy plugin add file:///tmp/ssf-marketplace-source --enable --yes` cloned,
validated, discovered and enabled `ssf.factory`. The widget opened its visible
setup terminal, `mise` downloaded Rust, and Cargo built SSF with the default
job count. The guest password was supplied only to the visible linger prompt.

The live run verified the installed helper and binary against their source,
idempotent setup after the mise-shim fix, exactly one enabled user unit, and
the expected `no bot account signed in` state without credentials. The unit's
disabled-marker condition evaluated yes, then no with the marker present, and
yes again after removal. After a real reboot and before graphical login, the
lingering user manager started the enabled unit and the journal recorded the
same expected missing-auth result. Uninstall removed the owned service while
preserving project work, configuration and state. Real plugin removal, re-add,
enable and visible reinstall also completed.

A final resumed-overlay proof copied only tracked and intentional untracked
source, rebuilt incrementally, and compared the installed helper and binary
byte for byte with the final checkout/build. It left the visible success
acknowledgement open and proved a concurrent uninstall completed within 30
seconds, then reconfirmed service removal, data preservation and plugin
removal. Artifacts are retained under
`/var/tmp/ssf-vm-smoke/omarchy-iso/test-runs/omarchy-4.0.3-integration/runs/`,
principally `20260910-193249-ssf-marketplace` and
`20260910-195859-ssf-final-proof`; they include serial logs, service journals,
condition results, command logs and screenshots.

No bot credentials entered the guest, so this verification does not claim an
authenticated GitHub job execution. It verifies installation, the explicit
needs-configuration state, boot/logout service ownership, removal and
reinstallation.

## Host failure follow-up — 2026-09-11

The user installed the local snapshot through `omarchy plugin add` and received
success, but verification found an enabled plugin with no SSF executable or
service unit. The earlier VM results therefore do not establish reliable
automatic setup on this desktop. Investigate the host read-only and reproduce
its relevant state in a disposable VM before changing the product.

Acceptance for this follow-up:

- Record the actual failure cause, distinguishing plugin discovery from QML
  loading, terminal launch, compilation and service registration.
- Exercise the real `plugin add --enable` path with automatic visible setup;
  manually invoking the helper must not substitute for first-run success.
- Cover the reproduced conditions and uninstall/re-add without leaving stale
  launch state that suppresses installation.
- Failures must be visible and actionable rather than silently reported as
  successful setup. Keep authorization prompts in the visible terminal.
- Verify executable installation, enabled service and linger from the final
  source; report missing authentication separately from installation failure.
- Preserve existing ownership, locking, retry and data-preservation guarantees.
- Keep the developer's installed plugin, service and configuration unchanged
  during diagnosis and VM validation.

Read-only host evidence shows that the installed manifest and helper match the
snapshot, while no runtime, user unit or setup throttle stamp exists and linger
is enabled. After a full plugin reload, the running shell continued invoking
the legacy panel's unconditional status command every 30 seconds. This is
consistent with the shell retaining the old `Panel.qml` component at the same
URL, but remains a host-side inference rather than a confirmed Qt cache cause.

The repair gives the widget a new stable `marketplace/FactoryPanel.qml`
entry-point URL and containing directory,
launches automatic setup through a directly observable QML process, reports an
early terminal-launch failure, and writes the throttle stamp only after the
launcher accepts the request. Its presentation-launcher argument is one
shell-quoted command string, including for checkout paths containing spaces and
shell metacharacters. Source tests, the real plugin validator, QML parsing and
an isolated Arch package build pass; the package's manifest, panel, helper and
unit template match source byte for byte. Disposable-VM confirmation of the
automatic path then reproduced the host symptom: the legacy root `Panel.qml`
remained active after plugin replacement, and a root `FactoryPanel.qml` load
failed with Qt's `File name case mismatch` despite exact on-disk casing and
byte matches. Moving the new entry point into the previously unseen
`marketplace/` directory avoided that stale directory/type cache. In the same
shell, with no restart, click or manual helper invocation, `plugin add
--enable` loaded it, created the launch stamp and opened the visible installer.

The lifecycle replay found a separate re-add race. `ssf uninstall` was invoked
without `OMARCHY_PATH`; widget disable failed but was only warned about, so the
still-enabled old widget launched setup after the stamp was cleared. Plugin
removal then deleted that helper's script and working directory, producing a
visible Rust working-directory failure and a stale throttle stamp. Omarchy
plugin commands now receive the supported `/usr/share/omarchy` fallback when
the caller has no override, and a disable failure aborts cleanup before the
runtime or stamp is removed.

The corrected disposable-VM run
`20260911-114632-ssf-marketplace` exited successfully. It reproduced the legacy
failure, automatically installed the final nested entry point, built the exact
source, enabled one user unit and linger, and verified boot startup and the
expected missing-authentication state. Public uninstall with `OMARCHY_PATH`
omitted disabled the widget before removing the owned runtime and unit while
preserving work, configuration and state. Plugin removal and same-URL re-add
then automatically rebuilt the runtime and restored the enabled unit with
linger. No credentials, manual helper call, widget click or shell restart were
used; the developer's installed plugin and runtime were not changed.
Only the disposable guest password was supplied to the visible linger prompt.
Escape dismissed the guest screensaver after re-add setup had already finished
so its result could be inspected. No bot credential entered the guest, so this
run proves the distinct missing-authentication state rather than authenticated
GitHub execution. Logs and five screenshots are under
`/var/tmp/ssf-vm-smoke/omarchy-iso/test-runs/omarchy-4.0.3-integration/runs/20260911-114632-ssf-marketplace`;
the root-entry-point diagnostic is `20260911-111249` and the uninstall-order
diagnostic is `20260911-113448`.
