//! Per-harness model, effort and context-compaction launch preferences.
//!
//! A model id is passed to the harness as-is (Claude Code family aliases such
//! as `opus`, Codex ids such as `gpt-5.5`), and an effort level is one of the
//! levels the harness accepts. Pi, Oh My Pi and OpenCode take their own
//! `provider/model` ids (Pi and Oh My Pi reach many providers,
//! OpenRouter among them) and, where they have one, a thinking or reasoning
//! level. This module knows how each harness takes those on its command
//! line, seeds or lists the model ids shown by the menus, and validates
//! effort levels; unknown model ids still pass through to the harness. It
//! also knows the one harness that takes its context-compaction threshold
//! through a settings file rather than a flag (`omp`), which is why the
//! overlay for it is written from here.

use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct Catalogue {
    pub harness: &'static str,
    /// Flag that selects the model.
    model_flag: &'static str,
    /// Model ids to offer in menus (the harness may know more).
    pub models: &'static [&'static str],
    /// Effort levels the harness accepts, lowest first; empty when it has no
    /// effort setting.
    pub effort_levels: &'static [&'static str],
    /// Arguments that select an effort level.
    effort_args: fn(&str) -> Vec<String>,
    /// Read the catalogue the harness wrote on this machine, when it writes
    /// one. Files only: config loading and `ssf agents` must not run an
    /// agent to answer.
    catalogue: Option<fn() -> Result<Catalogued>>,
    /// Ask the installed agent which models it has, when it can tell us: the
    /// command line that does it, and the parse of its output.
    list_models: Option<Listing>,
}

/// What a harness writes down about itself.
struct Catalogued {
    /// The file that answered.
    path: PathBuf,
    models: Vec<String>,
}

