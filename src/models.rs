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
//! overlay for it is written from here. The per-harness data (flags, seeded
//! ids, effort levels, compaction route) is each harness's row in
//! [`crate::harness::HARNESSES`]; this module holds the functions it names.

use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

/// How a harness takes a model and an effort level, and where its model ids
/// come from: one field of its row in [`crate::harness::HARNESSES`].
pub struct Catalogue {
    /// Flag that selects the model.
    pub model_flag: &'static str,
    /// Model ids to offer in menus (the harness may know more).
    pub models: &'static [&'static str],
    /// Effort levels the harness accepts, lowest first; empty when it has no
    /// effort setting.
    pub effort_levels: &'static [&'static str],
    /// Arguments that select an effort level.
    pub effort_args: fn(&str) -> Vec<String>,
    /// Read the catalogue the harness wrote on this machine, when it writes
    /// one. Files only: config loading and `ssf agents` must not run an
    /// agent to answer.
    pub catalogue: Option<fn() -> Result<Catalogued>>,
    /// Ask the installed agent which models it has, when it can tell us: the
    /// command line that does it, and the parse of its output.
    pub list_models: Option<Listing>,
    /// Make the installed agent rewrite the catalogue above, for a harness
    /// with no way to be asked for its models at all. Run only when the
    /// catalogue on this machine is missing, or past the freshness the
    /// harness's own file stamps on it.
    pub refresh: Option<fn()>,
}

/// What a harness writes down about itself.
pub struct Catalogued {
    /// The file that answered.
    path: PathBuf,
    models: Vec<String>,
    /// The point the harness's own file says it goes stale, in the epoch
    /// milliseconds it stamps it with. A file with no stamp is read as
    /// current: only a harness that says when its list expires can be told
    /// that it has.
    stale_at: Option<i64>,
}

