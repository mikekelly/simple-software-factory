//! One descriptor per coding-agent harness ssf launches: its names, how it
//! is installed and started unattended, the model and effort settings it
//! takes, how it takes a context-compaction threshold, the words its sign-in
//! screen shows, the API-key variables that stand in for a sign-in, and
//! whether ssf reads its local transcript, and the channel an event reaches it
//! through. Adding a harness is one row in [`HARNESSES`] (#446).
//!
//! Every harness ssf launches has a row; a capability a harness lacks is an
//! empty or `None` field, not a missing row. An id with no row is one ssf does
//! not know. What stays elsewhere, because it is logic rather than data: the
//! transcript readers (`sessions`), the sign-in probes (`login`), the VM login
//! flows (`vm::LOGINS`) and the delivery channels' implementations, which a
//! row names.

use crate::claude_delivery::Claude;
use crate::codex_delivery::Codex;
use crate::delivery_channel::Mailbox;
use crate::herdr::{Channel, Terminal};
use crate::models::{self, Catalogue, Compaction};

pub struct Harness {
    /// Id used by herdr, Omarchy and `repo.harness` (`claude`, `codex`, ...).
    pub id: &'static str,
    /// The name people see in ssf's messages (`login::display_name`).
    pub display_name: &'static str,
    /// The name Omarchy's agent catalogue (`omarchy-default-agent`) gives it,
    /// which `ssf agents` lists.
    pub omarchy_name: &'static str,
    /// The mise package that installs it.
    pub mise_package: &'static str,
    /// Executable expected on PATH.
    pub command: &'static str,
    /// Flags that make it run without stopping for approval: every tool call
    /// is allowed and the first-run trust question, where a flag can answer
    /// it, is answered. ssf's terminals are unmanned, so nothing could answer
    /// a prompt; Claude Code's `AskUserQuestion` tool is dropped for the same
    /// reason. Login and first-run onboarding survive all of these and are
    /// machine setup.
    pub unattended_flags: Option<&'static str>,
    /// How it takes a model and an effort level; `None` when it takes no
    /// model setting.
    pub catalogue: Option<Catalogue>,
    /// How it takes a context-compaction threshold; `None` when ssf knows no
    /// way, and the configured threshold is left out of its launch.
    pub auto_compaction: Option<Compaction>,
    /// The phrases (lowercase) it shows at its sign-in prompt.
    pub login_phrases: &'static [&'static str],
    /// Environment variables that stand in for a sign-in.
    pub api_key_vars: &'static [&'static str],
    /// Whether ssf has a reader for its local transcript, which is what both
    /// dates a conversation (`sessions::last_activity`) and resumes one
    /// (`sessions::resume_command`).
    pub reads_transcript: bool,
    /// How full the running session's context is (`12% of 1M`), read from
    /// inside the session for its byline; `None` when ssf cannot tell.
    pub context: Option<fn() -> Option<String>>,
    /// How an event reaches it while it runs: its own channel, or the
    /// terminal.
    pub channel: &'static dyn Channel,
}

/// The descriptor for `id`, when ssf knows the harness.
pub fn harness(id: &str) -> Option<&'static Harness> {
    HARNESSES.iter().find(|h| h.id == id)
}

/// The delivery channel for `id`: the terminal for a harness ssf does not
/// know, which is what reaches any harness.
pub fn channel(id: &str) -> &'static dyn Channel {
    harness(id).map_or(&Terminal, |h| h.channel)
}

/// Keys that stand in for a sign-in with a harness reaching many providers.
const PROVIDER_KEYS: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
    "OPENROUTER_API_KEY",
    "GEMINI_API_KEY",
    "XAI_API_KEY",
];

/// What Pi and Oh My Pi show when they have no provider signed in.
const PI_LOGIN_PHRASES: &[&str] = &[
    "use /login to log into a provider",
    "no models available",
    "select provider to login",
    "set up your providers",
];