/// A command line the installed agent answers with its model ids
/// (`pi --list-models`), and the parse of its output.
type Listing = (&'static str, fn() -> Result<Vec<String>>);

fn no_effort(_: &str) -> Vec<String> {
    Vec::new()
}
fn claude_effort(level: &str) -> Vec<String> {
    vec!["--effort".into(), level.into()]
}
fn codex_effort(level: &str) -> Vec<String> {
    vec!["-c".into(), format!("model_reasoning_effort={level}")]
}
fn grok_effort(level: &str) -> Vec<String> {
    vec!["--reasoning-effort".into(), level.into()]
}
fn thinking(level: &str) -> Vec<String> {
    vec!["--thinking".into(), level.into()]
}
/// Claude Code's own one-launch window (100k-1M tokens), which its settings
/// merge into nothing: it is a flag, not part of the one `--settings` JSON
/// the unattended posture needs (`claude_delivery::unattended`).
fn claude_compaction(tokens: u64) -> Vec<String> {
    vec!["--autocompact".into(), tokens.to_string()]
}
/// Codex takes it as a configuration override, the same route the effort
/// level goes (`codex_delivery::endpoint` accepts the pair ssf writes).
fn codex_compaction(tokens: u64) -> Vec<String> {
    vec![
        "-c".into(),
        format!("model_auto_compact_token_limit={tokens}"),
    ]
}

fn run(bin: &str, args: &[&str]) -> Result<String> {
    let out = Command::new(bin)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .with_context(|| format!("running {bin} {}", args.join(" ")))?;
    if !out.status.success() {
        bail!(
            "{bin} {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// `pi --list-models`: a table whose first two columns are provider and model.
fn pi_models() -> Result<Vec<String>> {
    Ok(parse_pi_models(&run("pi", &["--list-models"])?))
}
fn parse_pi_models(table: &str) -> Vec<String> {
    table
        .lines()
        .skip(1)
        .filter_map(|l| {
            let mut cols = l.split_whitespace();
            Some(format!("{}/{}", cols.next()?, cols.next()?))
        })
        .collect()
}

/// `omp models --json`: `{"models":[{"selector":"provider/id", ...}]}`.
fn omp_models() -> Result<Vec<String>> {
    let v: serde_json::Value =
        serde_json::from_str(&run("omp", &["models", "--json"])?).context("parsing omp models")?;
    Ok(v.get("models")
        .and_then(|m| m.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|m| m.get("selector").and_then(|s| s.as_str()))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default())
}

/// `opencode models`: one `provider/model` per line.
fn opencode_models() -> Result<Vec<String>> {
    Ok(run("opencode", &["models"])?
        .lines()
        .map(str::trim)
        .filter(|l| l.contains('/') && !l.contains(char::is_whitespace))
        .map(str::to_string)
        .collect())
}

/// Where a harness keeps its own files: the directory the environment
/// variable `env` names when it is set (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`),
/// else `~/<dotname>`. A test with no home of its own gets none, so a
/// catalogue on the machine running the tests cannot answer for it.
fn harness_dir(env: &str, dotname: &str) -> Option<PathBuf> {
    #[cfg(test)]
    {
        let _ = env;
        catalogue_dir(None, home_dir(), dotname)
    }
    #[cfg(not(test))]
    {
        catalogue_dir(std::env::var_os(env), home_dir(), dotname)
    }
}

/// [`harness_dir`] with its two inputs passed in rather than read, so the
/// precedence is testable: an empty variable counts as unset.
fn catalogue_dir(env: Option<OsString>, home: Option<PathBuf>, dotname: &str) -> Option<PathBuf> {
    env.filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| home.map(|home| home.join(dotname)))
}

#[cfg(not(test))]
fn home_dir() -> Option<PathBuf> {
    dirs::home_dir()
}

#[cfg(test)]
fn home_dir() -> Option<PathBuf> {
    crate::config::test_support::optional_home()
}

/// The array at `key`, or nothing when the document has no such array.
fn array<'a>(value: &'a serde_json::Value, key: &str) -> &'a [serde_json::Value] {
    value
        .get(key)
        .and_then(serde_json::Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

/// Codex writes `models_cache.json`: the models it knows, and a `visibility`
/// of `hide` for the ones its own picker leaves out.
fn codex_catalogue() -> Result<Catalogued> {
    let path = harness_dir("CODEX_HOME", ".codex")
        .context("no home directory for codex's model cache")?
        .join("models_cache.json");
    let raw =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let cache: serde_json::Value =
        serde_json::from_str(&raw).context("parsing codex's model cache")?;
    let mut models = Vec::new();
    for model in array(&cache, "models") {
        if model.get("visibility").and_then(serde_json::Value::as_str) == Some("hide") {
            continue;
        }
        let Some(slug) = model
            .get("slug")
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        models.push(slug.to_string());
    }
    Ok(Catalogued { path, models })
}

/// Claude Code caches the model catalogue it fetched under
/// `cache/model-catalog/`, one file per account or configuration. The newest
/// file that lists models answers: a file that is part-way through being
/// rewritten, or one another surface wrote, has none and is skipped.
fn claude_catalogue() -> Result<Catalogued> {
    let dir = harness_dir("CLAUDE_CONFIG_DIR", ".claude")
        .context("no home directory for claude's model catalog")?
        .join("cache/model-catalog");
    let mut newest: Option<(i64, PathBuf, Vec<String>)> = None;
    for entry in std::fs::read_dir(&dir).with_context(|| format!("reading {}", dir.display()))? {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(catalog) = serde_json::from_str::<serde_json::Value>(&raw) else {
            continue;
        };
        // Claude Code's own surface, when the file says whose it is.
        if let Some(surface) = catalog
            .pointer("/catalog/surface")
            .and_then(serde_json::Value::as_str)
            && surface != "cc"
        {
            continue;
        }
        let models = claude_models(&catalog);
        if models.is_empty() {
            continue;
        }
        let fetched_at = catalog
            .get("fetchedAt")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0);
        if newest
            .as_ref()
            .is_none_or(|(newest, ..)| fetched_at > *newest)
        {
            newest = Some((fetched_at, path, models));
        }
    }
    let (_, path, models) = newest.context("no claude model catalog on this machine")?;
    Ok(Catalogued { path, models })
}

/// `catalog.config.models[].id`, in the order the catalogue lists them.
fn claude_models(catalog: &serde_json::Value) -> Vec<String> {
    catalog
        .pointer("/catalog/config/models")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|model| model.get("id").and_then(serde_json::Value::as_str))
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .collect()
}

const THINKING_LEVELS: &[&str] = &["off", "minimal", "low", "medium", "high", "xhigh", "max"];

const CATALOGUES: &[Catalogue] = &[
    Catalogue {
        harness: "claude",
        model_flag: "--model",
        models: &["fable", "opus", "sonnet", "haiku"],
        effort_levels: &["low", "medium", "high", "xhigh", "max"],
        effort_args: claude_effort,
        catalogue: Some(claude_catalogue),
        list_models: None,
    },
    Catalogue {
        harness: "codex",
        model_flag: "-m",
        models: &[
            "gpt-5.6-sol",
            "gpt-5.6-terra",
            "gpt-5.6-luna",
            "gpt-5.5",
            "gpt-5.2-codex",
        ],
        effort_levels: &["minimal", "low", "medium", "high", "xhigh", "max", "ultra"],
        effort_args: codex_effort,
        catalogue: Some(codex_catalogue),
        list_models: None,
    },
    Catalogue {
        harness: "gemini",
        model_flag: "-m",
        models: &[
            "gemini-3-pro-preview",
            "gemini-3-flash-preview",
            "gemini-2.5-pro",
            "gemini-2.5-flash",
        ],
        effort_levels: &[],
        effort_args: no_effort,
        catalogue: None,
        list_models: None,
    },
    Catalogue {
        harness: "grok",
        model_flag: "-m",
        models: &["grok-4.6", "grok-4.5"],
        effort_levels: &["low", "medium", "high", "xhigh"],
        effort_args: grok_effort,
        catalogue: None,
        list_models: None,
    },
    // These agents use their own `provider/model` identifiers.
    Catalogue {
        harness: "pi",
        model_flag: "--model",
        models: &[],
        effort_levels: THINKING_LEVELS,
        effort_args: thinking,
        catalogue: None,
        list_models: Some(("pi --list-models", pi_models)),
    },
    Catalogue {
        harness: "omp",
        model_flag: "--model",
        models: &[],
        effort_levels: &[
            "off", "minimal", "low", "medium", "high", "xhigh", "max", "auto",
        ],
        effort_args: thinking,
        catalogue: None,
        list_models: Some(("omp models --json", omp_models)),
    },
    Catalogue {
        harness: "opencode",
        model_flag: "-m",
        models: &[],
        effort_levels: &[],
        effort_args: no_effort,
        catalogue: None,
        list_models: Some(("opencode models", opencode_models)),
    },
    Catalogue {
        harness: "copilot",
        model_flag: "--model",
        models: &["auto"],
        effort_levels: &["none", "minimal", "low", "medium", "high", "xhigh", "max"],
        effort_args: claude_effort,
        catalogue: None,
        list_models: None,
    },
];

pub fn catalogue(harness: &str) -> Option<&'static Catalogue> {
    CATALOGUES.iter().find(|c| c.harness == harness)
}

