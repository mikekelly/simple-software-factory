//! Minimal GitHub client for the bits ssf needs: identity, assigned issues
//! (with ETag support), single issues, issue timelines, and the project
//! boards an item is on (the one GraphQL call).

use anyhow::{Context, Result, bail};
use reqwest::header::{ACCEPT, AUTHORIZATION, ETAG, IF_NONE_MATCH, LINK, USER_AGENT};
use reqwest::{Client, Response, StatusCode};
use serde::Deserialize;
use serde_json::Value;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const API_VERSION: &str = "2022-11-28";

#[derive(Clone)]
pub struct GitHub {
    client: Client,
    api_url: String,
    token: String,
    /// When the last rate-limit answer said requests may resume; shared by
    /// every clone, so the engine can pause all polling until then.
    paused_until: Arc<Mutex<Option<SystemTime>>>,
}

/// GitHub refused a request for its rate limit (403/429 with
/// `Retry-After` or `x-ratelimit-remaining: 0`). Downcast from the
/// `anyhow::Error` a call returns; `until` is when to try again.
#[derive(Debug, Clone)]
pub struct RateLimited {
    pub what: String,
    pub until: SystemTime,
    pub message: String,
}

impl std::fmt::Display for RateLimited {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let wait = self
            .until
            .duration_since(SystemTime::now())
            .unwrap_or_default()
            .as_secs();
        write!(
            f,
            "GitHub rate limited while {} (retry in {wait}s): {}",
            self.what, self.message
        )
    }
}

impl std::error::Error for RateLimited {}

/// How long to wait when GitHub gives no usable hint.
const DEFAULT_RATE_LIMIT_PAUSE: Duration = Duration::from_secs(60);
/// Never trust a hint that asks for more than this.
const MAX_RATE_LIMIT_PAUSE: Duration = Duration::from_secs(3600);

/// When requests may resume after a rate-limit answer: `Retry-After`
/// seconds if given, else `x-ratelimit-reset` (epoch seconds) when
/// `x-ratelimit-remaining` is 0, else a minute. `None` when the answer is
/// not a rate limit at all (a plain 403).
pub fn rate_limit_until(
    status: StatusCode,
    retry_after: Option<&str>,
    remaining: Option<&str>,
    reset: Option<&str>,
    message: &str,
    now: SystemTime,
) -> Option<SystemTime> {
    if status != StatusCode::FORBIDDEN && status != StatusCode::TOO_MANY_REQUESTS {
        return None;
    }
    let exhausted = remaining.map(str::trim) == Some("0");
    // A secondary rate limit is a 403 that may carry neither header.
    let secondary = message.to_ascii_lowercase().contains("rate limit");
    if retry_after.is_none() && !exhausted && !secondary && status != StatusCode::TOO_MANY_REQUESTS
    {
        return None;
    }
    let wait = if let Some(secs) = retry_after.and_then(|v| v.trim().parse::<u64>().ok()) {
        Duration::from_secs(secs)
    } else if let Some(at) = reset
        .filter(|_| exhausted)
        .and_then(|v| v.trim().parse::<u64>().ok())
    {
        (UNIX_EPOCH + Duration::from_secs(at))
            .duration_since(now)
            .unwrap_or_default()
            // The reset is to the second; a little slack avoids a retry
            // that lands just before it.
            + Duration::from_secs(1)
    } else {
        DEFAULT_RATE_LIMIT_PAUSE
    };
    Some(now + wait.min(MAX_RATE_LIMIT_PAUSE))
}

#[derive(Debug, Clone, Deserialize)]
pub struct User {
    pub login: String,
    #[serde(default)]
    pub id: u64,
    #[serde(default, rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub email: Option<String>,
    /// The token's classic OAuth scopes, from `X-OAuth-Scopes`; `None` when
    /// GitHub sends no such header (fine-grained and app tokens).
    #[serde(skip)]
    pub scopes: Option<Vec<String>>,
}

impl User {
    /// Required scopes the token lacks; empty when its scopes are unknown.
    /// A keyless (`--no-keys`) setup needs only `repo` and `workflow`.
    pub fn missing_scopes(&self, keyless: bool) -> Vec<&'static str> {
        let Some(have) = &self.scopes else {
            return Vec::new();
        };
        crate::ghcli::REQUIRED_SCOPES
            .iter()
            .copied()
            .filter(|s| !keyless || matches!(*s, "repo" | "workflow"))
            .filter(|s| !have.iter().any(|h| h == s))
            .collect()
    }
}

/// Stable identity plus the mutable canonical name and clone URLs of a
/// GitHub repository.
#[derive(Debug, Clone, Deserialize, serde::Serialize, PartialEq, Eq)]
pub struct RepositoryIdentity {
    pub id: u64,
    pub full_name: String,
    pub clone_url: String,
    pub ssh_url: String,
}