/// A command line the installed agent answers with its model ids
/// (`pi --list-models`), and the parse of its output.
pub type Listing = (&'static str, fn() -> Result<Vec<String>>);

/// What a stub start does in the harness's place: write the catalogue the
/// harness would have rewritten.
#[cfg(test)]
type StartHook = Box<dyn FnMut(&str)>;

/// What a test's stub start is: the harnesses ssf asked to be started for their
/// catalogue, in the order it asked, and what a start does in the harness's
/// place.
#[cfg(test)]
#[derive(Default)]
struct Started {
    asked: Vec<String>,
    /// Run where the harness would have run, so a test can write the catalogue
    /// the harness would have rewritten. Nothing else stands in for it: no
    /// test starts an agent.
    hook: Option<StartHook>,
}

#[cfg(test)]
thread_local! {
    /// The harnesses a test asked to be started for their catalogue, in order.
    /// A refresh is a side effect on the machine the tests run on, so a test
    /// records the request instead of making it (see `start_refresh`).
    static STARTED: std::cell::RefCell<Started> = std::cell::RefCell::default();
}

/// Take the harnesses started since the last call, and clear the record.
#[cfg(test)]
fn started() -> Vec<String> {
    STARTED.with(|started| std::mem::take(&mut started.borrow_mut().asked))
}

/// What a start does while it is being tested, in place of running the harness,
/// until the guard this returns is dropped.
///
/// The guard is what keeps the seam in the test that set it: libtest reuses its
/// worker threads, so a hook left installed would run inside the next test on
/// that thread -- writing the catalogue it means to write into a sandbox that
/// test has already dropped.
#[cfg(test)]
fn on_start(hook: impl FnMut(&str) + 'static) -> StartGuard {
    STARTED.with(|started| started.borrow_mut().hook = Some(Box::new(hook)));
    StartGuard
}

/// Puts the seam back when the test that installed a hook ends.
#[cfg(test)]
struct StartGuard;

#[cfg(test)]
impl Drop for StartGuard {
    fn drop(&mut self) {
        STARTED.with(|started| started.borrow_mut().hook = None);
    }
}

pub(crate) fn no_effort(_: &str) -> Vec<String> {
    Vec::new()
}
pub(crate) fn claude_effort(level: &str) -> Vec<String> {
    vec!["--effort".into(), level.into()]
}
pub(crate) fn codex_effort(level: &str) -> Vec<String> {
    vec!["-c".into(), format!("model_reasoning_effort={level}")]
}
pub(crate) fn grok_effort(level: &str) -> Vec<String> {
    vec!["--reasoning-effort".into(), level.into()]
}
pub(crate) fn thinking(level: &str) -> Vec<String> {
    vec!["--thinking".into(), level.into()]
}
/// Claude Code's own one-launch window (100k-1M tokens), which its settings
/// merge into nothing: it is a flag, not part of the one `--settings` JSON
/// the unattended posture needs (`claude_delivery::unattended`).
pub(crate) fn claude_compaction(tokens: u64) -> Vec<String> {
    vec!["--autocompact".into(), tokens.to_string()]
}
/// Codex takes it as a configuration override, the same route the effort
/// level goes (`codex_delivery::endpoint` accepts the pair ssf writes).
pub(crate) fn codex_compaction(tokens: u64) -> Vec<String> {
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

/// Start `bin` with `args` so it rewrites its own model catalogue, and take
/// nothing from the run: no output, no exit status. The file it leaves behind
/// is what answers, and the caller reads that next.
///
/// The updater is off for this start. A listing is a read, and a harness that
/// installs itself while answering one would be doing something nobody asked
/// for on a machine whose agents are the package manager's to update.
fn start_refresh(bin: &str, args: &[&str]) {
    #[cfg(test)]
    {
        // Nothing is started under test: the agents on the machine running the
        // tests are not the test's to start, the same rule `harness_dir`
        // follows for the directories it reads. The request is recorded
        // instead, and a test that needs the file a harness would have written
        // writes it from its own hook.
        let _ = args;
        STARTED.with(|started| {
            let mut started = started.borrow_mut();
            started.asked.push(bin.to_string());
            if let Some(hook) = started.hook.as_mut() {
                hook(bin);
            }
        });
    }
    #[cfg(not(test))]
    {
        let mut command = Command::new(bin);
        command
            .args(args)
            .env("DISABLE_AUTOUPDATER", "1")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let _ = command.status();
    }
}

/// The epoch milliseconds a catalogue's own freshness stamp is read against.
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_millis() as i64)
        .unwrap_or(0)
}

/// The catalogue on this machine, when it reads and lists something. One that
/// will not read, or lists nothing, is no answer at all: the table is better
/// than an empty list, and `source` says which one the ids came from.
fn catalogue_of(cat: &Catalogue) -> Option<Catalogued> {
    cat.catalogue
        .and_then(|read| read().ok())
        .filter(|own| !own.models.is_empty())
}

/// Whether the harness has to be started to answer: there is no catalogue on
/// this machine, or the harness's own stamp says the one there is has gone
/// stale. A file with no stamp is taken as current -- see `Catalogued`.
fn needs_refresh(own: Option<&Catalogued>, now: i64) -> bool {
    own.is_none_or(|own| own.stale_at.is_some_and(|stale| now >= stale))
}