pub fn supports_model(harness: &str) -> bool {
    catalogue(harness).is_some()
}

/// The ids the built-in table seeds for `harness`.
fn seeded_models(cat: &Catalogue) -> Vec<String> {
    cat.models.iter().map(|m| (*m).to_string()).collect()
}

/// Model ids ssf can offer without asking the installed agent: the catalogue
/// that agent wrote on this machine when there is one, else the seeded ids.
/// A harness that lists models only when asked has none here; `ssf models`
/// asks it.
pub fn known_models(harness: &str) -> Vec<String> {
    let Some(cat) = catalogue(harness) else {
        return Vec::new();
    };
    match cat.catalogue.and_then(|read| read().ok()) {
        Some(own) if !own.models.is_empty() => own.models,
        _ => seeded_models(cat),
    }
}

/// Effort levels `harness` accepts, lowest first. Unlike the model ids, these
/// stay ssf's own: they are what a config is checked against when it loads,
/// and a check whose answer moved with a catalogue file would let a cleared
/// `$CODEX_HOME`, or an agent that dropped a level, stop a factory loading.
///
/// A catalogue answers a narrower question than this does: the levels a
/// *model* supports, not the ones the harness's flag accepts. omp's
/// `--thinking` takes `off`, `minimal`, `low`, `medium`, `high`, `xhigh`,
/// `max` and `auto`, while the models `omp models --json` lists support a
/// subset of those; codex's cache gives each model its own
/// `supported_reasoning_levels`, which need not include `minimal`; Claude
/// Code gives each model its own `effort_options`. Reading the levels from a
/// catalogue would refuse levels the flag takes, so `repo.effort` stays
/// checked against the flag's own set, which is what it is turned into (#392).
pub fn effort_levels(harness: &str) -> &'static [&'static str] {
    catalogue(harness).map(|c| c.effort_levels).unwrap_or(&[])
}

/// What a harness takes as its context-compaction threshold: how full its
/// context may get before it summarises its own history. ssf sets a
/// conservative one (`auto_compaction_tokens`) so an unattended session does
/// not grow until the model's own limit does, and a harness ssf knows no way
/// for is left alone whatever the configuration asks: an instance-wide value
/// has to be able to sit above a repository running something else (#404).
enum Compaction {
    /// Flags appended to the launch command, with the counts the harness
    /// accepts (`None`: any count it is given). Claude Code refuses to start
    /// outside its own range, so a count it cannot take is refused while the
    /// configuration is read rather than handed to a launch that would not
    /// come up.
    Args {
        args: fn(u64) -> Vec<String>,
        tokens: Option<std::ops::RangeInclusive<u64>>,
    },
    /// Through its settings, and only that way: `ssf launch` writes the value
    /// as an overlay and points the session at it with `PI_CONFIG_FILES`, omp
    /// having neither a flag nor an environment variable for it.
    Overlay,
}

const AUTO_COMPACTION: &[(&str, Compaction)] = &[
    (
        "claude",
        Compaction::Args {
            args: claude_compaction,
            tokens: Some(100_000..=1_000_000),
        },
    ),
    (
        "codex",
        Compaction::Args {
            args: codex_compaction,
            tokens: None,
        },
    ),
    ("omp", Compaction::Overlay),
];

/// How `harness` takes a context-compaction threshold, when it takes one at
/// all.
fn auto_compaction(harness: &str) -> Option<&'static Compaction> {
    AUTO_COMPACTION
        .iter()
        .find(|(h, _)| *h == harness)
        .map(|(_, c)| c)
}

/// Arguments that give `harness` the context-compaction threshold `tokens`:
/// none for `0` (leave the harness's own default alone), and none for a
/// harness that takes it another way.
pub fn auto_compaction_args(harness: &str, tokens: u64) -> Vec<String> {
    if tokens == 0 {
        return Vec::new();
    }
    match auto_compaction(harness) {
        Some(Compaction::Args { args, .. }) => args(tokens),
        _ => Vec::new(),
    }
}

