# Package-owned application and Omarchy widget

## Decision

Replace the custom marketplace installer with the separation agreed with the
user: the OS package owns SSF binaries, dependencies, service units and shared
resources; explicit SSF setup owns per-user configuration and service
enablement; the Omarchy plugin is a status/control interface. mise is a build
tool only, never an end-user installation dependency.

The earlier marketplace-bootstrap plan records historical work and tests; its
automatic compilation and runtime-management design is superseded by this plan.

## Acceptance

- A local Arch package installs through pacman before publication. Package
  ownership and removal are verifiable with pacman, and packaged code does not
  duplicate itself into a separately managed per-user runtime.
- The unit supports boot and logout persistence after explicit per-user setup
  and linger. Package installation must not silently configure bot accounts,
  start unconfigured factories for every user, or install/enable a shell widget.
- The root Omarchy manifest remains valid. Adding/enabling/removing the widget
  does not compile, install, remove or upgrade SSF, and removing the package
  leaves a clear missing-application state in an installed widget.
- Missing package, missing configuration and service failure have distinct,
  actionable widget messages. Only explicit user controls change service state.
- Retain safe cleanup of previous custom marketplace runtimes through an
  explicit migration/removal path; do not leave an obsolete installer callable
  from the new widget. Preserve projects and do not delete unowned files.
- Package removal stops the owned running service before deleting its binary.
  User configuration/work are preserved unless explicit SSF cleanup requests
  otherwise. Plugin checkout removal remains the plugin manager's job.
- Documentation, setup skill, packaging and CI describe the final split. Build
  and validate a real package, and exercise package-first and widget-first
  ordering, startup persistence, upgrades, removal and reinstall in the VM.
- No installation, migration, service changes, or configuration changes on the
  developer's live desktop. No publication or external messages.

## Delegation and review

Sol agents implement the runtime/package boundary, the widget boundary, and VM
validation/docs in separate file scopes. The coordinator owns scope and
integration decisions. Agree command and status interfaces before coupling
changes. Startup, package and cleanup changes require two consecutive clean
independent reviews, appropriate tests, formatting, lint and an actual package
build. Report VM observations separately from mocked checks and unauthenticated
configuration state separately from actual job execution.

## Evidence

On 2026-09-11, `tests/omarchy-vm-smoke` ran the package/widget lifecycle in a
throwaway overlay of an installed Omarchy 4.0.3 base. The source-only plugin
payload contained no ignored build output. The final v7 artifact (SHA-256
`08107504333e2bc0e3ee0fc26b39f815cf29e13bd16896375aa5cdf9c62bd804`)
was installed and reinstalled through the complete scenario.
Artifacts and logs are under
`/var/tmp/ssf-vm-smoke/omarchy-iso/test-runs/omarchy-4.0.3-integration/runs/20260911-150748-ssf-package-widget`.

Observed in the real guest:

- Widget-first add validated and enabled `ssf.factory`, contained its
  actionable missing-package state, and did not install SSF, create user state,
  enable a unit, or install Cargo, rustc or mise.
- A full guest package upgrade preceded `pacman -U`. Pacman resolved the real
  `herdr` and `github-cli` dependencies, owned `/usr/bin/ssf`, and left every
  declared dependency satisfied without `--nodeps`. Package installation made
  no per-user configuration, state or service change.
- The installed Omarchy terminal launcher ran `/usr/bin/ssf setup`. It invoked
  the prior marketplace helper's own `validate-uninstall` and `uninstall`
  protocol, removed the positively owned legacy runtime/unit/PATH link,
  obtained visible linger authorization, enabled the packaged
  `default.target` unit, wrote `ssf-setup-v1` only on completion, and left bot
  authentication as a distinct required step.
- After reboot, linger and enablement survived and the service attempted to run
  before graphical login, reporting missing authentication separately. The
  package update preserved configuration, state, project work and the widget.
- `ssf uninstall`, reactivation, and `pacman -R` exercised the package's removal
  hook against the opted-in service: the service stopped, enablement and package
  files disappeared, retained data remained, and the installed widget returned
  to its missing-package state. Removing only the widget after package/setup
  reinstall left the service enabled. Adding the widget again did not change
  the binary or service state, and retained data survived the full lifecycle.

This VM had no bot credential, repository, harness login or live job. The
evidence therefore establishes package, migration, setup, service and widget
lifecycle behavior through the unauthenticated state; it does not claim an
agent completed a GitHub job.

Two consecutive independent final reviews found no correctness, safety or
requirements defects. The final validation passed all 377 tests (361 unit, 5
widget and 11 packaging; 4 ignored), the v7 `makepkg` build, and ordinary
Clippy with only the repository's existing baseline warnings.