/// `pi --list-models`: a table whose first two columns are provider and model.
pub(crate) fn pi_models() -> Result<Vec<String>> {
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
pub(crate) fn omp_models() -> Result<Vec<String>> {
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

/// How a harness lists its models' context windows: the listing command,
/// the file its output is cached in under `$XDG_CACHE_HOME/ssf`, and how a
/// window is read out of that output.
struct WindowListing {
    name: &'static str,
    command: &'static str,
    cache: &'static str,
    window_in: fn(&str, &str, &str) -> Option<u64>,
}

const OMP_LISTING: WindowListing = WindowListing {
    name: "omp",
    command: "omp models --json",
    cache: "omp-models.json",
    window_in: omp_window_in,
};

const PI_LISTING: WindowListing = WindowListing {
    name: "pi",
    command: "pi --list-models",
    cache: "pi-models.txt",
    window_in: pi_window_in,
};

/// The context window `omp models --json` lists for `provider`'s `model`.
pub(crate) fn omp_context_window(provider: &str, model: &str) -> Option<u64> {
    listed_context_window(&OMP_LISTING, provider, model)
}

/// The context window `pi --list-models` lists for `provider`'s `model`.
pub(crate) fn pi_context_window(provider: &str, model: &str) -> Option<u64> {
    listed_context_window(&PI_LISTING, provider, model)
}

/// The context window `listing` gives for `provider`'s `model`.
///
/// Asking the harness can take seconds, which a post must not wait on, so the
/// listing is cached in `$XDG_CACHE_HOME/ssf/` (`~/.cache` by default) and
/// answered from there, stale or not. A listing older than a day, or one
/// without this model and more than a few minutes old, is refreshed in the
/// background for the next post; until then this post goes without.
fn listed_context_window(listing: &WindowListing, provider: &str, model: &str) -> Option<u64> {
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".cache")))?
        .join("ssf")
        .join(listing.cache);
    let (window, refresh) = cached_window(&cache, listing, provider, model);
    if refresh {
        refresh_listing(&cache, listing);
    }
    window
}

/// The window the cached listing at `path` gives, and whether to refresh it.
fn cached_window(
    path: &Path,
    listing: &WindowListing,
    provider: &str,
    model: &str,
) -> (Option<u64>, bool) {
    const DAY: u64 = 24 * 60 * 60;
    const RETRY: u64 = 5 * 60;
    let age = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .map(|t| t.elapsed().map_or(0, |d| d.as_secs()));
    let window = std::fs::read_to_string(path)
        .ok()
        .and_then(|text| (listing.window_in)(&text, provider, model));
    let refresh = match age {
        None => true,
        Some(age) => age >= DAY || (window.is_none() && age >= RETRY),
    };
    (window, refresh)
}

/// Rewrite the cached listing at `cache` in a detached process that outlives
/// this one; a failed listing leaves the old file in place.
fn refresh_listing(cache: &Path, listing: &WindowListing) {
    #[cfg(test)]
    {
        let _ = (cache, listing.command);
        STARTED.with(|started| started.borrow_mut().asked.push(listing.name.into()));
    }
    #[cfg(not(test))]
    {
        use std::os::unix::process::CommandExt;
        let _ = listing.name;
        let Some(dir) = cache.parent() else { return };
        if std::fs::create_dir_all(dir).is_err() {
            return;
        }
        let _ = Command::new("sh")
            .arg("-c")
            .arg(format!(
                r#"t="$1.$$.tmp"; {} > "$t" && mv -f "$t" "$1" || rm -f "$t""#,
                listing.command
            ))
            .arg("sh")
            .arg(cache)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .process_group(0)
            .spawn();
    }
}

fn omp_window_in(listing: &str, provider: &str, model: &str) -> Option<u64> {
    let v: serde_json::Value = serde_json::from_str(listing).ok()?;
    v.get("models")?
        .as_array()?
        .iter()
        .find(|m| m["provider"].as_str() == Some(provider) && m["id"].as_str() == Some(model))?
        .get("contextWindow")?
        .as_u64()
        .filter(|w| *w > 0)
}