/// Check a context-compaction threshold against what `harness` accepts, so a
/// value that would keep a launch from coming up is refused while the
/// configuration is read. `0` is "leave the harness's own default alone" and
/// passes; a harness with no threshold is not a harness to check.
pub fn validate_auto_compaction(harness: &str, tokens: u64) -> Result<()> {
    if tokens == 0 {
        return Ok(());
    }
    if let Some(Compaction::Args {
        tokens: Some(range),
        ..
    }) = auto_compaction(harness)
        && !range.contains(&tokens)
    {
        bail!(
            "auto_compaction_tokens {tokens} is not a threshold {harness} accepts ({} to {}); 0 leaves its own default alone",
            range.start(),
            range.end()
        );
    }
    Ok(())
}

/// The settings overlay that carries the threshold for the harness that takes
/// it no other way, and the YAML it holds: omp's `compaction.thresholdTokens`.
/// One file per value, under the directory the session's own harness files
/// live in, so a launch can point at it without a flag. A file of ours that is
/// already right is left alone, so pointing a session at it never rewrites one
/// another session is reading.
fn omp_compaction_overlay(tokens: u64) -> (PathBuf, String) {
    (
        crate::config::state_dir()
            .join("harness")
            .join(format!("omp-compaction-{tokens}.yml")),
        format!(
            "# Written by ssf for the sessions it starts; the value comes from\n\
             # auto_compaction_tokens in its configuration. An overlay of your own\n\
             # still applies, and one naming this key after it would win.\n\
             compaction:\n  thresholdTokens: {tokens}\n"
        ),
    )
}

/// Write that overlay, when this build has not written it already.
pub fn write_omp_compaction_overlay(tokens: u64) -> Result<PathBuf> {
    let (path, body) = omp_compaction_overlay(tokens);
    if std::fs::read_to_string(&path).is_ok_and(|on_disk| on_disk == body) {
        return Ok(path);
    }
    let dir = path.parent().expect("the overlay has a parent directory");
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    crate::config::write_atomic(&path, body.as_bytes(), 0o644)?;
    Ok(path)
}

/// Flags that make a harness run without stopping for approval: every tool
/// call is allowed and the first-run trust question, where a flag can answer
/// it, is answered. ssf's terminals are unmanned, so nothing could answer a
/// prompt; Claude Code's `AskUserQuestion` tool is dropped for the same
/// reason. Login and first-run onboarding survive all of these and are
/// machine setup.
const UNATTENDED_FLAGS: &[(&str, &str)] = &[
    (
        "claude",
        "--dangerously-skip-permissions --disallowedTools AskUserQuestion --settings '{\"crossSessionInbound\":\"accept\"}'",
    ),
    (
        "codex",
        "--dangerously-bypass-approvals-and-sandbox --dangerously-bypass-hook-trust",
    ),
    ("gemini", "--yolo --skip-trust"),
    ("grok", "--always-approve"),
    // Pi has no tool approvals; `--approve` trusts the project's `.pi/` files.
    ("pi", "--approve"),
    ("omp", "--auto-approve"),
    ("opencode", "--auto"),
    ("copilot", "--allow-all"),
    ("crush", "--yolo"),
];

/// OMP's normal five-minute inter-event watchdog can end a healthy, long
/// reasoning turn. It retries before any visible output, but deliberately
/// stops after partial output because replay could duplicate work. Factory
/// sessions are unattended, so give them the longer timeout OMP recommends
/// for this workload (#322). A configured `repo.command` remains authoritative.
const OMP_DEFAULT_COMMAND: &str = "PI_STREAM_IDLE_TIMEOUT_MS=900000 \"$SSF_PI_LAUNCHER\" omp --auto-approve -e \"$SSF_PI_BRIDGE\"";

/// The flags that let `harness` run unattended, if ssf knows them.
pub fn unattended_flags(harness: &str) -> Option<&'static str> {
    UNATTENDED_FLAGS
        .iter()
        .find(|(h, _)| *h == harness)
        .map(|(_, f)| *f)
}

/// The command that starts `harness` when `repo.command` is not set: the
/// harness id plus its unattended flags, or the bare id for a harness ssf
/// does not know.
pub fn default_command(harness: &str) -> String {
    if harness == "omp" {
        return OMP_DEFAULT_COMMAND.into();
    }
    if harness == "pi" {
        return "\"$SSF_PI_LAUNCHER\" pi --approve -e \"$SSF_PI_BRIDGE\"".into();
    }
    match unattended_flags(harness) {
        Some(flags) => format!("{harness} {flags}"),
        None => harness.to_string(),
    }
}

/// Model ids to offer for a harness, and where they came from.
#[derive(Debug, Clone, Serialize)]
pub struct Available {
    pub harness: String,
    /// Model ids, in the order the source lists them.
    pub models: Vec<String>,
    pub source: Source,
}

