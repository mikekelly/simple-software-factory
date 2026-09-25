//! What is left of each harness's provider allowance (#514): the plan's
//! five-hour and weekly windows, or an account balance.
//!
//! Each harness's own stored credential is read, never written: ssf does not
//! refresh a token, so an expired one or a provider's 401 keeps the last
//! answer, marked stale, until the harness runs again and refreshes it
//! itself. An answer is kept for five minutes per harness and provider in
//! `usage.json` under the state directory, which holds the numbers alone.
//! Provider bodies carry the account's email and ids, so they are parsed
//! here and never logged.

use crate::login::credential_path;
use anyhow::Result;
use chrono::{DateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long one provider's answer is used before it is asked again.
const FRESH_SECS: i64 = 300;
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
/// Said beside numbers ssf could not bring up to date because the stored
/// token has expired or was refused.
const STALE_NOTE: &str = "refreshes on the harness's next run";

/// The harnesses whose credentials ssf knows how to ask about.
const HARNESSES: &[&str] = &["claude", "codex", "omp", "pi", "opencode", "grok"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Provider {
    /// Claude Pro/Max OAuth.
    Anthropic,
    /// ChatGPT (Codex) OAuth.
    ChatGpt,
    DeepSeek,
    OpenRouter,
}

impl Provider {
    fn id(self) -> &'static str {
        match self {
            Provider::Anthropic => "anthropic",
            Provider::ChatGpt => "chatgpt",
            Provider::DeepSeek => "deepseek",
            Provider::OpenRouter => "openrouter",
        }
    }
}

/// One stored credential, as read from a harness's own file.
struct Credential {
    provider: Provider,
    secret: String,
    /// ChatGPT's account id, sent beside its token.
    account: Option<String>,
    /// When the token expires, in epoch milliseconds, where the file says.
    expires_ms: Option<i64>,
    /// omp's row id, which tells apart two enabled credentials for one
    /// provider; not a secret.
    row: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Window {
    /// `5h` or `week`.
    pub label: String,
    /// How much of the window is used, 0 to 100; `None` when not given.
    pub used_percent: Option<f64>,
    pub resets_at: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Balance {
    pub currency: String,
    pub amount: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub windows: Vec<Window>,
    #[serde(default)]
    pub balances: Vec<Balance>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Account {
    pub provider: &'static str,
    /// omp's credential row, given when the harness has more than one
    /// credential for this provider.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub row: Option<i64>,
    /// `ok`, `stale` (the last known numbers; see `note`) or `unavailable`.
    pub state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fetched_at: Option<String>,
    #[serde(flatten)]
    pub usage: Usage,
}

#[derive(Debug, Clone, Serialize)]
pub struct HarnessUsage {
    pub harness: &'static str,
    pub accounts: Vec<Account>,
    /// Why there is nothing to show, when there is nothing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Cached {
    fetched_at: i64,
    usage: Usage,
}

type Cache = BTreeMap<String, Cached>;

/// What one request came back with.
#[derive(Debug)]
enum Outcome {
    Fresh(Usage),
    /// The token has expired or was refused: only the harness can renew it.
    Expired,
    Failed(String),
}

/// Every harness ssf can ask about, with what its credentials show. Every
/// provider is asked at once, so the slowest bounds the answer.
pub async fn report() -> Result<Vec<HarnessUsage>> {
    let path = crate::config::state_dir().join("usage.json");
    let mut cache: Cache = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    let now = Utc::now();
    let client = reqwest::Client::builder().timeout(HTTP_TIMEOUT).build()?;
    let wanted: Vec<(&str, Credential)> = HARNESSES
        .iter()
        .flat_map(|&harness| credentials(harness).into_iter().map(move |c| (harness, c)))
        .collect();
    let outcomes = futures_util::future::join_all(wanted.iter().map(|(harness, credential)| {
        let cached = cache.get(&key(harness, credential));
        let fresh = cached.is_some_and(|c| now.timestamp() - c.fetched_at < FRESH_SECS);
        let expired = credential
            .expires_ms
            .is_some_and(|e| e <= now.timestamp_millis());
        let client = &client;
        async move {
            if fresh {
                None
            } else if expired {
                Some(Outcome::Expired)
            } else {
                Some(fetch(client, credential).await)
            }
        }
    }))
    .await;
    let mut out: Vec<HarnessUsage> = HARNESSES
        .iter()
        .map(|&harness| HarnessUsage {
            harness,
            accounts: Vec::new(),
            note: None,
        })
        .collect();
    let mut changed = false;
    for ((harness, credential), outcome) in wanted.iter().zip(outcomes) {
        let key = key(harness, credential);
        let cached = cache.get(&key).cloned();
        if let Some(Outcome::Fresh(usage)) = &outcome {
            let fetched_at = now.timestamp();
            let usage = usage.clone();
            cache.insert(key, Cached { fetched_at, usage });
            changed = true;
        }
        let mut account = resolve(credential.provider, cached.as_ref(), outcome, now);
        let shared = wanted
            .iter()
            .filter(|(h, c)| h == harness && c.provider == credential.provider)
            .count();
        if shared > 1 {
            account.row = credential.row;
        }
        if let Some(entry) = out.iter_mut().find(|h| h.harness == *harness) {
            entry.accounts.push(account);
        }
    }
    for entry in &mut out {
        entry.note = match (entry.harness, entry.accounts.is_empty()) {
            // xAI documents no usage or balance request a signed-in grok
            // credential can make.
            ("grok", _) => Some("no usage data".to_string()),
            (_, true) => Some("unavailable".to_string()),
            _ => None,
        };
    }
    if changed {
        // Only the parsed numbers are kept; losing the file costs one fetch.
        let _ = std::fs::create_dir_all(path.parent().unwrap_or(Path::new(".")));
        let _ = std::fs::write(&path, serde_json::to_string(&cache)?);
    }
    Ok(out)
}

fn key(harness: &str, credential: &Credential) -> String {
    match credential.row {
        Some(row) => format!("{harness}:{}:{row}", credential.provider.id()),
        None => format!("{harness}:{}", credential.provider.id()),
    }
}

/// One account's answer from its cache entry and what was just asked, if
/// anything was: fresh numbers, the last numbers marked stale, or
/// `unavailable` when there are none.
fn resolve(
    provider: Provider,
    cached: Option<&Cached>,
    outcome: Option<Outcome>,
    now: DateTime<Utc>,
) -> Account {
    let at = |secs: i64| Utc.timestamp_opt(secs, 0).single().map(|t| t.to_rfc3339());
    let (state, note, fetched_at, usage) = match (outcome, cached) {
        (Some(Outcome::Fresh(usage)), _) => ("ok", None, at(now.timestamp()), usage),
        (None, Some(c)) => ("ok", None, at(c.fetched_at), c.usage.clone()),
        (Some(Outcome::Expired), Some(c)) => (
            "stale",
            Some(STALE_NOTE.to_string()),
            at(c.fetched_at),
            c.usage.clone(),
        ),
        (Some(Outcome::Failed(why)), Some(c)) => {
            ("stale", Some(why), at(c.fetched_at), c.usage.clone())
        }
        (Some(Outcome::Expired), None) => (
            "unavailable",
            Some(format!("token expired; {STALE_NOTE}")),
            None,
            Usage::default(),
        ),
        (Some(Outcome::Failed(why)), None) => ("unavailable", Some(why), None, Usage::default()),
        (None, None) => ("unavailable", None, None, Usage::default()),
    };
    Account {
        provider: provider.id(),
        row: None,
        state,
        note,
        fetched_at,
        usage,
    }
}

async fn fetch(client: &reqwest::Client, credential: &Credential) -> Outcome {
    let request = match credential.provider {
        Provider::Anthropic => client
            .get("https://api.anthropic.com/api/oauth/usage")
            .header("anthropic-beta", "oauth-2025-04-20"),
        Provider::ChatGpt => {
            let request = client.get("https://chatgpt.com/backend-api/wham/usage");
            match &credential.account {
                Some(account) => request.header("ChatGPT-Account-Id", account),
                None => request,
            }
        }
        Provider::DeepSeek => client.get("https://api.deepseek.com/user/balance"),
        Provider::OpenRouter => client.get("https://openrouter.ai/api/v1/credits"),
    }
    .bearer_auth(&credential.secret)
    .header(reqwest::header::USER_AGENT, "ssf");
    let response = match request.send().await {
        Ok(response) => response,
        Err(_) => return Outcome::Failed("could not reach the provider".into()),
    };
    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Outcome::Expired;
    }
    if !status.is_success() {
        tracing::debug!(provider = credential.provider.id(), %status, "usage request refused");
        return Outcome::Failed(format!("the provider answered {}", status.as_u16()));
    }
    match response.json::<Value>().await {
        Ok(body) => Outcome::Fresh(parse(credential.provider, &body)),
        Err(_) => Outcome::Failed("the provider's answer could not be read".into()),
    }
}

fn parse(provider: Provider, body: &Value) -> Usage {
    match provider {
        Provider::Anthropic => parse_anthropic(body),
        Provider::ChatGpt => parse_chatgpt(body),
        Provider::DeepSeek => parse_deepseek(body),
        Provider::OpenRouter => parse_openrouter(body),
    }
}

/// `five_hour` and `seven_day`, each `{utilization, resets_at}`.
fn parse_anthropic(body: &Value) -> Usage {
    let window = |key: &str, label: &str| {
        let w = body.get(key).filter(|w| w.is_object())?;
        Some(Window {
            label: label.into(),
            used_percent: w.get("utilization").and_then(Value::as_f64),
            resets_at: w
                .get("resets_at")
                .and_then(Value::as_str)
                .map(str::to_string),
        })
    };
    Usage {
        windows: [window("five_hour", "5h"), window("seven_day", "week")]
            .into_iter()
            .flatten()
            .collect(),
        balances: Vec::new(),
    }
}

/// `rate_limit.primary_window`/`secondary_window`, either of which may be
/// null, told apart by their length rather than their position; and the
/// credit balance where the account has credits.
fn parse_chatgpt(body: &Value) -> Usage {
    let mut windows: Vec<(i64, Window)> = ["primary_window", "secondary_window"]
        .iter()
        .filter_map(|key| body.pointer(&format!("/rate_limit/{key}")))
        .filter(|w| w.is_object())
        .map(|w| {
            let seconds = w
                .get("limit_window_seconds")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            let label = if seconds >= 24 * 3600 {
                "week".to_string()
            } else if seconds > 0 {
                format!("{}h", (seconds + 1800) / 3600)
            } else {
                "window".to_string()
            };
            let resets_at = w
                .get("reset_at")
                .and_then(Value::as_i64)
                .and_then(|s| Utc.timestamp_opt(s, 0).single())
                .map(|t| t.to_rfc3339());
            (
                seconds,
                Window {
                    label,
                    used_percent: w.get("used_percent").and_then(Value::as_f64),
                    resets_at,
                },
            )
        })
        .collect();
    windows.sort_by_key(|(seconds, _)| *seconds);
    let balances = body
        .get("credits")
        .filter(|c| c.get("has_credits").and_then(Value::as_bool) == Some(true))
        .and_then(|c| c.get("balance"))
        .and_then(amount)
        .map(|amount| Balance {
            currency: "credits".into(),
            amount,
        })
        .into_iter()
        .collect();
    Usage {
        windows: windows.into_iter().map(|(_, w)| w).collect(),
        balances,
    }
}

/// `balance_infos[]`, each `{currency, total_balance}`.
fn parse_deepseek(body: &Value) -> Usage {
    let balances = body
        .get("balance_infos")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|b| {
            Some(Balance {
                currency: b.get("currency")?.as_str()?.to_string(),
                amount: amount(b.get("total_balance")?)?,
            })
        })
        .collect();
    Usage {
        windows: Vec::new(),
        balances,
    }
}

/// `data.total_credits` less `data.total_usage`, in dollars.
fn parse_openrouter(body: &Value) -> Usage {
    let left = body.get("data").and_then(|d| {
        let total = d.get("total_credits")?.as_f64()?;
        let used = d.get("total_usage")?.as_f64()?;
        Some(format!("{:.2}", total - used))
    });
    Usage {
        windows: Vec::new(),
        balances: left
            .map(|amount| Balance {
                currency: "USD".into(),
                amount,
            })
            .into_iter()
            .collect(),
    }
}

/// A money amount, given as a string or a number, to two places.
fn amount(value: &Value) -> Option<String> {
    let n = match value {
        Value::String(s) => s.trim().parse::<f64>().ok()?,
        other => other.as_f64()?,
    };
    Some(format!("{n:.2}"))
}

/// The credentials a harness has stored that ssf can ask a provider about.
/// A file that is missing or not understood gives none.
fn credentials(harness: &str) -> Vec<Credential> {
    let Some(path) = credential_path(harness) else {
        return Vec::new();
    };
    match harness {
        "claude" => read_json(&path)
            .and_then(|v| claude_credential(&v))
            .into_iter()
            .collect(),
        "codex" => read_json(&path)
            .and_then(|v| codex_credential(&v))
            .into_iter()
            .collect(),
        // Once omp has migrated to agent.db it is authoritative, as in
        // `login::probe_omp_at`; before that, its auth.json is pi's shape.
        "omp" if path.exists() => omp_rows(&path)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(row, provider, data)| {
                let mut credential = keyed_credential(&provider, &data)?;
                credential.row = Some(row);
                Some(credential)
            })
            .collect(),
        "omp" => keyed_file(&path.with_file_name("auth.json")),
        "pi" | "opencode" => keyed_file(&path),
        _ => Vec::new(),
    }
}

/// pi's, opencode's and omp's legacy `auth.json`: entries by provider id.
fn keyed_file(path: &PathBuf) -> Vec<Credential> {
    read_json(path)
        .and_then(|v| v.as_object().cloned())
        .into_iter()
        .flatten()
        .filter_map(|(provider, data)| keyed_credential(&provider, &data))
        .collect()
}

fn read_json(path: &PathBuf) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

fn text(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)?
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// `claudeAiOauth.{accessToken, expiresAt}` in `~/.claude/.credentials.json`.
fn claude_credential(file: &Value) -> Option<Credential> {
    let oauth = file.get("claudeAiOauth")?;
    Some(Credential {
        provider: Provider::Anthropic,
        secret: text(oauth, "accessToken")?,
        account: None,
        expires_ms: oauth.get("expiresAt").and_then(Value::as_i64),
        row: None,
    })
}

/// `tokens.{access_token, account_id}` in `~/.codex/auth.json`.
fn codex_credential(file: &Value) -> Option<Credential> {
    let tokens = file.get("tokens")?;
    Some(Credential {
        provider: Provider::ChatGpt,
        secret: text(tokens, "access_token")?,
        account: text(tokens, "account_id"),
        expires_ms: None,
        row: None,
    })
}

/// One entry of pi's or opencode's `auth.json`, or one of omp's rows,
/// keyed by the harness's provider id: OAuth entries hold
/// `{access, expires, accountId}`, key entries `{key}`.
fn keyed_credential(provider: &str, data: &Value) -> Option<Credential> {
    let provider = match provider {
        "anthropic" => Provider::Anthropic,
        // pi and omp call ChatGPT sign-in `openai-codex`; opencode `openai`.
        "openai-codex" | "openai" => Provider::ChatGpt,
        "deepseek" => Provider::DeepSeek,
        "openrouter" => Provider::OpenRouter,
        _ => return None,
    };
    let oauth = data.get("access").is_some();
    // An Anthropic or OpenAI API key has no plan windows to show.
    if !oauth && matches!(provider, Provider::Anthropic | Provider::ChatGpt) {
        return None;
    }
    Some(Credential {
        provider,
        secret: text(data, if oauth { "access" } else { "key" })?,
        account: text(data, "accountId"),
        expires_ms: data.get("expires").and_then(Value::as_i64),
        row: None,
    })
}

/// omp's enabled credentials, as `(row id, provider, data)`, read from `agent.db`
/// opened read-only.
fn omp_rows(path: &Path) -> rusqlite::Result<Vec<(i64, String, Value)>> {
    let db = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    db.busy_timeout(Duration::from_millis(250))?;
    let mut query = db.prepare(
        "SELECT id, provider, data FROM auth_credentials
         WHERE disabled_cause IS NULL AND json_valid(data)",
    )?;
    let rows = query
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .filter_map(|row| row.ok())
        .filter_map(|(id, provider, data)| Some((id, provider, serde_json::from_str(&data).ok()?)))
        .collect();
    Ok(rows)
}

/// The report as `ssf usage` prints it, one harness a line:
/// `claude · 5h 42% (resets 16:10) · week 18%`, `omp · deepseek USD 12.40`.
pub fn lines(report: &[HarnessUsage]) -> Vec<String> {
    report
        .iter()
        .map(|h| {
            let mut words = vec![h.harness.to_string()];
            let several = h.accounts.len() > 1;
            for account in &h.accounts {
                words.extend(account_words(account, several));
            }
            words.extend(h.note.clone());
            words.join(" \u{b7} ")
        })
        .collect()
}

fn account_words(account: &Account, named: bool) -> Vec<String> {
    let mut words: Vec<String> = account
        .usage
        .windows
        .iter()
        .map(|w| {
            let used = w
                .used_percent
                .map_or("unavailable".to_string(), |p| format!("{p:.0}%"));
            let reset = w
                .resets_at
                .as_deref()
                .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
                .map(|t| {
                    // A weekly reset days away needs its day, not just its time.
                    let far =
                        t.signed_duration_since(chrono::Utc::now()) > chrono::Duration::hours(24);
                    let format = if far { "%a %H:%M" } else { "%H:%M" };
                    format!(
                        " (resets {})",
                        t.with_timezone(&chrono::Local).format(format)
                    )
                })
                .unwrap_or_default();
            format!("{} {used}{reset}", w.label)
        })
        .chain(
            account
                .usage
                .balances
                .iter()
                .map(|b| match b.currency.as_str() {
                    "USD" => format!("${}", b.amount),
                    other => format!("{} {other}", b.amount),
                }),
        )
        .collect();
    if words.is_empty() {
        words.push("unavailable".into());
    }
    if named {
        let name = match account.row {
            Some(row) => format!("{} #{row}", account.provider),
            None => account.provider.to_string(),
        };
        words[0] = format!("{name} {}", words[0]);
    }
    if account.state == "stale" {
        let why = account.note.as_deref().unwrap_or(STALE_NOTE);
        words.push(format!("stale, {why}"));
    } else if account.state == "unavailable"
        && let Some(note) = &account.note
    {
        words.push(note.clone());
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_anthropic_windows() {
        let usage = parse_anthropic(&json!({
            "five_hour": {"utilization": 42.0, "resets_at": "2026-09-25T16:10:00+00:00"},
            "seven_day": {"utilization": 18, "resets_at": null},
            "seven_day_opus": null,
        }));
        assert_eq!(usage.windows.len(), 2);
        assert_eq!(usage.windows[0].label, "5h");
        assert_eq!(usage.windows[0].used_percent, Some(42.0));
        assert_eq!(
            usage.windows[0].resets_at.as_deref(),
            Some("2026-09-25T16:10:00+00:00")
        );
        assert_eq!(usage.windows[1].label, "week");
        assert_eq!(usage.windows[1].used_percent, Some(18.0));
        assert_eq!(usage.windows[1].resets_at, None);
        // Nothing known is nothing shown, not an error.
        assert_eq!(parse_anthropic(&json!({})), Usage::default());
    }

    /// The weekly window may come first or alone; its length says which.
    #[test]
    fn reads_chatgpt_windows_by_length() {
        let usage = parse_chatgpt(&json!({
            "rate_limit": {
                "primary_window": {"used_percent": 7, "limit_window_seconds": 604800, "reset_at": 1790000000},
                "secondary_window": {"used_percent": 55.5, "limit_window_seconds": 18000, "reset_at": 1789000000},
            },
            "credits": {"has_credits": true, "balance": "12.4"},
        }));
        let labels: Vec<_> = usage.windows.iter().map(|w| w.label.as_str()).collect();
        assert_eq!(labels, ["5h", "week"]);
        assert_eq!(usage.windows[0].used_percent, Some(55.5));
        assert!(
            usage.windows[1]
                .resets_at
                .as_deref()
                .unwrap()
                .starts_with("2026-")
        );
        assert_eq!(usage.balances[0].amount, "12.40");

        let usage = parse_chatgpt(&json!({
            "rate_limit": {"primary_window": null, "secondary_window": {"used_percent": 3, "limit_window_seconds": 604800}},
            "credits": {"has_credits": false, "balance": "0"},
        }));
        assert_eq!(usage.windows.len(), 1);
        assert_eq!(usage.windows[0].label, "week");
        assert!(usage.balances.is_empty());
    }

    #[test]
    fn reads_balances() {
        let usage = parse_deepseek(&json!({
            "is_available": true,
            "balance_infos": [{"currency": "USD", "total_balance": "12.4", "granted_balance": "0"}],
        }));
        assert_eq!(
            usage.balances,
            [Balance {
                currency: "USD".into(),
                amount: "12.40".into()
            }]
        );
        let usage = parse_openrouter(&json!({"data": {"total_credits": 20, "total_usage": 7.25}}));
        assert_eq!(usage.balances[0].amount, "12.75");
        assert_eq!(parse_openrouter(&json!({"data": {}})), Usage::default());
    }

    #[test]
    fn reads_each_harness_credential_shape() {
        let claude =
            claude_credential(&json!({"claudeAiOauth": {"accessToken": "t", "expiresAt": 5}}))
                .unwrap();
        assert_eq!(claude.provider, Provider::Anthropic);
        assert_eq!(claude.expires_ms, Some(5));
        let codex =
            codex_credential(&json!({"tokens": {"access_token": "t", "account_id": "a"}})).unwrap();
        assert_eq!(codex.provider, Provider::ChatGpt);
        assert_eq!(codex.account.as_deref(), Some("a"));
        let omp = keyed_credential(
            "openai-codex",
            &json!({"access": "t", "expires": 9, "accountId": "a"}),
        )
        .unwrap();
        assert_eq!(omp.provider, Provider::ChatGpt);
        assert_eq!(
            (omp.account.as_deref(), omp.expires_ms),
            (Some("a"), Some(9))
        );
        let opencode =
            keyed_credential("openai", &json!({"type": "oauth", "access": "t"})).unwrap();
        assert_eq!(opencode.provider, Provider::ChatGpt);
        let key = keyed_credential("deepseek", &json!({"type": "api_key", "key": "k"})).unwrap();
        assert_eq!(key.provider, Provider::DeepSeek);
        assert!(keyed_credential("openrouter", &json!({"type": "api", "key": "k"})).is_some());
        // An API key for a plan provider has no windows; an unknown
        // provider is not asked.
        assert!(keyed_credential("anthropic", &json!({"type": "api_key", "key": "k"})).is_none());
        assert!(keyed_credential("xai", &json!({"key": "k"})).is_none());
        assert!(claude_credential(&json!({})).is_none());
    }

    #[test]
    fn prints_one_line_a_harness() {
        let report = vec![
            HarnessUsage {
                harness: "claude",
                accounts: vec![Account {
                    provider: "anthropic",
                    row: None,
                    state: "ok",
                    note: None,
                    fetched_at: None,
                    usage: Usage {
                        windows: vec![
                            Window {
                                label: "5h".into(),
                                used_percent: Some(42.0),
                                resets_at: None,
                            },
                            Window {
                                label: "week".into(),
                                used_percent: Some(18.4),
                                resets_at: None,
                            },
                        ],
                        balances: Vec::new(),
                    },
                }],
                note: None,
            },
            HarnessUsage {
                harness: "grok",
                accounts: Vec::new(),
                note: Some("no usage data".into()),
            },
        ];
        assert_eq!(
            lines(&report),
            [
                "claude \u{b7} 5h 42% \u{b7} week 18%",
                "grok \u{b7} no usage data"
            ]
        );
    }

    /// Before omp moved to agent.db its credentials were in auth.json, in
    /// pi's shape; two enabled rows for one provider are told apart.
    #[test]
    fn reads_omp_from_the_database_or_its_legacy_file() {
        let dir = std::env::temp_dir().join(format!("ssf-usage-omp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("agent.db");
        std::fs::write(
            dir.join("auth.json"),
            json!({"deepseek": {"type": "api_key", "key": "k"}}).to_string(),
        )
        .unwrap();
        let legacy = keyed_file(&db.with_file_name("auth.json"));
        assert_eq!(legacy.len(), 1);
        assert_eq!(legacy[0].provider, Provider::DeepSeek);

        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE auth_credentials (id INTEGER PRIMARY KEY, provider TEXT,
             credential_type TEXT, data TEXT, disabled_cause TEXT);
             INSERT INTO auth_credentials (provider, credential_type, data) VALUES
             ('deepseek', 'api_key', '{\"key\":\"a\"}'),
             ('deepseek', 'api_key', '{\"key\":\"b\"}');",
        )
        .unwrap();
        drop(conn);
        let rows = omp_rows(&db).unwrap();
        assert_eq!(rows.iter().map(|r| r.0).collect::<Vec<_>>(), [1, 2]);
        let keys: Vec<_> = rows
            .iter()
            .map(|(id, provider, data)| {
                let mut c = keyed_credential(provider, data).unwrap();
                c.row = Some(*id);
                key("omp", &c)
            })
            .collect();
        assert_eq!(keys, ["omp:deepseek:1", "omp:deepseek:2"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_expired_token_keeps_the_last_numbers_as_stale() {
        let now = Utc::now();
        let usage = Usage {
            windows: vec![Window {
                label: "5h".into(),
                used_percent: Some(42.0),
                resets_at: None,
            }],
            balances: Vec::new(),
        };
        let cached = Cached {
            fetched_at: now.timestamp() - 3600,
            usage: usage.clone(),
        };
        let account = resolve(
            Provider::Anthropic,
            Some(&cached),
            Some(Outcome::Expired),
            now,
        );
        assert_eq!(account.state, "stale");
        assert_eq!(account.note.as_deref(), Some(STALE_NOTE));
        assert_eq!(account.usage, usage);
        let account = resolve(Provider::Anthropic, None, Some(Outcome::Expired), now);
        assert_eq!(account.state, "unavailable");
        let account = resolve(Provider::Anthropic, Some(&cached), None, now);
        assert_eq!(account.state, "ok");
        let account = resolve(
            Provider::Anthropic,
            Some(&cached),
            Some(Outcome::Fresh(Usage::default())),
            now,
        );
        assert_eq!((account.state, account.usage), ("ok", Usage::default()));
    }
}