/// The `context` column of the `pi --list-models` row for `provider`'s
/// `model`. Pi rounds it for display (`200K`, `262.1K`, `1.0M`), so the window
/// read back is within a tenth of a unit of the real one.
fn pi_window_in(table: &str, provider: &str, model: &str) -> Option<u64> {
    let size = table.lines().skip(1).find_map(|l| {
        let mut cols = l.split_whitespace();
        if cols.next()? == provider && cols.next()? == model {
            cols.next()
        } else {
            None
        }
    })?;
    let (number, unit) = match size.as_bytes().last()? {
        b'K' | b'k' => (&size[..size.len() - 1], 1_000.0),
        b'M' | b'm' => (&size[..size.len() - 1], 1_000_000.0),
        _ => (size, 1.0),
    };
    let window = (number.parse::<f64>().ok()? * unit).round();
    (window >= 1.0).then_some(window as u64)
}

/// `opencode models`: one `provider/model` per line.
pub(crate) fn opencode_models() -> Result<Vec<String>> {
    Ok(run("opencode", &["models"])?
        .lines()
        .map(str::trim)
        .filter(|l| l.contains('/') && !l.contains(char::is_whitespace))
        .map(str::to_string)
        .collect())
}

/// `grok models`: the ids listed under `Available models:`, one per line as
/// `* grok-4.6 (default)` or `- grok-4.5`. It answers without a sign-in.
/// Grok has a table to fall back on, so a listing that fails is an empty one.
pub(crate) fn grok_models() -> Result<Vec<String>> {
    Ok(run("grok", &["models"])
        .map(|listing| parse_grok_models(&listing))
        .unwrap_or_default())
}
fn parse_grok_models(listing: &str) -> Vec<String> {
    listing
        .lines()
        .skip_while(|l| !l.trim_start().starts_with("Available models"))
        .skip(1)
        .filter_map(|l| {
            let rest = l.trim_start().strip_prefix(['*', '-'])?;
            rest.split_whitespace().next().map(str::to_string)
        })
        .collect()
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
pub(crate) fn codex_catalogue() -> Result<Catalogued> {
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
    Ok(Catalogued {
        path,
        models,
        stale_at: None,
    })
}

/// Start Claude Code so it refreshes the model catalogue it keeps under
/// `cache/model-catalog/`. It has no command that lists its models
/// (`claude --help`), so this is the only way to ask it, and `--print` is what
/// makes the ask a short one: it starts non-interactively, stops for lack of a
/// prompt before it reaches any model call, and has fetched the catalogue by
/// then. A start that stops earlier -- no login, no network -- leaves the
/// catalogue as it was, which `available` then reads as before.
pub(crate) fn claude_refresh() {
    start_refresh("claude", &["--print"]);
}

/// The context window of Claude model `id` in tokens, where ssf knows it:
/// the catalogue Claude Code caches does not say. A `[1m]` suffix asks for
/// the 1M window; the Claude 5 family has it by default, Haiku 4.5 200k.
pub(crate) fn claude_context_window(id: &str) -> Option<u64> {
    let id = id.strip_prefix("anthropic/").unwrap_or(id);
    if id.ends_with("[1m]") {
        return Some(1_000_000);
    }
    let five = ["claude-opus-5", "claude-sonnet-5", "claude-fable-5"];
    if five.iter().any(|f| id.starts_with(f)) {
        Some(1_000_000)
    } else if id.starts_with("claude-haiku-4-5") {
        Some(200_000)
    } else {
        None
    }
}

/// A token count as a byline spells it: `1M`, `200k`, or rounded to
/// thousands (`258k` for Codex's 258400), the bare count below that.
pub(crate) fn token_size(tokens: u64) -> String {
    if tokens >= 1_000_000 && tokens.is_multiple_of(1_000_000) {
        format!("{}M", tokens / 1_000_000)
    } else if tokens >= 1_000 {
        format!("{}k", (tokens + 500) / 1_000)
    } else {
        tokens.to_string()
    }
}

/// Claude Code caches the model catalogue it fetched under
/// `cache/model-catalog/`, one file per account or configuration. The newest
/// file that lists models answers: a file that is part-way through being
/// rewritten, or one another surface wrote, has none and is skipped.
pub(crate) fn claude_catalogue() -> Result<Catalogued> {
    let dir = harness_dir("CLAUDE_CONFIG_DIR", ".claude")
        .context("no home directory for claude's model catalog")?
        .join("cache/model-catalog");
    let mut newest: Option<(i64, Catalogued)> = None;
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
            .is_none_or(|(newest, _)| fetched_at > *newest)
        {
            newest = Some((
                fetched_at,
                Catalogued {
                    path,
                    models,
                    // When this file goes stale, in its own words: it is what
                    // tells `available` whether to start the CLI again (#442).
                    stale_at: catalog.get("staleAt").and_then(serde_json::Value::as_i64),
                },
            ));
        }
    }
    let (_, catalogued) = newest.context("no claude model catalog on this machine")?;
    Ok(catalogued)
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

pub(crate) const THINKING_LEVELS: &[&str] =
    &["off", "minimal", "low", "medium", "high", "xhigh", "max"];

pub fn catalogue(harness: &str) -> Option<&'static Catalogue> {
    crate::harness::harness(harness)?.catalogue.as_ref()
}