/// Where a harness's model ids came from.
#[derive(Debug, Clone, Serialize)]
pub struct Source {
    /// The harness's own catalogue, its listing command, or ssf's table.
    pub kind: SourceKind,
    /// The file or command line that answered; empty for the table.
    pub detail: String,
}

/// Which of the three answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    /// A catalogue the installed agent wrote on this machine.
    Catalogue,
    /// A listing command the installed agent answered on this machine.
    Command,
    /// ssf's built-in table, when the machine has neither.
    Table,
}

impl Source {
    fn table() -> Self {
        Self {
            kind: SourceKind::Table,
            detail: String::new(),
        }
    }

    fn catalogue(path: &Path) -> Self {
        Self {
            kind: SourceKind::Catalogue,
            detail: path.display().to_string(),
        }
    }

    fn command(command: &str) -> Self {
        Self {
            kind: SourceKind::Command,
            detail: command.to_string(),
        }
    }

    /// A phrase naming the source, for the note `ssf models` prints under
    /// the ids.
    pub fn describe(&self, harness: &str) -> String {
        match self.kind {
            SourceKind::Table => format!("ssf's built-in table for {harness}"),
            SourceKind::Catalogue => format!("{harness}'s own model catalogue ({})", self.detail),
            SourceKind::Command => format!("{harness}'s own list ({})", self.detail),
        }
    }
}

/// Model ids to offer for `harness`: what the installed agent itself lists on
/// this machine, else ssf's built-in table. The agent's own list wins because
/// the table goes stale between releases (#382).
pub fn available(harness: &str) -> Result<Available> {
    let Some(cat) = catalogue(harness) else {
        bail!("{harness} does not take a model setting");
    };
    // A catalogue that will not read, or lists nothing, is no answer at all:
    // the table is better than an empty list, and `source` says which one
    // the ids below came from.
    let mut source = Source::table();
    let mut models = Vec::new();
    if let Some(own) = cat.catalogue.and_then(|read| read().ok())
        && !own.models.is_empty()
    {
        source = Source::catalogue(&own.path);
        models = own.models;
    }
    if models.is_empty()
        && let Some((command, list)) = cat.list_models
    {
        let ids: Vec<String> = list()?.into_iter().filter(|m| !m.is_empty()).collect();
        if !ids.is_empty() {
            source = Source::command(command);
            models = ids;
        }
    }
    if models.is_empty() {
        models = seeded_models(cat);
    }
    Ok(Available {
        harness: harness.to_string(),
        models,
        source,
    })
}