impl User {
    /// Address GitHub attributes commits to even when the profile email is private.
    pub fn noreply_email(&self) -> String {
        format!("{}+{}@users.noreply.github.com", self.id, self.login)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct KeyRecord {
    pub id: u64,
    pub key: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RepositoryInvitation {
    pub id: u64,
    pub repository: RepositoryIdentity,
    pub inviter: Option<User>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Label {
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Issue {
    pub number: u64,
    pub title: String,
    #[serde(default)]
    pub body: Option<String>,
    pub html_url: String,
    pub state: String,
    #[serde(default)]
    pub state_reason: Option<String>,
    pub user: Option<User>,
    #[serde(default)]
    pub assignees: Vec<User>,
    #[serde(default)]
    pub labels: Vec<Label>,
    #[serde(default)]
    pub pull_request: Option<Value>,
    /// Reaction counts on the item's body (`total_count`, `+1`, `heart` ...).
    #[serde(default)]
    pub reactions: Option<Value>,
    pub created_at: String,
    pub updated_at: String,
}

impl Issue {
    pub fn is_pull_request(&self) -> bool {
        self.pull_request.is_some()
    }

    pub fn is_assigned_to(&self, login: &str) -> bool {
        self.assignees
            .iter()
            .any(|a| a.login.eq_ignore_ascii_case(login))
    }

    pub fn author(&self) -> &str {
        self.user
            .as_ref()
            .map(|u| u.login.as_str())
            .unwrap_or("ghost")
    }
}

/// The logins a pull request payload asks for a review.
fn reviewers(v: &Value) -> Vec<String> {
    v.get("requested_reviewers")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|r| value_str(r, &["login"]).map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Where a pull request's code lives.
#[derive(Debug, Clone, Default, serde::Serialize, Deserialize)]
pub struct PrInfo {
    pub head_ref: String,
    pub head_repo: String,
    pub base_ref: String,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub merged: bool,
    /// Logins the pull request currently asks for a review. Kept so that a
    /// review request can be re-checked against the pull request itself,
    /// the way an assignment is re-checked against the issue.
    #[serde(default)]
    pub requested_reviewers: Vec<String>,
}

impl PrInfo {
    pub fn from_value(v: &Value) -> Self {
        Self {
            head_ref: value_str(v, &["head", "ref"]).unwrap_or("").to_string(),
            head_repo: value_str(v, &["head", "repo", "full_name"])
                .unwrap_or("")
                .to_string(),
            base_ref: value_str(v, &["base", "ref"]).unwrap_or("").to_string(),
            draft: v.get("draft").and_then(Value::as_bool).unwrap_or(false),
            merged: v.get("merged").and_then(Value::as_bool).unwrap_or(false)
                || v.get("merged_at").is_some_and(|m| !m.is_null()),
            requested_reviewers: reviewers(v),
        }
    }

    pub fn same_repo(&self, full_name: &str) -> bool {
        self.head_repo.eq_ignore_ascii_case(full_name)
    }

    /// Whether the pull request still asks `login` for a review.
    pub fn requests_review_from(&self, login: &str) -> bool {
        self.requested_reviewers
            .iter()
            .any(|r| r.eq_ignore_ascii_case(login))
    }
}

/// One project (v2) board an issue or pull request is on, with what the
/// agent needs to read and change the card's Status.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, Deserialize)]
pub struct ProjectCard {
    /// Node id of the project (`PVT_...`).
    pub project_id: String,
    pub title: String,
    pub url: String,
    /// Node id of this item on the board (`PVTI_...`).
    pub item_id: String,
    /// Current value of the Status field, if the board has one and it is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Node id of the Status field (`PVTSSF_...`), if the board has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_field_id: Option<String>,
    /// The Status options the board offers, in board order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub status_options: Vec<StatusOption>,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, Deserialize)]
pub struct StatusOption {
    pub id: String,
    pub name: String,
}

const PROJECT_ITEMS_QUERY: &str = r#"query($owner: String!, $name: String!, $number: Int!) {
  repository(owner: $owner, name: $name) {
    issueOrPullRequest(number: $number) {
      ... on Issue { projectItems(first: 20) { nodes { ...card } } }
      ... on PullRequest { projectItems(first: 20) { nodes { ...card } } }
    }
  }
}
fragment card on ProjectV2Item {
  id
  project {
    id
    title
    url
    closed
    field(name: "Status") {
      ... on ProjectV2SingleSelectField { id options { id name } }
    }
  }
  fieldValueByName(name: "Status") {
    ... on ProjectV2ItemFieldSingleSelectValue { name }
  }
}"#;

const REPOSITORY_PROJECTS_QUERY: &str = r#"query($owner: String!, $name: String!) {
  repository(owner: $owner, name: $name) { projectsV2(first: 50) { nodes { url } } }
}"#;

/// The URLs of the Projects v2 linked to a repository, out of a
/// `projectsV2` GraphQL response (#499).
pub fn parse_repository_projects(data: &Value) -> Vec<String> {
    data.pointer("/repository/projectsV2/nodes")
        .and_then(Value::as_array)
        .map(|nodes| {
            nodes
                .iter()
                .filter_map(|n| value_str(n, &["url"]))
                .filter(|url| !url.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Cards out of a `projectItems` GraphQL response. Closed boards are left
/// out: nothing is expected to be kept accurate on them.
pub fn parse_project_items(data: &Value) -> Vec<ProjectCard> {
    let nodes = data
        .pointer("/repository/issueOrPullRequest/projectItems/nodes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    nodes
        .iter()
        .filter_map(|n| {
            let project = n.get("project")?;
            if project
                .get("closed")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                return None;
            }
            let field = project.get("field").filter(|f| !f.is_null());
            let status_options = field
                .and_then(|f| f.get("options"))
                .and_then(Value::as_array)
                .map(|opts| {
                    opts.iter()
                        .filter_map(|o| {
                            Some(StatusOption {
                                id: value_str(o, &["id"])?.to_string(),
                                name: value_str(o, &["name"])?.to_string(),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            Some(ProjectCard {
                project_id: value_str(project, &["id"])?.to_string(),
                title: value_str(project, &["title"])
                    .unwrap_or("(untitled)")
                    .to_string(),
                url: value_str(project, &["url"]).unwrap_or("").to_string(),
                item_id: value_str(n, &["id"])?.to_string(),
                status: value_str(n, &["fieldValueByName", "name"]).map(str::to_string),
                status_field_id: field
                    .and_then(|f| value_str(f, &["id"]))
                    .map(str::to_string),
                status_options,
            })
        })
        .collect()
}

/// One CI check on a commit: a check run or a commit status (#641).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub name: String,
    /// Where its run is shown (`html_url`, `details_url` or `target_url`).
    pub url: String,
    /// It has finished (a check run `completed`, a status not `pending`).
    pub done: bool,
    /// It finished and did not pass.
    pub failed: bool,
    /// When it last moved, GitHub's ISO timestamp, or empty.
    pub at: String,
}

/// The checks in a `check-runs` answer and a combined `status` answer.
/// GitHub gives the latest run of each check and the latest status of each
/// context, so a re-run replaces the run it repeats.
pub fn ci_checks(runs: Option<&Value>, status: Option<&Value>) -> Vec<Check> {
    let list = |v: Option<&Value>, key: &str| -> Vec<Value> {
        v.and_then(|v| v.get(key))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    let mut out = Vec::new();
    for r in list(runs, "check_runs") {
        let done = value_str(&r, &["status"]) == Some("completed");
        let conclusion = value_str(&r, &["conclusion"]).unwrap_or("");
        out.push(Check {
            name: value_str(&r, &["name"]).unwrap_or("?").to_string(),
            url: value_str(&r, &["html_url"])
                .or_else(|| value_str(&r, &["details_url"]))
                .unwrap_or("")
                .to_string(),
            done,
            failed: done
                && matches!(
                    conclusion,
                    "failure" | "timed_out" | "cancelled" | "action_required" | "startup_failure"
                ),
            at: value_str(&r, &["completed_at"])
                .or_else(|| value_str(&r, &["started_at"]))
                .unwrap_or("")
                .to_string(),
        });
    }
    for s in list(status, "statuses") {
        let state = value_str(&s, &["state"]).unwrap_or("pending");
        out.push(Check {
            name: value_str(&s, &["context"]).unwrap_or("?").to_string(),
            url: value_str(&s, &["target_url"]).unwrap_or("").to_string(),
            done: state != "pending",
            failed: matches!(state, "failure" | "error"),
            at: value_str(&s, &["updated_at"]).unwrap_or("").to_string(),
        });
    }
    out
}

/// Result of a conditional GET.
pub enum Conditional<T> {
    NotModified,
    Modified { value: T, etag: Option<String> },
}

impl GitHub {
    pub fn new(api_url: &str, token: &str) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .context("building HTTP client")?;
        Ok(Self {
            client,
            api_url: api_url.trim_end_matches('/').to_string(),
            token: token.to_string(),
            paused_until: Arc::new(Mutex::new(None)),
        })
    }

    /// When the last rate-limit answer said to resume, while that is still
    /// in the future.
    pub fn paused_until(&self) -> Option<SystemTime> {
        let until = (*self.paused_until.lock().unwrap_or_else(|e| e.into_inner()))?;
        (until > SystemTime::now()).then_some(until)
    }

    fn get(&self, url: &str) -> reqwest::RequestBuilder {
        self.client
            .get(url)
            .header(USER_AGENT, concat!("ssf/", env!("CARGO_PKG_VERSION")))
            .header(ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", API_VERSION)
            .header(AUTHORIZATION, format!("Bearer {}", self.token))
    }

    fn url(&self, path: &str) -> String {
        format!("{}/{}", self.api_url, path.trim_start_matches('/'))
    }

    async fn check(&self, resp: Response, what: &str) -> Result<Response> {
        let status = resp.status();
        if status.is_success() || status == StatusCode::NOT_MODIFIED {
            return Ok(resp);
        }
        let header = |name: &str| {
            resp.headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
        };
        let remaining = header("x-ratelimit-remaining");
        let retry_after = header("retry-after");
        let reset = header("x-ratelimit-reset");
        let body = resp.text().await.unwrap_or_default();
        let msg: String = serde_json::from_str::<Value>(&body)
            .ok()
            .and_then(|v| {
                v.get("message")
                    .and_then(|m| m.as_str())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| body.chars().take(300).collect());
        if let Some(until) = rate_limit_until(
            status,
            retry_after.as_deref(),
            remaining.as_deref(),
            reset.as_deref(),
            &msg,
            SystemTime::now(),
        ) {
            let mut paused = self.paused_until.lock().unwrap_or_else(|e| e.into_inner());
            if paused.is_none_or(|p| p < until) {
                *paused = Some(until);
            }
            return Err(RateLimited {
                what: what.to_string(),
                until,
                message: msg,
            }
            .into());
        }
        bail!("GitHub {status} while {what}: {msg}")
    }

    fn post(&self, url: &str) -> reqwest::RequestBuilder {
        self.client
            .post(url)
            .header(USER_AGENT, concat!("ssf/", env!("CARGO_PKG_VERSION")))
            .header(ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", API_VERSION)
            .header(AUTHORIZATION, format!("Bearer {}", self.token))
    }

    fn delete(&self, url: &str) -> reqwest::RequestBuilder {
        self.client
            .delete(url)
            .header(USER_AGENT, concat!("ssf/", env!("CARGO_PKG_VERSION")))
            .header(ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", API_VERSION)
            .header(AUTHORIZATION, format!("Bearer {}", self.token))
    }

    /// Keys of the given kind (`keys` = authentication, `ssh_signing_keys` = signing).
    pub async fn list_keys(&self, kind: &str) -> Result<Vec<KeyRecord>> {
        let url = format!("{}?per_page=100", self.url(&format!("user/{kind}")));
        let resp = self
            .get(&url)
            .send()
            .await
            .with_context(|| format!("GET {url}"))?;
        let resp = self.check(resp, &format!("listing {kind}")).await?;
        resp.json().await.context("decoding keys")
    }

    /// Register a public key, reusing an identical one that is already there.
    pub async fn add_key(&self, kind: &str, title: &str, public_key: &str) -> Result<u64> {
        let wanted = key_body(public_key);
        for k in self.list_keys(kind).await? {
            if key_body(&k.key) == wanted {
                return Ok(k.id);
            }
        }
        let url = self.url(&format!("user/{kind}"));
        let resp = self
            .post(&url)
            .json(&serde_json::json!({"title": title, "key": public_key.trim()}))
            .send()
            .await
            .with_context(|| format!("POST {url}"))?;
        let resp = self
            .check(
                resp,
                &format!("adding {kind} entry (token needs write access to keys)"),
            )
            .await?;
        let rec: KeyRecord = resp.json().await.context("decoding key")?;
        Ok(rec.id)
    }

    pub async fn delete_key(&self, kind: &str, id: u64) -> Result<()> {
        let url = self.url(&format!("user/{kind}/{id}"));
        let resp = self
            .delete(&url)
            .send()
            .await
            .with_context(|| format!("DELETE {url}"))?;
        if resp.status() == StatusCode::NOT_FOUND {
            return Ok(());
        }
        self.check(resp, &format!("removing {kind} entry {id}"))
            .await?;
        Ok(())
    }

    pub async fn whoami(&self) -> Result<User> {
        let resp = self
            .get(&self.url("user"))
            .send()
            .await
            .context("GET /user")?;
        let resp = self.check(resp, "fetching bot identity").await?;
        let scopes = resp
            .headers()
            .get("x-oauth-scopes")
            .and_then(|v| v.to_str().ok())
            .map(|v| {
                v.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
            })
            .filter(|s| !s.is_empty());
        let mut me = resp.json::<User>().await.context("decoding /user")?;
        me.scopes = scopes;
        Ok(me)
    }

    /// Pending repository invitations for the authenticated user, across all
    /// pages. These are account-wide rather than scoped to a configured repo.
    pub async fn repository_invitations(
        &self,
        etag: Option<&str>,
    ) -> Result<Conditional<Vec<RepositoryInvitation>>> {
        let mut url = Some(format!(
            "{}?per_page=100",
            self.url("user/repository_invitations")
        ));
        let mut invitations = Vec::new();
        let mut pages = 0;
        let mut new_etag = None;
        while let Some(u) = url.take() {
            pages += 1;
            if pages > 50 {
                bail!("repository invitations exceed 50 pages; giving up");
            }
            let mut req = self.get(&u);
            if pages == 1
                && let Some(tag) = etag
            {
                req = req.header(IF_NONE_MATCH, tag);
            }
            let resp = req.send().await.with_context(|| format!("GET {u}"))?;
            let resp = self.check(resp, "listing repository invitations").await?;
            if pages == 1 {
                if resp.status() == StatusCode::NOT_MODIFIED {
                    return Ok(Conditional::NotModified);
                }
                new_etag = header_str(&resp, ETAG);
            }
            url = next_link(&resp);
            let page: Vec<RepositoryInvitation> = resp
                .json()
                .await
                .context("decoding repository invitations")?;
            invitations.extend(page);
        }
        // The first page's ETag vouches for the whole list only when there
        // is no other page.
        Ok(Conditional::Modified {
            value: invitations,
            etag: new_etag.filter(|_| pages == 1),
        })
    }

    pub async fn accept_repository_invitation(&self, id: u64) -> Result<()> {
        let url = self.url(&format!("user/repository_invitations/{id}"));
        let resp = self
            .client
            .patch(&url)
            .header(USER_AGENT, concat!("ssf/", env!("CARGO_PKG_VERSION")))
            .header(ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", API_VERSION)
            .header(AUTHORIZATION, format!("Bearer {}", self.token))
            .send()
            .await
            .with_context(|| format!("PATCH {url}"))?;
        self.check(resp, &format!("accepting repository invitation {id}"))
            .await?;
        Ok(())
    }

    pub async fn repository(&self, owner: &str, repo: &str) -> Result<RepositoryIdentity> {
        let url = self.url(&format!("repos/{owner}/{repo}"));
        let resp = self
            .get(&url)
            .send()
            .await
            .with_context(|| format!("GET {url}"))?;
        let resp = self
            .check(resp, &format!("resolving repository {owner}/{repo}"))
            .await?;
        resp.json().await.context("decoding repository identity")
    }

    pub async fn repository_by_id(&self, id: u64) -> Result<RepositoryIdentity> {
        let url = self.url(&format!("repositories/{id}"));
        let resp = self
            .get(&url)
            .send()
            .await
            .with_context(|| format!("GET {url}"))?;
        let resp = self
            .check(resp, &format!("resolving repository id {id}"))
            .await?;
        resp.json().await.context("decoding repository identity")
    }

    /// Open issues and pull requests matching one list filter
    /// (`assignee=<login>` or `mentioned=<login>`), with ETag support.
    pub async fn items(
        &self,
        owner: &str,
        repo: &str,
        filter: &str,
        login: &str,
        etag: Option<&str>,
    ) -> Result<Conditional<Vec<Issue>>> {
        let first = format!(
            "{}?{filter}={login}&state=open&per_page=100&sort=updated&direction=asc",
            self.url(&format!("repos/{owner}/{repo}/issues"))
        );
        let mut req = self.get(&first);
        if let Some(tag) = etag {
            req = req.header(IF_NONE_MATCH, tag);
        }
        let resp = req.send().await.with_context(|| format!("GET {first}"))?;
        let resp = self
            .check(resp, &format!("listing {filter} items for {owner}/{repo}"))
            .await?;
        if resp.status() == StatusCode::NOT_MODIFIED {
            return Ok(Conditional::NotModified);
        }
        let new_etag = header_str(&resp, ETAG);
        let mut next = next_link(&resp);
        let mut issues: Vec<Issue> = resp.json().await.context("decoding item list")?;
        while let Some(url) = next.take() {
            let resp = self
                .get(&url)
                .send()
                .await
                .with_context(|| format!("GET {url}"))?;
            let resp = self.check(resp, "paging item list").await?;
            next = next_link(&resp);
            let page: Vec<Issue> = resp.json().await.context("decoding item page")?;
            issues.extend(page);
        }
        Ok(Conditional::Modified {
            value: issues,
            etag: new_etag,
        })
    }

    /// Open pull requests that currently request a review from `login`.
    pub async fn review_requested(
        &self,
        owner: &str,
        repo: &str,
        login: &str,
        etag: Option<&str>,
    ) -> Result<Conditional<Vec<(Issue, PrInfo)>>> {
        let first = format!(
            "{}?state=open&per_page=100&sort=updated&direction=desc",
            self.url(&format!("repos/{owner}/{repo}/pulls"))
        );
        let mut req = self.get(&first);
        if let Some(tag) = etag {
            req = req.header(IF_NONE_MATCH, tag);
        }
        let resp = req.send().await.with_context(|| format!("GET {first}"))?;
        let resp = self
            .check(resp, &format!("listing pull requests for {owner}/{repo}"))
            .await?;
        if resp.status() == StatusCode::NOT_MODIFIED {
            return Ok(Conditional::NotModified);
        }
        let new_etag = header_str(&resp, ETAG);
        let mut next = next_link(&resp);
        let mut pulls: Vec<Value> = resp.json().await.context("decoding pull list")?;
        while let Some(url) = next.take() {
            let resp = self
                .get(&url)
                .send()
                .await
                .with_context(|| format!("GET {url}"))?;
            let resp = self.check(resp, "paging pull list").await?;
            next = next_link(&resp);
            let page: Vec<Value> = resp.json().await.context("decoding pull page")?;
            pulls.extend(page);
        }
        let mut out = Vec::new();
        for pr in pulls {
            let info = PrInfo::from_value(&pr);
            if !info.requests_review_from(login) {
                continue;
            }
            let mut v = pr.clone();
            v["pull_request"] = serde_json::json!({});
            let issue: Issue = serde_json::from_value(v).context("decoding pull as issue")?;
            out.push((issue, info));
        }
        Ok(Conditional::Modified {
            value: out,
            etag: new_etag,
        })
    }

    /// The repository's collaborators (every affiliation), as GitHub
    /// returns them, with ETag support; `allow::pushers` keeps the ones
    /// with push access. Needs push access itself, and `read:org` on an
    /// organisation's repository.
    pub async fn collaborators(
        &self,
        owner: &str,
        repo: &str,
        etag: Option<&str>,
    ) -> Result<Conditional<Vec<Value>>> {
        let first = format!(
            "{}?per_page=100",
            self.url(&format!("repos/{owner}/{repo}/collaborators"))
        );
        let mut req = self.get(&first);
        if let Some(tag) = etag {
            req = req.header(IF_NONE_MATCH, tag);
        }
        let resp = req.send().await.with_context(|| format!("GET {first}"))?;
        let resp = self
            .check(resp, &format!("listing collaborators of {owner}/{repo}"))
            .await?;
        if resp.status() == StatusCode::NOT_MODIFIED {
            return Ok(Conditional::NotModified);
        }
        let new_etag = header_str(&resp, ETAG);
        let mut next = next_link(&resp);
        let mut out: Vec<Value> = resp.json().await.context("decoding collaborators")?;
        while let Some(url) = next.take() {
            let resp = self
                .get(&url)
                .send()
                .await
                .with_context(|| format!("GET {url}"))?;
            let resp = self.check(resp, "paging collaborators").await?;
            next = next_link(&resp);
            let page: Vec<Value> = resp.json().await.context("decoding collaborators page")?;
            out.extend(page);
        }
        Ok(Conditional::Modified {
            value: out,
            etag: new_etag,
        })
    }

    /// Branch details of a pull request.
    pub async fn pull(&self, owner: &str, repo: &str, number: u64) -> Result<PrInfo> {
        let url = self.url(&format!("repos/{owner}/{repo}/pulls/{number}"));
        let resp = self
            .get(&url)
            .send()
            .await
            .with_context(|| format!("GET {url}"))?;
        let resp = self
            .check(resp, &format!("fetching {owner}/{repo} PR #{number}"))
            .await?;
        let v: Value = resp.json().await.context("decoding pull request")?;
        Ok(PrInfo::from_value(&v))
    }

    /// A GET asked with `If-None-Match: etag`: `NotModified` (free against
    /// the rate limit) when the answer is as it was when `etag` was read.
    async fn get_if_changed(
        &self,
        url: &str,
        etag: Option<&str>,
        what: &str,
    ) -> Result<Conditional<Value>> {
        let mut req = self.get(url);
        if let Some(tag) = etag {
            req = req.header(IF_NONE_MATCH, tag);
        }
        let resp = req.send().await.with_context(|| format!("GET {url}"))?;
        let resp = self.check(resp, what).await?;
        if resp.status() == StatusCode::NOT_MODIFIED {
            return Ok(Conditional::NotModified);
        }
        let etag = header_str(&resp, ETAG);
        let value = resp
            .json()
            .await
            .with_context(|| format!("decoding {what}"))?;
        Ok(Conditional::Modified { value, etag })
    }

    /// The head commit of a pull request, asked against `etag` (#641).
    pub async fn pull_head(
        &self,
        owner: &str,
        repo: &str,
        number: u64,
        etag: Option<&str>,
    ) -> Result<Conditional<String>> {
        let url = self.url(&format!("repos/{owner}/{repo}/pulls/{number}"));
        let what = format!("fetching {owner}/{repo} PR #{number}");
        Ok(match self.get_if_changed(&url, etag, &what).await? {
            Conditional::NotModified => Conditional::NotModified,
            Conditional::Modified { value, etag } => Conditional::Modified {
                value: value_str(&value, &["head", "sha"])
                    .unwrap_or("")
                    .to_string(),
                etag,
            },
        })
    }

    /// The check runs (`check-runs`) or the combined commit status
    /// (`status`) of a commit, asked against `etag` (#641); read with
    /// [`ci_checks`]. Only the first hundred of either are read.
    pub async fn commit_ci(
        &self,
        owner: &str,
        repo: &str,
        sha: &str,
        which: &str,
        etag: Option<&str>,
    ) -> Result<Conditional<Value>> {
        let url = format!(
            "{}?per_page=100",
            self.url(&format!("repos/{owner}/{repo}/commits/{sha}/{which}"))
        );
        let what = format!("reading {which} of {owner}/{repo}@{sha}");
        self.get_if_changed(&url, etag, &what).await
    }

    /// The open project boards an issue or pull request is on. Needs the
    /// `project` scope; without it GitHub answers with a GraphQL error.
    pub async fn project_items(
        &self,
        owner: &str,
        repo: &str,
        number: u64,
    ) -> Result<Vec<ProjectCard>> {
        let url = self.url("graphql");
        let resp = self
            .post(&url)
            .json(&serde_json::json!({
                "query": PROJECT_ITEMS_QUERY,
                "variables": {"owner": owner, "name": repo, "number": number},
            }))
            .send()
            .await
            .with_context(|| format!("POST {url}"))?;
        let resp = self
            .check(
                resp,
                &format!("looking up project boards of {owner}/{repo}#{number}"),
            )
            .await?;
        let v: Value = resp.json().await.context("decoding GraphQL response")?;
        if let Some(errors) = v
            .get("errors")
            .and_then(Value::as_array)
            .filter(|e| !e.is_empty())
        {
            let msgs: Vec<&str> = errors
                .iter()
                .filter_map(|e| value_str(e, &["message"]))
                .collect();
            bail!(
                "GraphQL error looking up project boards of {owner}/{repo}#{number}: {}",
                msgs.join("; ")
            );
        }
        Ok(parse_project_items(v.get("data").unwrap_or(&Value::Null)))
    }

    /// The URLs of the Projects v2 linked to a repository (#499).
    pub async fn repository_projects(&self, owner: &str, repo: &str) -> Result<Vec<String>> {
        let url = self.url("graphql");
        let resp = self
            .post(&url)
            .json(&serde_json::json!({
                "query": REPOSITORY_PROJECTS_QUERY,
                "variables": {"owner": owner, "name": repo},
            }))
            .send()
            .await
            .with_context(|| format!("POST {url}"))?;
        let what = format!("looking up the projects of {owner}/{repo}");
        let resp = self.check(resp, &what).await?;
        let v: Value = resp.json().await.context("decoding GraphQL response")?;
        if let Some(errors) = v
            .get("errors")
            .and_then(Value::as_array)
            .filter(|e| !e.is_empty())
        {
            let msgs: Vec<&str> = errors
                .iter()
                .filter_map(|e| value_str(e, &["message"]))
                .collect();
            bail!("GraphQL error {what}: {}", msgs.join("; "));
        }
        Ok(parse_repository_projects(
            v.get("data").unwrap_or(&Value::Null),
        ))
    }

    pub async fn issue(&self, owner: &str, repo: &str, number: u64) -> Result<Issue> {
        self.issue_opt(owner, repo, number)
            .await?
            .with_context(|| format!("GitHub 404 Not Found while fetching {owner}/{repo}#{number}"))
    }

    /// The item, asked with `If-None-Match: etag`: `NotModified` (free
    /// against the rate limit) when it is exactly as it was when `etag`
    /// was read.
    pub async fn issue_if_changed(
        &self,
        owner: &str,
        repo: &str,
        number: u64,
        etag: Option<&str>,
    ) -> Result<Conditional<Issue>> {
        let url = self.url(&format!("repos/{owner}/{repo}/issues/{number}"));
        let mut req = self.get(&url);
        if let Some(tag) = etag {
            req = req.header(IF_NONE_MATCH, tag);
        }
        let resp = req.send().await.with_context(|| format!("GET {url}"))?;
        if resp.status() == StatusCode::NOT_FOUND {
            bail!("GitHub 404 Not Found while fetching {owner}/{repo}#{number}");
        }
        let resp = self
            .check(resp, &format!("fetching {owner}/{repo}#{number}"))
            .await?;
        if resp.status() == StatusCode::NOT_MODIFIED {
            return Ok(Conditional::NotModified);
        }
        let etag = header_str(&resp, ETAG);
        let value = resp.json().await.context("decoding issue")?;
        Ok(Conditional::Modified { value, etag })
    }

    /// The item, or `None` when GitHub 404s for it: deleted, or not
    /// readable with this token any more. An item transferred to another
    /// repository answers a redirect, which is followed, so it comes back
    /// as the item at its new home rather than as `None`. A caller asking
    /// whether an item is still there (`prune_ignored`) reads a 404 as an
    /// answer rather than as a request to retry.
    pub async fn issue_opt(&self, owner: &str, repo: &str, number: u64) -> Result<Option<Issue>> {
        let url = self.url(&format!("repos/{owner}/{repo}/issues/{number}"));
        let resp = self
            .get(&url)
            .send()
            .await
            .with_context(|| format!("GET {url}"))?;
        if resp.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let resp = self
            .check(resp, &format!("fetching {owner}/{repo}#{number}"))
            .await?;
        resp.json().await.context("decoding issue").map(Some)
    }

    /// Whether `path` exists in the repository, on `branch` or the default
    /// branch, through the contents API: one request, no clone (`ssf
    /// doctor` looks for the SSF agent guidance this way). A 404 is `false`.
    pub async fn has_file(
        &self,
        owner: &str,
        repo: &str,
        path: &str,
        branch: Option<&str>,
    ) -> Result<bool> {
        let mut url = reqwest::Url::parse(&self.url(&format!("repos/{owner}/{repo}/contents/")))
            .context("building the contents URL")?;
        url.path_segments_mut()
            .map_err(|_| anyhow::anyhow!("cannot build the contents URL"))?
            .pop_if_empty()
            .extend(path.trim_matches('/').split('/'));
        if let Some(b) = branch {
            url.query_pairs_mut().append_pair("ref", b);
        }
        let resp = self
            .get(url.as_str())
            .send()
            .await
            .with_context(|| format!("GET {url}"))?;
        if resp.status() == StatusCode::NOT_FOUND {
            return Ok(false);
        }
        self.check(resp, &format!("looking for {path} in {owner}/{repo}"))
            .await?;
        Ok(true)
    }

    pub async fn comment(
        &self,
        owner: &str,
        repo: &str,
        number: u64,
        body: &str,
    ) -> Result<String> {
        let url = self.url(&format!("repos/{owner}/{repo}/issues/{number}/comments"));
        let resp = self
            .post(&url)
            .json(&serde_json::json!({ "body": body }))
            .send()
            .await
            .with_context(|| format!("POST {url}"))?;
        let resp = self
            .check(resp, &format!("commenting on {owner}/{repo}#{number}"))
            .await?;
        let v: Value = resp.json().await.unwrap_or(Value::Null);
        Ok(v.get("html_url")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string())
    }

    /// Assign `login` to an issue or pull request (`ssf assign`). GitHub
    /// answers with the updated item; nothing here needs it.
    pub async fn add_assignee(
        &self,
        owner: &str,
        repo: &str,
        number: u64,
        login: &str,
    ) -> Result<()> {
        let url = self.url(&format!("repos/{owner}/{repo}/issues/{number}/assignees"));
        let resp = self
            .post(&url)
            .json(&serde_json::json!({ "assignees": [login] }))
            .send()
            .await
            .with_context(|| format!("POST {url}"))?;
        self.check(
            resp,
            &format!("assigning @{login} to {owner}/{repo}#{number}"),
        )
        .await?;
        Ok(())
    }

    /// Full timeline for an issue, oldest first, all pages.
    pub async fn timeline(&self, owner: &str, repo: &str, number: u64) -> Result<Vec<Value>> {
        Ok(self.timeline_tagged(owner, repo, number).await?.0)
    }

    fn timeline_page(&self, owner: &str, repo: &str, number: u64, page: usize) -> String {
        format!(
            "{}?per_page=100&page={page}",
            self.url(&format!("repos/{owner}/{repo}/issues/{number}/timeline"))
        )
    }

    fn review_comments_page(&self, owner: &str, repo: &str, number: u64, page: usize) -> String {
        format!(
            "{}?per_page=100&page={page}",
            self.url(&format!("repos/{owner}/{repo}/pulls/{number}/comments"))
        )
    }

    /// The full timeline and the ETag of each of its pages, in order, for
    /// [`Self::timeline_changed`] to ask about later. The timeline lacks a
    /// pull request's inline review comments, so they are read from
    /// `pulls/N/comments` and appended, oldest first, each as its own
    /// `line-commented` event keyed by the comment's id; those pages' ETags
    /// follow the timeline's, prefixed [`PULLS_ETAG`]. An issue answers
    /// 404 there: no comments, no ETags.
    pub async fn timeline_tagged(
        &self,
        owner: &str,
        repo: &str,
        number: u64,
    ) -> Result<(Vec<Value>, Vec<String>)> {
        let mut url = Some(self.timeline_page(owner, repo, number, 1));
        let mut events = Vec::new();
        let mut etags = Vec::new();
        let mut pages = 0;
        while let Some(u) = url.take() {
            pages += 1;
            if pages > 50 {
                bail!("timeline for {owner}/{repo}#{number} exceeds 50 pages; giving up");
            }
            let resp = self
                .get(&u)
                .send()
                .await
                .with_context(|| format!("GET {u}"))?;
            let resp = self
                .check(
                    resp,
                    &format!("fetching timeline of {owner}/{repo}#{number}"),
                )
                .await?;
            url = next_link(&resp);
            etags.push(header_str(&resp, ETAG).unwrap_or_default());
            let page: Vec<Value> = resp.json().await.context("decoding timeline page")?;
            // Inline review comments come from `pulls/N/comments` below.
            events.extend(
                page.into_iter()
                    .filter(|e| value_str(e, &["event"]) != Some("line-commented")),
            );
        }
        let mut url = Some(self.review_comments_page(owner, repo, number, 1));
        let mut comments = Vec::new();
        let mut pages = 0;
        while let Some(u) = url.take() {
            pages += 1;
            if pages > 50 {
                bail!("review comments on {owner}/{repo}#{number} exceed 50 pages; giving up");
            }
            let resp = self
                .get(&u)
                .send()
                .await
                .with_context(|| format!("GET {u}"))?;
            if resp.status() == StatusCode::NOT_FOUND {
                break;
            }
            let resp = self
                .check(
                    resp,
                    &format!("fetching review comments of {owner}/{repo}#{number}"),
                )
                .await?;
            url = next_link(&resp);
            etags.push(format!(
                "{PULLS_ETAG}{}",
                header_str(&resp, ETAG).unwrap_or_default()
            ));
            let page: Vec<Value> = resp.json().await.context("decoding review comments")?;
            comments.extend(page);
        }
        comments.sort_by(|a, b| value_str(a, &["created_at"]).cmp(&value_str(b, &["created_at"])));
        events.extend(comments.into_iter().map(review_comment_event));
        Ok((events, etags))
    }

    /// Whether any page of a timeline differs from when it answered
    /// `etags` ([`Self::timeline_tagged`]). Each page is asked with
    /// `If-None-Match`; a 304 costs no rate limit. A reaction moves the
    /// ETag of the page its comment is on, though not the item's
    /// `updated_at`. No ETags (never fetched) counts as changed.
    pub async fn timeline_changed(
        &self,
        owner: &str,
        repo: &str,
        number: u64,
        etags: &[String],
    ) -> Result<bool> {
        let empty = |t: &String| t.strip_prefix(PULLS_ETAG).unwrap_or(t).is_empty();
        if etags.is_empty() || etags.iter().any(empty) {
            return Ok(true);
        }
        let (mut timeline, mut pulls) = (0, 0);
        for tag in etags {
            let (u, tag) = match tag.strip_prefix(PULLS_ETAG) {
                Some(t) => {
                    pulls += 1;
                    (self.review_comments_page(owner, repo, number, pulls), t)
                }
                None => {
                    timeline += 1;
                    (
                        self.timeline_page(owner, repo, number, timeline),
                        tag.as_str(),
                    )
                }
            };
            let resp = self
                .get(&u)
                .header(IF_NONE_MATCH, tag)
                .send()
                .await
                .with_context(|| format!("GET {u}"))?;
            let resp = self
                .check(
                    resp,
                    &format!("checking timeline of {owner}/{repo}#{number}"),
                )
                .await?;
            if resp.status() != StatusCode::NOT_MODIFIED {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Who reacted with what on an item's body (`issues/N`) or on a
    /// comment (`issues/comments/ID`): `(login, content, created_at)`.
    pub async fn reactions(
        &self,
        owner: &str,
        repo: &str,
        on: &str,
    ) -> Result<Vec<(String, String, String)>> {
        let mut url = Some(format!(
            "{}?per_page=100",
            self.url(&format!("repos/{owner}/{repo}/{on}/reactions"))
        ));
        let mut out = Vec::new();
        let mut pages = 0;
        while let Some(u) = url.take() {
            pages += 1;
            if pages > 50 {
                bail!("reactions on {owner}/{repo} {on} exceed 50 pages; giving up");
            }
            let resp = self
                .get(&u)
                .send()
                .await
                .with_context(|| format!("GET {u}"))?;
            let resp = self
                .check(resp, &format!("listing reactions on {owner}/{repo} {on}"))
                .await?;
            url = next_link(&resp);
            let page: Vec<Value> = resp.json().await.context("decoding reactions page")?;
            out.extend(page.iter().map(|r| {
                (
                    value_str(r, &["user", "login"])
                        .unwrap_or("ghost")
                        .to_string(),
                    value_str(r, &["content"]).unwrap_or("").to_string(),
                    value_str(r, &["created_at"]).unwrap_or("").to_string(),
                )
            }));
        }
        Ok(out)
    }
}

/// Marks the ETag of a `pulls/N/comments` page among a timeline's
/// ([`GitHub::timeline_tagged`]).
pub const PULLS_ETAG: &str = "pulls:";

/// An inline review comment (`pulls/N/comments`) as a timeline event: a
/// `line-commented` batch of one, keyed `line-commented:<comment id>` like
/// the per-comment keys origin scanning records.
pub fn review_comment_event(c: Value) -> Value {
    serde_json::json!({
        "event": "line-commented",
        "id": c.get("id").cloned().unwrap_or(Value::Null),
        "created_at": c.get("created_at").cloned().unwrap_or(Value::Null),
        "updated_at": c.get("updated_at").cloned().unwrap_or(Value::Null),
        "user": c.get("user").cloned().unwrap_or(Value::Null),
        "html_url": c.get("html_url").cloned().unwrap_or(Value::Null),
        "comments": [c],
    })
}

/// `type base64` part of an OpenSSH public key, ignoring the comment.
pub fn key_body(key: &str) -> String {
    key.split_whitespace().take(2).collect::<Vec<_>>().join(" ")
}

fn header_str(resp: &Response, name: reqwest::header::HeaderName) -> Option<String> {
    resp.headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

fn next_link(resp: &Response) -> Option<String> {
    let link = resp.headers().get(LINK)?.to_str().ok()?;
    for part in link.split(',') {
        let mut pieces = part.split(';');
        let target = pieces.next()?.trim();
        let is_next = pieces.any(|p| p.trim() == "rel=\"next\"");
        if is_next {
            return Some(
                target
                    .trim_start_matches('<')
                    .trim_end_matches('>')
                    .to_string(),
            );
        }
    }
    None
}

pub fn value_str<'a>(v: &'a Value, path: &[&str]) -> Option<&'a str> {
    let mut cur = v;
    for key in path {
        cur = cur.get(key)?;
    }
    cur.as_str()
}

pub fn value_u64(v: &Value, path: &[&str]) -> Option<u64> {
    let mut cur = v;
    for key in path {
        cur = cur.get(key)?;
    }
    cur.as_u64()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rate_limit_answers_say_when_to_resume() {
        let now = UNIX_EPOCH + Duration::from_secs(1_000);
        let wait = |until: Option<SystemTime>| until.map(|u| u.duration_since(now).unwrap());
        // Retry-After wins.
        let got = rate_limit_until(
            StatusCode::FORBIDDEN,
            Some("30"),
            Some("0"),
            Some("5000"),
            "",
            now,
        );
        assert_eq!(wait(got), Some(Duration::from_secs(30)));
        // Exhausted primary limit: until the reset, plus a second.
        let got = rate_limit_until(
            StatusCode::FORBIDDEN,
            None,
            Some("0"),
            Some("1100"),
            "",
            now,
        );
        assert_eq!(wait(got), Some(Duration::from_secs(101)));
        // A 429 or a secondary limit with no hints: a minute.
        let got = rate_limit_until(StatusCode::TOO_MANY_REQUESTS, None, None, None, "", now);
        assert_eq!(wait(got), Some(Duration::from_secs(60)));
        let msg = "You have exceeded a secondary rate limit.";
        let got = rate_limit_until(StatusCode::FORBIDDEN, None, Some("12"), None, msg, now);
        assert_eq!(wait(got), Some(Duration::from_secs(60)));
        // A hint far in the future is capped at an hour.
        let got = rate_limit_until(StatusCode::FORBIDDEN, Some("99999"), None, None, "", now);
        assert_eq!(wait(got), Some(Duration::from_secs(3600)));
        // A plain 403, or any other status, is not a rate limit.
        let msg = "Must have push access";
        assert!(
            rate_limit_until(StatusCode::FORBIDDEN, None, Some("12"), None, msg, now).is_none()
        );
        assert!(rate_limit_until(StatusCode::NOT_FOUND, Some("30"), None, None, "", now).is_none());
    }

    #[test]
    fn a_rate_limit_error_is_found_under_context() {
        let err: anyhow::Error = RateLimited {
            what: "listing".into(),
            until: SystemTime::now() + Duration::from_secs(5),
            message: "slow down".into(),
        }
        .into();
        let err = err.context("tick_repo");
        assert!(
            err.chain()
                .any(|c| c.downcast_ref::<RateLimited>().is_some())
        );
    }

    #[test]
    fn repository_projects_are_parsed() {
        let data = serde_json::json!({"repository":{"projectsV2":{"nodes":[
            {"url":"https://github.com/users/o/projects/5"},{"url":""},{},
            {"url":"https://github.com/orgs/x/projects/2"}]}}});
        assert_eq!(
            parse_repository_projects(&data),
            vec![
                "https://github.com/users/o/projects/5",
                "https://github.com/orgs/x/projects/2"
            ]
        );
        assert!(parse_repository_projects(&Value::Null).is_empty());
    }

    #[test]
    fn project_items_are_parsed_and_closed_boards_dropped() {
        let data = json!({"repository": {"issueOrPullRequest": {"projectItems": {"nodes": [
            {"id": "PVTI_1", "project": {"id": "PVT_1", "title": "Roadmap", "url": "https://gh/p/1", "closed": false,
                "field": {"id": "PVTSSF_1", "options": [{"id": "a1", "name": "Todo"}, {"id": "b2", "name": "Done"}]}},
             "fieldValueByName": {"name": "Todo"}},
            {"id": "PVTI_2", "project": {"id": "PVT_2", "title": "Old", "url": "https://gh/p/2", "closed": true,
                "field": null}, "fieldValueByName": null},
            {"id": "PVTI_3", "project": {"id": "PVT_3", "title": "No status", "url": "https://gh/p/3", "closed": false,
                "field": null}, "fieldValueByName": null}
        ]}}}});
        let cards = parse_project_items(&data);
        assert_eq!(cards.len(), 2);
        assert_eq!(cards[0].title, "Roadmap");
        assert_eq!(cards[0].item_id, "PVTI_1");
        assert_eq!(cards[0].status.as_deref(), Some("Todo"));
        assert_eq!(cards[0].status_field_id.as_deref(), Some("PVTSSF_1"));
        assert_eq!(cards[0].status_options.len(), 2);
        assert_eq!(cards[0].status_options[1].name, "Done");
        assert_eq!(cards[1].title, "No status");
        assert!(cards[1].status.is_none());
        assert!(cards[1].status_field_id.is_none());
        assert!(cards[1].status_options.is_empty());
        assert!(parse_project_items(&Value::Null).is_empty());
    }

    #[test]
    fn a_pull_request_reports_who_it_asks_for_a_review() {
        let v = json!({
            "head": {"ref": "b", "repo": {"full_name": "o/r"}},
            "base": {"ref": "main"},
            "requested_reviewers": [{"login": "Bot"}, {"login": "someone"}]
        });
        let pr = PrInfo::from_value(&v);
        assert_eq!(pr.requested_reviewers, vec!["Bot", "someone"]);
        assert!(pr.requests_review_from("bot"));
        assert!(!pr.requests_review_from("nobody"));
        // A pull request with the key missing asks nobody.
        let none = PrInfo::from_value(&json!({"head": {"ref": "b"}, "base": {"ref": "main"}}));
        assert!(none.requested_reviewers.is_empty());
        assert!(!none.requests_review_from("bot"));
    }
}

#[cfg(test)]
mod user_scope_tests {
    use super::User;

    fn user(scopes: Option<&[&str]>) -> User {
        User {
            login: "bot".into(),
            id: 0,
            kind: String::new(),
            email: None,
            scopes: scopes.map(|s| s.iter().map(|s| s.to_string()).collect()),
        }
    }

    #[test]
    fn missing_scopes_names_workflow_and_ignores_unknown_scopes() {
        let old = user(Some(&[
            "repo",
            "project",
            "admin:public_key",
            "admin:ssh_signing_key",
        ]));
        assert_eq!(old.missing_scopes(false), vec!["workflow"]);
        assert_eq!(user(Some(&["repo"])).missing_scopes(true), vec!["workflow"]);
        let full = user(Some(crate::ghcli::REQUIRED_SCOPES));
        assert!(full.missing_scopes(false).is_empty());
        assert!(user(None).missing_scopes(false).is_empty());
    }
}