pub fn supports_model(harness: &str) -> bool {
    catalogue(harness).is_some()
}

/// What ssf says when asked for a harness's model ids and there are none to
/// give, because it is not a harness that takes the setting at all. One
/// wording for the command and the web endpoint that answer the same
/// question.
pub fn no_model_setting(harness: &str) -> String {
    format!("{harness} does not take a model setting")
}

/// The ids the built-in table seeds for `harness`.
fn seeded_models(cat: &Catalogue) -> Vec<String> {
    cat.models.iter().map(|m| (*m).to_string()).collect()
}

/// Model ids ssf can offer without asking the installed agent: the catalogue
/// that agent wrote on this machine when there is one, else the seeded ids.
/// A harness that lists models only when asked has none here; `ssf models`
/// asks it. Reading only, deliberately: this is what `ssf agents` prints, and
/// what a configuration's model is checked against, so it never starts an
/// agent the way `available` may (#442).
pub fn known_models(harness: &str) -> Vec<String> {
    let Some(cat) = catalogue(harness) else {
        return Vec::new();
    };
    match catalogue_of(cat) {
        Some(own) => own.models,
        None => seeded_models(cat),
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
pub enum Compaction {
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

/// How `harness` takes a context-compaction threshold, when it takes one at
/// all.
fn auto_compaction(harness: &str) -> Option<&'static Compaction> {
    crate::harness::harness(harness)?.auto_compaction.as_ref()
}

/// The counts `harness` takes, when it caps them: Claude Code refuses to start
/// outside its own window.
fn auto_compaction_range(harness: &str) -> Option<std::ops::RangeInclusive<u64>> {
    match auto_compaction(harness) {
        Some(Compaction::Args { tokens, .. }) => tokens.clone(),
        _ => None,
    }
}

/// Arguments that give `harness` the context-compaction threshold `tokens`:
/// none for `0` (leave the harness's own default alone), none for a harness
/// that takes it another way, and none for a count outside the range the
/// harness accepts — dropped the way `launch_args` drops an effort level the
/// harness does not take. Every boundary that writes or loads a count has
/// already refused such a value out loud (`validate_auto_compaction`); the one
/// that can still reach a launch is a stored per-item override written before
/// the count was checked, and a session that starts on its harness's own
/// default is worth more than one that never comes up.
pub fn auto_compaction_args(harness: &str, tokens: u64) -> Vec<String> {
    if tokens == 0 {
        return Vec::new();
    }
    match auto_compaction(harness) {
        Some(Compaction::Args { args, .. })
            if auto_compaction_range(harness).is_none_or(|r| r.contains(&tokens)) =>
        {
            args(tokens)
        }
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
    if let Some(range) = auto_compaction_range(harness)
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

/// OMP's normal five-minute inter-event watchdog can end a healthy, long
/// reasoning turn. It retries before any visible output, but deliberately
/// stops after partial output because replay could duplicate work. Factory
/// sessions are unattended, so give them the longer timeout OMP recommends
/// for this workload (#322). A configured `repo.command` remains authoritative.
const OMP_DEFAULT_COMMAND: &str = "PI_STREAM_IDLE_TIMEOUT_MS=900000 \"$SSF_PI_LAUNCHER\" omp --auto-approve -e \"$SSF_PI_BRIDGE\"";

/// The flags that let `harness` run unattended, if ssf knows them.
pub fn unattended_flags(harness: &str) -> Option<&'static str> {
    crate::harness::harness(harness)?.unattended_flags
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
///
/// A harness that cannot be asked for its models at all (`claude`: it has no
/// such command) is started to rewrite the catalogue it keeps, when that
/// catalogue is missing or past the harness's own staleness stamp, so the list
/// is the harness's own and current rather than ssf's table (#442). That is
/// the only place an agent is started to answer: `ssf agents`, configuration
/// loading and every other reader go on taking the file as they find it.
pub fn available(harness: &str) -> Result<Available> {
    let Some(cat) = catalogue(harness) else {
        bail!(crate::ipc::Refused::bad_input(no_model_setting(harness)));
    };
    let mut own = catalogue_of(cat);
    if needs_refresh(own.as_ref(), now_ms())
        && let Some(refresh) = cat.refresh
    {
        refresh();
        own = catalogue_of(cat);
    }
    // A catalogue that will not read, or lists nothing, is no answer at all:
    // the table is better than an empty list, and `source` says which one
    // the ids below came from.
    let mut source = Source::table();
    let mut models = Vec::new();
    if let Some(own) = own {
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
                crate::harness::HARNESSES
                    .iter()
                    .filter(|h| h.catalogue.is_some())
                    .map(|h| h.id)
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
                crate::harness::HARNESSES
                    .iter()
                    .filter(|h| !effort_levels(h.id).is_empty())
                    .map(|h| h.id)
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
    fn grok_models_are_read_from_the_available_list() {
        let listing = "You are not authenticated.\n\nDefault model: grok-4.6\n\nAvailable models:\n  * grok-4.6 (default)\n  - grok-4.5\n";
        assert_eq!(parse_grok_models(listing), ["grok-4.6", "grok-4.5"]);
        assert!(parse_grok_models("error: something\n- not-a-model\n").is_empty());
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
    fn omp_context_window_is_the_listed_models() {
        let listing = r#"{"models":[{"provider":"deepseek","id":"deepseek-flash","contextWindow":1000000},{"provider":"other","id":"deepseek-flash","contextWindow":64000},{"provider":"x","id":"none"}]}"#;
        assert_eq!(
            omp_window_in(listing, "deepseek", "deepseek-flash"),
            Some(1_000_000)
        );
        assert_eq!(
            omp_window_in(listing, "other", "deepseek-flash"),
            Some(64_000)
        );
        assert_eq!(omp_window_in(listing, "x", "none"), None);
        assert_eq!(omp_window_in(listing, "deepseek", "missing"), None);

        // Cached: a fresh listing answers without a refresh; a model it does
        // not list goes without, and waits a few minutes before asking again.
        let sandbox = crate::config::test_support::sandbox();
        let path = sandbox.root().join("omp-models.json");
        assert_eq!(
            cached_window(&path, &OMP_LISTING, "deepseek", "deepseek-flash"),
            (None, true)
        );
        std::fs::write(&path, listing).unwrap();
        assert_eq!(
            cached_window(&path, &OMP_LISTING, "deepseek", "deepseek-flash"),
            (Some(1_000_000), false)
        );
        assert_eq!(
            cached_window(&path, &OMP_LISTING, "deepseek", "missing"),
            (None, false)
        );
        // A day-old listing still answers, and is refreshed.
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(25 * 60 * 60);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(old)
            .unwrap();
        assert_eq!(
            cached_window(&path, &OMP_LISTING, "deepseek", "deepseek-flash"),
            (Some(1_000_000), true)
        );
        assert_eq!(
            cached_window(&path, &OMP_LISTING, "deepseek", "missing"),
            (None, true)
        );
    }

    #[test]
    fn pi_context_window_is_the_listed_models() {
        let table = "provider    model          context  max-out\nopenrouter  ~anthropic/claude-opus-latest  1M  128K\nopenrouter  moonshotai/kimi-k2.6  262.1K  32K\nanthropic   claude-sonnet-4  200K  64K\nx  bad  ?  1K\n";
        assert_eq!(
            pi_window_in(table, "openrouter", "~anthropic/claude-opus-latest"),
            Some(1_000_000)
        );
        assert_eq!(
            pi_window_in(table, "openrouter", "moonshotai/kimi-k2.6"),
            Some(262_100)
        );
        assert_eq!(
            pi_window_in(table, "anthropic", "claude-sonnet-4"),
            Some(200_000)
        );
        assert_eq!(pi_window_in(table, "openrouter", "claude-sonnet-4"), None);
        assert_eq!(pi_window_in(table, "x", "bad"), None);
        // The header row is not a model.
        assert_eq!(pi_window_in(table, "provider", "model"), None);

        let sandbox = crate::config::test_support::sandbox();
        let path = sandbox.root().join("pi-models.txt");
        std::fs::write(&path, table).unwrap();
        assert_eq!(
            cached_window(&path, &PI_LISTING, "anthropic", "claude-sonnet-4"),
            (Some(200_000), false)
        );
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
        // A count the harness refuses is dropped rather than shelled into a
        // launch that would not come up: this is the one way a count that was
        // never checked can still reach a launch, a stored per-item override
        // written before it was checked.
        assert!(auto_compaction_args("claude", 50_000).is_empty());
        assert!(auto_compaction_args("claude", 1_000_001).is_empty());
        assert_eq!(
            apply_to_command("claude", "claude", Some("opus"), None, 50_000),
            "claude --model opus"
        );
        // Codex caps nothing of its own, so any count reaches it.
        assert!(!auto_compaction_args("codex", 50_000).is_empty());
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

    /// A catalogue the harness's own stamp calls stale is not the list to
    /// answer with: the harness is started to rewrite it, and what it wrote
    /// answers. This is the case #442 opened with -- a model released after
    /// this machine's Claude Code last ran.
    #[test]
    fn a_stale_catalogue_is_refreshed_and_the_rewritten_one_answers() {
        let sandbox = crate::config::test_support::sandbox();
        let dir = sandbox.home().join(".claude/cache/model-catalog");
        write(
            &dir.join("stale.json"),
            r#"{"fetchedAt":10,"staleAt":20,"catalog":{"surface":"cc","config":{"models":[{"id":"claude-opus-4-6"}]}}}"#,
        );
        // What the harness does when it is started: fetch the catalogue it
        // keeps and write it again, with the model released since. The guard
        // takes the stand-in away again when this test ends, so no other test
        // on this thread runs it.
        let rewritten = dir.clone();
        let _started = on_start(move |_| {
            write(
                &rewritten.join("fresh.json"),
                r#"{"fetchedAt":30,"staleAt":9999999999999,"catalog":{"surface":"cc","config":{"models":[{"id":"claude-opus-5-5"}]}}}"#,
            );
        });
        let available = available("claude").unwrap();
        assert_eq!(started(), ["claude"]);
        assert_eq!(available.models, vec!["claude-opus-5-5"]);
        assert!(
            available.source.detail.ends_with("fresh.json"),
            "{}",
            available.source.detail
        );
    }

    /// A machine that has never run the harness has no catalogue to read, and
    /// no ids but ssf's own: the harness is started once to write one, and the
    /// table answers when it will not -- no login, no network, no CLI at all.
    #[test]
    fn a_missing_catalogue_is_refreshed_and_the_table_answers() {
        let _sandbox = crate::config::test_support::sandbox();
        // No stand-in of this test's: a start is bare here, so it leaves
        // nothing behind and the table below is what ssf has to answer with.
        let available = available("claude").unwrap();
        assert_eq!(started(), ["claude"]);
        assert_eq!(available.models, vec!["fable", "opus", "sonnet", "haiku"]);
        assert_eq!(available.source.kind, SourceKind::Table);
        assert_eq!(
            available.source.describe("claude"),
            "ssf's built-in table for claude"
        );
    }

    /// A catalogue the harness still calls current is the list, with nothing
    /// started for it. ssf lists models on every form render, and starting the
    /// CLI for a list it already has would make a read cost a process.
    #[test]
    fn a_catalogue_that_is_still_current_is_not_refreshed() {
        let sandbox = crate::config::test_support::sandbox();
        write(
            &sandbox.home().join(".claude/cache/model-catalog/cc.json"),
            r#"{"fetchedAt":10,"staleAt":9999999999999,"catalog":{"surface":"cc","config":{"models":[{"id":"claude-opus-5"}]}}}"#,
        );
        let available = available("claude").unwrap();
        assert_eq!(available.models, vec!["claude-opus-5"]);
        assert!(started().is_empty());
    }

    /// A hook stands in for a start only while the test that installed it holds
    /// the guard: libtest reuses its worker threads, so one left behind would
    /// run inside the next test on that thread, writing the catalogue it means
    /// to write into a sandbox that test has already dropped.
    #[test]
    fn a_start_is_stubbed_only_while_its_test_holds_the_guard() {
        let _sandbox = crate::config::test_support::sandbox();
        let ran = std::rc::Rc::new(std::cell::Cell::new(false));
        let mark = std::rc::Rc::clone(&ran);
        drop(on_start(move |_| mark.set(true)));
        let available = available("claude").unwrap();
        assert!(!ran.get(), "a start ran a hook an earlier test had left");
        assert_eq!(started(), ["claude"]);
        assert_eq!(available.source.kind, SourceKind::Table);
    }

    /// Whether the harness has to be started: nothing on the machine is
    /// nothing to answer with, the harness's own stamp decides once there is,
    /// and a file that says nothing about going stale is taken as current --
    /// codex's cache, which has no such stamp, is never a reason to start an
    /// agent.
    #[test]
    fn the_stamp_decides_whether_the_harness_is_started() {
        // No stamp: current. The exact instant of the stamp is stale.
        let unstamped = Catalogued {
            path: PathBuf::from("/c"),
            models: vec!["m".into()],
            stale_at: None,
        };
        assert!(!needs_refresh(Some(&unstamped), 1_000));
        assert!(needs_refresh(None, 1_000));
        let stamped = |stale_at| Catalogued {
            path: PathBuf::from("/c"),
            models: vec!["m".into()],
            stale_at: Some(stale_at),
        };
        assert!(!needs_refresh(Some(&stamped(1_001)), 1_000));
        assert!(needs_refresh(Some(&stamped(1_000)), 1_000));
        assert!(needs_refresh(Some(&stamped(999)), 1_000));
    }

    /// `ssf agents` and everything else that reads without asking -- a
    /// configuration's model check among them -- never starts a harness, so a
    /// stale catalogue answers as it stands instead of costing a process.
    #[test]
    fn a_reading_caller_never_starts_a_harness() {
        let sandbox = crate::config::test_support::sandbox();
        write(
            &sandbox.home().join(".claude/cache/model-catalog/cc.json"),
            r#"{"fetchedAt":10,"staleAt":20,"catalog":{"surface":"cc","config":{"models":[{"id":"claude-opus-4-6"}]}}}"#,
        );
        assert_eq!(known_models("claude"), vec!["claude-opus-4-6"]);
        assert!(started().is_empty());
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