/// Check that `model` and `effort` can be applied to `harness`. Model ids are
/// opaque (the harness decides whether it knows them); effort levels must be
/// ones the harness accepts.
pub fn validate(harness: &str, model: Option<&str>, effort: Option<&str>) -> Result<()> {
    if let Some(m) = model.map(str::trim) {
        if m.is_empty() {
            bail!("model must not be empty");
        }
        if m.contains(char::is_whitespace) {
            bail!("model must be a single identifier, got {m:?}");
        }
        if !supports_model(harness) {
            bail!(
                "{harness} does not take a model setting (supported: {})",
                CATALOGUES
                    .iter()
                    .map(|c| c.harness)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }
    if let Some(e) = effort.map(str::trim) {
        if e.is_empty() {
            bail!("effort must not be empty");
        }
        let levels = effort_levels(harness);
        if levels.is_empty() {
            bail!(
                "{harness} does not take an effort level (supported: {})",
                CATALOGUES
                    .iter()
                    .filter(|c| !c.effort_levels.is_empty())
                    .map(|c| c.harness)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        if !levels.contains(&e) {
            bail!(
                "effort {e:?} is not a level {harness} accepts (one of: {})",
                levels.join(", ")
            );
        }
    }
    Ok(())
}

/// Command-line arguments that make `harness` use `model` and `effort`.
/// Settings the harness has no way to take are dropped.
pub fn launch_args(harness: &str, model: Option<&str>, effort: Option<&str>) -> Vec<String> {
    let Some(cat) = catalogue(harness) else {
        return Vec::new();
    };
    let mut args = Vec::new();
    if let Some(m) = model.map(str::trim).filter(|m| !m.is_empty()) {
        args.push(cat.model_flag.to_string());
        args.push(m.to_string());
    }
    // The same levels `validate` accepts: a level ssf let a config keep is
    // one the harness is handed.
    if let Some(e) = effort
        .map(str::trim)
        .filter(|e| cat.effort_levels.contains(e))
    {
        args.extend((cat.effort_args)(e));
    }
    args
}

/// `command` with the model, effort and context-compaction arguments
/// appended, shell-quoted. `auto_compaction_tokens` is `0` when the harness's
/// own default is to be left alone.
pub fn apply_to_command(
    command: &str,
    harness: &str,
    model: Option<&str>,
    effort: Option<&str>,
    auto_compaction_tokens: u64,
) -> String {
    let mut out = command.trim_end().to_string();
    for arg in launch_args(harness, model, effort)
        .into_iter()
        .chain(auto_compaction_args(harness, auto_compaction_tokens))
    {
        out.push(' ');
        out.push_str(&shell_word(&arg));
    }
    out
}

/// Quote a word for `sh -c` unless it is plain enough to leave alone.
fn shell_word(value: &str) -> String {
    let plain = !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.=/:,+@%".contains(c));
    if plain {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_gets_model_and_effort_flags() {
        assert_eq!(
            apply_to_command(
                "claude --dangerously-skip-permissions",
                "claude",
                Some("opus"),
                Some("high"),
                0
            ),
            "claude --dangerously-skip-permissions --model opus --effort high"
        );
        assert_eq!(
            apply_to_command("claude", "claude", None, Some("max"), 0),
            "claude --effort max"
        );
        assert_eq!(
            apply_to_command("claude", "claude", None, None, 0),
            "claude"
        );
    }

    #[test]
    fn codex_uses_short_model_flag_and_config_override() {
        assert_eq!(
            apply_to_command("codex", "codex", Some("gpt-5.5"), Some("xhigh"), 0),
            "codex -m gpt-5.5 -c model_reasoning_effort=xhigh"
        );
    }

    #[test]
    fn grok_and_gemini_flags() {
        assert_eq!(
            apply_to_command("grok", "grok", Some("grok-4.6"), Some("xhigh"), 0),
            "grok -m grok-4.6 --reasoning-effort xhigh"
        );
        assert_eq!(
            apply_to_command("gemini", "gemini", Some("gemini-2.5-pro"), None, 0),
            "gemini -m gemini-2.5-pro"
        );
    }

    #[test]
    fn pi_family_take_provider_models_and_thinking_levels() {
        assert_eq!(
            apply_to_command(
                "pi",
                "pi",
                Some("openrouter/anthropic/claude-sonnet-4"),
                Some("high"),
                0
            ),
            "pi --model openrouter/anthropic/claude-sonnet-4 --thinking high"
        );
        assert_eq!(
            apply_to_command("omp", "omp", Some("openai-codex/gpt-5.4"), Some("auto"), 0),
            "omp --model openai-codex/gpt-5.4 --thinking auto"
        );
        assert_eq!(
            apply_to_command("opencode", "opencode", Some("openrouter/x"), None, 0),
            "opencode -m openrouter/x"
        );
        assert_eq!(
            apply_to_command("copilot", "copilot", Some("auto"), Some("xhigh"), 0),
            "copilot --model auto --effort xhigh"
        );
        assert!(validate("pi", None, Some("auto")).is_err());
        assert!(validate("omp", None, Some("auto")).is_ok());
        assert!(validate("opencode", None, Some("high")).is_err());
    }

    #[test]
    fn pi_model_table_is_parsed() {
        let table = "provider    model          context  max-out\nopenrouter  ~anthropic/claude-opus-latest  1M  128K\nanthropic   claude-sonnet-4  200K  64K\n";
        assert_eq!(
            parse_pi_models(table),
            vec![
                "openrouter/~anthropic/claude-opus-latest",
                "anthropic/claude-sonnet-4"
            ]
        );
    }

    #[test]
    fn unsupported_settings_are_dropped_from_the_command() {
        assert_eq!(
            apply_to_command("crush", "crush", Some("x"), Some("high"), 0),
            "crush"
        );
        // Gemini has no effort setting.
        assert_eq!(
            apply_to_command("gemini", "gemini", None, Some("high"), 0),
            "gemini"
        );
        // A level the harness does not accept is not passed on.
        assert_eq!(
            apply_to_command("claude", "claude", None, Some("ultra"), 0),
            "claude"
        );
    }

    #[test]
    fn validation() {
        assert!(validate("claude", Some("opus"), Some("high")).is_ok());
        assert!(validate("claude", Some("claude-opus-5"), None).is_ok());
        assert!(validate("claude", None, None).is_ok());
        assert!(validate("codex", Some("gpt-5.5"), Some("ultra")).is_ok());
        assert!(validate("claude", None, Some("ultra")).is_err());
        assert!(validate("claude", Some("opus sonnet"), None).is_err());
        assert!(validate("claude", Some(""), None).is_err());
        assert!(validate("gemini", None, Some("high")).is_err());
        assert!(validate("crush", Some("x"), None).is_err());
        assert!(validate("crush", None, None).is_ok());
    }

    #[test]
    fn default_commands_are_permission_free() {
        assert_eq!(
            default_command("claude"),
            "claude --dangerously-skip-permissions --disallowedTools AskUserQuestion --settings '{\"crossSessionInbound\":\"accept\"}'"
        );
        assert_eq!(
            default_command("codex"),
            "codex --dangerously-bypass-approvals-and-sandbox --dangerously-bypass-hook-trust"
        );
        assert_eq!(default_command("gemini"), "gemini --yolo --skip-trust");
        assert_eq!(default_command("grok"), "grok --always-approve");
        assert_eq!(
            default_command("pi"),
            "\"$SSF_PI_LAUNCHER\" pi --approve -e \"$SSF_PI_BRIDGE\""
        );
        assert_eq!(
            default_command("omp"),
            "PI_STREAM_IDLE_TIMEOUT_MS=900000 \"$SSF_PI_LAUNCHER\" omp --auto-approve -e \"$SSF_PI_BRIDGE\""
        );
        assert_eq!(default_command("opencode"), "opencode --auto");
        assert_eq!(default_command("copilot"), "copilot --allow-all");
        assert_eq!(default_command("crush"), "crush --yolo");
        // Every agent ssf knows has unattended flags.
        for a in crate::agents::list() {
            assert!(unattended_flags(&a.id).is_some(), "{}", a.id);
        }
        // An unknown harness is started as given.
        assert_eq!(default_command("aider"), "aider");
        // Model and effort go after the flags.
        assert_eq!(
            apply_to_command(&default_command("codex"), "codex", Some("gpt-5.5"), None, 0),
            "codex --dangerously-bypass-approvals-and-sandbox --dangerously-bypass-hook-trust -m gpt-5.5"
        );
    }

    #[test]
    fn odd_model_ids_are_quoted() {
        assert_eq!(
            apply_to_command("claude", "claude", Some("it's"), None, 0),
            "claude --model 'it'\\''s'"
        );
    }

    /// The three harnesses that take a context-compaction threshold, each the
    /// way that harness takes it, and the rest left alone however the config
    /// is set.
    #[test]
    fn a_context_compaction_threshold_reaches_the_harnesses_that_take_one() {
        // claude and codex on the command line, after the model and effort.
        assert_eq!(
            apply_to_command("claude", "claude", Some("opus"), Some("high"), 300_000),
            "claude --model opus --effort high --autocompact 300000"
        );
        assert_eq!(
            apply_to_command("codex", "codex", Some("gpt-5.5"), None, 300_000),
            "codex -m gpt-5.5 -c model_auto_compact_token_limit=300000"
        );
        // omp has no flag or environment variable for it: its value is an
        // overlay `ssf launch` writes, so nothing goes on the command line.
        assert!(auto_compaction_args("omp", 300_000).is_empty());
        assert!(matches!(auto_compaction("omp"), Some(Compaction::Overlay)));
        // A harness without one is unaffected, and a count its harness
        // refuses is dropped rather than shelled into a launch that would
        // not come up.
        for harness in [
            "pi", "opencode", "gemini", "grok", "copilot", "crush", "aider",
        ] {
            assert!(
                auto_compaction_args(harness, 300_000).is_empty(),
                "{harness}"
            );
            assert!(auto_compaction(harness).is_none(), "{harness}");
        }
        assert!(auto_compaction_args("claude", 500_000) == ["--autocompact", "500000"]);
        // 0 is "leave the harness's own default alone".
        assert!(auto_compaction_args("claude", 0).is_empty());
        assert!(auto_compaction_args("codex", 0).is_empty());
    }

    /// Claude Code refuses to start outside its own window, so a count it
    /// cannot take is refused while the configuration is read; a harness
    /// without a ceiling of its own takes any count.
    #[test]
    fn a_threshold_outside_the_harnesss_own_range_is_refused() {
        assert!(validate_auto_compaction("claude", 100_000).is_ok());
        assert!(validate_auto_compaction("claude", 1_000_000).is_ok());
        assert!(validate_auto_compaction("claude", 300_000).is_ok());
        let err = validate_auto_compaction("claude", 99_999).unwrap_err();
        let text = format!("{err:#}");
        assert!(text.contains("100000"), "{text}");
        assert!(text.contains("1000000"), "{text}");
        assert!(validate_auto_compaction("claude", 1_000_001).is_err());
        for harness in ["codex", "omp", "pi", "aider"] {
            assert!(validate_auto_compaction(harness, 1).is_ok(), "{harness}");
            assert!(
                validate_auto_compaction(harness, 10_000_000).is_ok(),
                "{harness}"
            );
        }
        // 0 is each harness's own default, whatever that harness is.
        assert!(validate_auto_compaction("claude", 0).is_ok());
    }

    /// omp's overlay: the one key it reads its threshold from, under the state
    /// directory `ssf launch` already materialises the bridge and launcher in.
    #[test]
    fn the_omp_overlay_carries_the_threshold_and_nothing_else() {
        let sandbox = crate::config::test_support::sandbox();
        let path = write_omp_compaction_overlay(300_000).unwrap();
        assert!(path.starts_with(sandbox.root()), "{}", path.display());
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(
            body.contains("compaction:\n  thresholdTokens: 300000\n"),
            "{body}"
        );
        // The comment says where the value comes from, so an operator who
        // finds the file knows what wrote it.
        assert!(body.contains("auto_compaction_tokens"), "{body}");
        // Another count is another file: one session's overlay is never
        // rewritten under a running session.
        let other = write_omp_compaction_overlay(150_000).unwrap();
        assert_ne!(other, path);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            body,
            "the first overlay is left alone"
        );
        assert!(std::fs::read_to_string(&other).unwrap().contains("150000"));
    }

    fn write(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    /// codex's cache names the models its own picker offers, so one it added
    /// after ssf's table was written shows up; `hide` stays out.
    #[test]
    fn codex_offers_the_models_its_own_cache_lists() {
        let sandbox = crate::config::test_support::sandbox();
        write(
            &sandbox.home().join(".codex/models_cache.json"),
            r#"{"models":[
                {"slug":"gpt-5.6-sol","visibility":"list"},
                {"slug":"gpt-reserve","visibility":"hide"},
                {"slug":"gpt-6-astra","visibility":"list"},
                {"slug":"","visibility":"list"},
                {"visibility":"list"}
            ]}"#,
        );
        let available = available("codex").unwrap();
        assert_eq!(available.models, vec!["gpt-5.6-sol", "gpt-6-astra"]);
        assert_eq!(available.source.kind, SourceKind::Catalogue);
        assert!(
            available
                .source
                .detail
                .ends_with(".codex/models_cache.json"),
            "{}",
            available.source.detail
        );
        // `ssf agents` seeds its model list from the same cache.
        assert_eq!(known_models("codex"), vec!["gpt-5.6-sol", "gpt-6-astra"]);
        // Effort levels stay ssf's own: they are what a config is checked
        // against when it loads, so a cache file cannot move them.
        assert_eq!(
            effort_levels("codex"),
            ["minimal", "low", "medium", "high", "xhigh", "max", "ultra"]
        );
    }

    /// Claude Code writes one catalogue file per configuration, and the
    /// newest one that lists models answers: a file part-way through being
    /// written, one another surface wrote, and one whose models moved
    /// elsewhere in a format change are all skipped rather than answering
    /// with nothing and sending `ssf models` back to the stale table.
    #[test]
    fn claude_offers_the_newest_catalogues_models() {
        let sandbox = crate::config::test_support::sandbox();
        let dir = sandbox.home().join(".claude/cache/model-catalog");
        write(
            &dir.join("old.json"),
            r#"{"fetchedAt":10,"catalog":{"surface":"cc","config":{"models":[{"id":"claude-old"}]}}}"#,
        );
        write(
            &dir.join("new.json"),
            r#"{"fetchedAt":20,"catalog":{"surface":"cc","config":{"models":[
                {"id":"claude-opus-5"},
                {"id":"claude-haiku-4-5","thinking":{"type":"none"}}
            ]}}}"#,
        );
        write(&dir.join("half-written.json"), "{");
        write(
            &dir.join("other-surface.json"),
            r#"{"fetchedAt":30,"catalog":{"surface":"web","config":{"models":[{"id":"web-only"}]}}}"#,
        );
        write(
            &dir.join("elsewhere.json"),
            r#"{"fetchedAt":40,"catalog":{"surface":"cc","config":{}}}"#,
        );
        let available = available("claude").unwrap();
        assert_eq!(available.models, vec!["claude-opus-5", "claude-haiku-4-5"]);
        assert_eq!(available.source.kind, SourceKind::Catalogue);
        assert!(
            available.source.detail.ends_with("new.json"),
            "{}",
            available.source.detail
        );
    }

    /// The two environment variables that move a harness's own directory,
    /// and the home each falls back to.
    #[test]
    fn catalogue_directories_follow_the_harnesss_environment() {
        let home = Some(PathBuf::from("/home/ssf"));
        assert_eq!(
            catalogue_dir(Some("/var/lib/codex".into()), home.clone(), ".codex"),
            Some(PathBuf::from("/var/lib/codex"))
        );
        assert_eq!(
            catalogue_dir(None, home.clone(), ".codex"),
            Some(PathBuf::from("/home/ssf/.codex"))
        );
        // An empty variable is unset, not a directory.
        assert_eq!(
            catalogue_dir(Some(OsString::new()), home, ".claude"),
            Some(PathBuf::from("/home/ssf/.claude"))
        );
        assert_eq!(catalogue_dir(None, None, ".codex"), None);
    }

    /// A machine that has never run the agent keeps working, and says so.
    #[test]
    fn the_seeded_table_answers_without_a_catalogue() {
        let _sandbox = crate::config::test_support::sandbox();
        let available = available("codex").unwrap();
        assert_eq!(available.source.kind, SourceKind::Table);
        assert!(available.source.detail.is_empty());
        assert_eq!(
            available.models,
            vec![
                "gpt-5.6-sol",
                "gpt-5.6-terra",
                "gpt-5.6-luna",
                "gpt-5.5",
                "gpt-5.2-codex"
            ]
        );
        assert_eq!(known_models("codex"), available.models);
        // The note says which list the ids came from.
        assert_eq!(
            available.source.describe("codex"),
            "ssf's built-in table for codex"
        );
    }
}