/// Every harness ssf knows, in the order `ssf agents` lists them.
pub static HARNESSES: &[Harness] = &[
    Harness {
        id: "claude",
        display_name: "Claude Code",
        omarchy_name: "Claude Code",
        mise_package: "claude",
        command: "claude",
        unattended_flags: Some(
            "--dangerously-skip-permissions --disallowedTools AskUserQuestion --settings '{\"crossSessionInbound\":\"accept\"}'",
        ),
        catalogue: Some(Catalogue {
            model_flag: "--model",
            models: &["fable", "opus", "sonnet", "haiku"],
            effort_levels: &["low", "medium", "high", "xhigh", "max"],
            effort_args: models::claude_effort,
            catalogue: Some(models::claude_catalogue),
            list_models: None,
            refresh: Some(models::claude_refresh),
        }),
        auto_compaction: Some(Compaction::Args {
            args: models::claude_compaction,
            tokens: Some(100_000..=1_000_000),
        }),
        login_phrases: &[
            "login expired",
            "run /login",
            "select login method",
            "oauth token expired",
            "oauth token revoked",
            "run claude auth login",
            "invalid api key",
        ],
        api_key_vars: &["ANTHROPIC_API_KEY"],
        reads_transcript: true,
        context: Some(crate::sessions::claude_context),
        channel: &Claude,
    },
    Harness {
        id: "codex",
        display_name: "Codex",
        omarchy_name: "Codex",
        mise_package: "codex",
        command: "codex",
        unattended_flags: Some(
            "--dangerously-bypass-approvals-and-sandbox --dangerously-bypass-hook-trust",
        ),
        catalogue: Some(Catalogue {
            model_flag: "-m",
            models: &[
                "gpt-5.6-sol",
                "gpt-5.6-terra",
                "gpt-5.6-luna",
                "gpt-5.5",
                "gpt-5.2-codex",
            ],
            effort_levels: &["minimal", "low", "medium", "high", "xhigh", "max", "ultra"],
            effort_args: models::codex_effort,
            catalogue: Some(models::codex_catalogue),
            list_models: None,
            refresh: None,
        }),
        auto_compaction: Some(Compaction::Args {
            args: models::codex_compaction,
            tokens: None,
        }),
        login_phrases: &[
            "sign in with chatgpt",
            "re-run codex login",
            "run codex login",
            "provide your own api key",
        ],
        api_key_vars: &["OPENAI_API_KEY"],
        reads_transcript: true,
        context: Some(crate::sessions::codex_context),
        channel: &Codex,
    },
    // Pi, Oh My Pi and OpenCode use their own `provider/model` identifiers.
    Harness {
        id: "omp",
        display_name: "Oh My Pi",
        omarchy_name: "Oh My Pi",
        mise_package: "github:can1357/oh-my-pi",
        command: "omp",
        unattended_flags: Some("--auto-approve"),
        catalogue: Some(Catalogue {
            model_flag: "--model",
            models: &[],
            effort_levels: &[
                "off", "minimal", "low", "medium", "high", "xhigh", "max", "auto",
            ],
            effort_args: models::thinking,
            catalogue: None,
            list_models: Some(("omp models --json", models::omp_models)),
            refresh: None,
        }),
        auto_compaction: Some(Compaction::Overlay),
        login_phrases: PI_LOGIN_PHRASES,
        api_key_vars: PROVIDER_KEYS,
        reads_transcript: false,
        context: Some(crate::sessions::omp_context),
        channel: &Mailbox,
    },
    Harness {
        id: "pi",
        display_name: "Pi",
        omarchy_name: "Pi",
        mise_package: "pi",
        command: "pi",
        // Pi has no tool approvals; `--approve` trusts the project's `.pi/` files.
        unattended_flags: Some("--approve"),
        catalogue: Some(Catalogue {
            model_flag: "--model",
            models: &[],
            effort_levels: models::THINKING_LEVELS,
            effort_args: models::thinking,
            catalogue: None,
            list_models: Some(("pi --list-models", models::pi_models)),
            refresh: None,
        }),
        // Pi compacts at `reserveTokens` below the window, set only in
        // `~/.pi/agent/settings.json` or the project's `.pi/settings.json`:
        // no flag, variable or overlay file reaches one session alone.
        auto_compaction: None,
        login_phrases: PI_LOGIN_PHRASES,
        api_key_vars: PROVIDER_KEYS,
        reads_transcript: false,
        context: Some(crate::sessions::pi_context),
        channel: &Mailbox,
    },
    Harness {
        id: "opencode",
        display_name: "OpenCode",
        omarchy_name: "OpenCode",
        mise_package: "opencode",
        command: "opencode",
        unattended_flags: Some("--auto"),
        catalogue: Some(Catalogue {
            model_flag: "-m",
            models: &[],
            effort_levels: &[],
            effort_args: models::no_effort,
            catalogue: None,
            list_models: Some(("opencode models", models::opencode_models)),
            refresh: None,
        }),
        auto_compaction: None,
        login_phrases: &["run /connect to add an ai provider"],
        api_key_vars: PROVIDER_KEYS,
        reads_transcript: false,
        context: None,
        channel: &Mailbox,
    },
    Harness {
        id: "gemini",
        display_name: "Gemini CLI",
        omarchy_name: "Gemini",
        mise_package: "gemini",
        command: "gemini",
        unattended_flags: Some("--yolo --skip-trust"),
        catalogue: Some(Catalogue {
            model_flag: "-m",
            models: &[
                "gemini-3-pro-preview",
                "gemini-3-flash-preview",
                "gemini-2.5-pro",
                "gemini-2.5-flash",
            ],
            effort_levels: &[],
            effort_args: models::no_effort,
            catalogue: None,
            list_models: None,
            refresh: None,
        }),
        auto_compaction: None,
        login_phrases: &[
            "how would you like to authenticate",
            "no authentication method selected",
            "sign in with google",
        ],
        api_key_vars: &["GEMINI_API_KEY", "GOOGLE_API_KEY"],
        reads_transcript: false,
        context: None,
        channel: &Terminal,
    },
    Harness {
        id: "copilot",
        display_name: "GitHub Copilot",
        omarchy_name: "GitHub Copilot",
        mise_package: "copilot",
        command: "copilot",
        unattended_flags: Some("--allow-all"),
        catalogue: Some(Catalogue {
            model_flag: "--model",
            models: &["auto"],
            effort_levels: &["none", "minimal", "low", "medium", "high", "xhigh", "max"],
            effort_args: models::claude_effort,
            catalogue: None,
            list_models: None,
            refresh: None,
        }),
        auto_compaction: None,
        login_phrases: &["run /login"],
        api_key_vars: &["COPILOT_GITHUB_TOKEN", "GH_TOKEN", "GITHUB_TOKEN"],
        reads_transcript: false,
        context: None,
        channel: &Terminal,
    },
    Harness {
        id: "grok",
        display_name: "Grok",
        omarchy_name: "Grok",
        mise_package: "npm:@xai-official/grok",
        command: "grok",
        unattended_flags: Some("--always-approve"),
        catalogue: Some(Catalogue {
            model_flag: "-m",
            models: &["grok-4.6", "grok-4.5"],
            effort_levels: &["low", "medium", "high", "xhigh"],
            effort_args: models::grok_effort,
            catalogue: None,
            list_models: Some(("grok models", models::grok_models)),
            refresh: None,
        }),
        auto_compaction: None,
        login_phrases: &[
            "approve in your browser to finish signing in",
            "waiting for approval",
        ],
        api_key_vars: &["XAI_API_KEY"],
        reads_transcript: true,
        context: None,
        channel: &Terminal,
    },
    Harness {
        id: "crush",
        display_name: "Crush",
        omarchy_name: "Crush",
        mise_package: "crush",
        command: "crush",
        unattended_flags: Some("--yolo"),
        catalogue: None,
        auto_compaction: None,
        login_phrases: &["let's choose a provider and model"],
        api_key_vars: PROVIDER_KEYS,
        reads_transcript: false,
        context: None,
        channel: &Terminal,
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(pick: impl Fn(&Harness) -> bool) -> Vec<&'static str> {
        HARNESSES.iter().filter(|h| pick(h)).map(|h| h.id).collect()
    }

    #[test]
    fn every_harness_has_one_entry() {
        let mut seen = std::collections::BTreeSet::new();
        for h in HARNESSES {
            assert!(seen.insert(h.id), "{} has two entries", h.id);
            assert!(std::ptr::eq(harness(h.id).unwrap(), h));
        }
        assert!(harness("nope").is_none());
        assert!(harness("").is_none());
    }

    /// The membership the separate tables had before they were one: the
    /// agents and sign-in lists named all nine, the model table all but
    /// crush, compaction three and transcripts two; grok has since gained a
    /// transcript reader.
    #[test]
    fn membership_matches_the_tables_it_replaced() {
        let all = [
            "claude", "codex", "omp", "pi", "opencode", "gemini", "copilot", "grok", "crush",
        ];
        assert_eq!(ids(|_| true), all);
        assert_eq!(ids(|h| h.unattended_flags.is_some()), all);
        assert_eq!(ids(|h| !h.login_phrases.is_empty()), all);
        assert_eq!(ids(|h| !h.api_key_vars.is_empty()), all);
        assert_eq!(
            ids(|h| h.catalogue.is_some()),
            [
                "claude", "codex", "omp", "pi", "opencode", "gemini", "copilot", "grok"
            ]
        );
        assert_eq!(
            ids(|h| h.auto_compaction.is_some()),
            ["claude", "codex", "omp"]
        );
        assert_eq!(ids(|h| h.reads_transcript), ["claude", "codex", "grok"]);
        assert_eq!(
            ids(|h| h.context.is_some()),
            ["claude", "codex", "omp", "pi"]
        );
        // The delivery if-chain `Herdr::deliver` had before its channels
        // were a field: Claude and Codex by id, OMP and Pi by
        // `delivery_channel::supports`, the terminal for everyone else.
        // OpenCode joined the mailbox with its plugin bridge (#508).
        assert_eq!(ids(|h| h.channel.session_bound()), ["claude", "codex"]);
        assert_eq!(ids(|h| h.channel.bridged()), ["omp", "pi", "opencode"]);
        assert_eq!(
            ids(|h| h.channel.journaled()),
            ["claude", "codex", "omp", "pi", "opencode"]
        );
        assert!(!channel("nope").journaled());
    }
}
