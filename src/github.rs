//! Minimal GitHub client for the bits ssf needs: identity, assigned issues
//! (with ETag support), single issues, issue timelines, and the project
//! boards an item is on (the one GraphQL call).

use anyhow::{Context, Result, bail};
use reqwest::header::{ACCEPT, AUTHORIZATION, ETAG, IF_NONE_MATCH, LINK, USER_AGENT};
use reqwest::{Client, Response, StatusCode};
use serde::Deserialize;
use serde_json::Value;
use std::time::Duration;

const API_VERSION: &str = "2022-11-28";

#[derive(Clone)]
pub struct GitHub {
    client: Client,
    api_url: String,
    token: String,
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
        })
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

    async fn check(resp: Response, what: &str) -> Result<Response> {
        let status = resp.status();
        if status.is_success() || status == StatusCode::NOT_MODIFIED {
            return Ok(resp);
        }
        let remaining = resp
            .headers()
            .get("x-ratelimit-remaining")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let retry_after = resp
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let body = resp.text().await.unwrap_or_default();
        let msg: String = serde_json::from_str::<Value>(&body)
            .ok()
            .and_then(|v| {
                v.get("message")
                    .and_then(|m| m.as_str())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| body.chars().take(300).collect());
        if status == StatusCode::FORBIDDEN || status == StatusCode::TOO_MANY_REQUESTS {
            if remaining.as_deref() == Some("0") || retry_after.is_some() {
                bail!(
                    "GitHub rate limited while {what} (retry-after: {}): {msg}",
                    retry_after.unwrap_or_else(|| "unknown".into())
                );
            }
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
        let resp = Self::check(resp, &format!("listing {kind}")).await?;
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
        let resp = Self::check(
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
        Self::check(resp, &format!("removing {kind} entry {id}")).await?;
        Ok(())
    }

    pub async fn whoami(&self) -> Result<User> {
        let resp = self
            .get(&self.url("user"))
            .send()
            .await
            .context("GET /user")?;
        let resp = Self::check(resp, "fetching bot identity").await?;
        resp.json::<User>().await.context("decoding /user")
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
        let resp = Self::check(resp, &format!("listing {filter} items for {owner}/{repo}")).await?;
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
            let resp = Self::check(resp, "paging item list").await?;
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
            "{}?state=open&per_page=100&sort=updated&direction=asc",
            self.url(&format!("repos/{owner}/{repo}/pulls"))
        );
        let mut req = self.get(&first);
        if let Some(tag) = etag {
            req = req.header(IF_NONE_MATCH, tag);
        }
        let resp = req.send().await.with_context(|| format!("GET {first}"))?;
        let resp = Self::check(resp, &format!("listing pull requests for {owner}/{repo}")).await?;
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
            let resp = Self::check(resp, "paging pull list").await?;
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
        let resp = Self::check(resp, &format!("listing collaborators of {owner}/{repo}")).await?;
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
            let resp = Self::check(resp, "paging collaborators").await?;
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
        let resp = Self::check(resp, &format!("fetching {owner}/{repo} PR #{number}")).await?;
        let v: Value = resp.json().await.context("decoding pull request")?;
        Ok(PrInfo::from_value(&v))
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
        let resp = Self::check(
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

    pub async fn issue(&self, owner: &str, repo: &str, number: u64) -> Result<Issue> {
        self.issue_opt(owner, repo, number)
            .await?
            .with_context(|| format!("GitHub 404 Not Found while fetching {owner}/{repo}#{number}"))
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
        let resp = Self::check(resp, &format!("fetching {owner}/{repo}#{number}")).await?;
        resp.json().await.context("decoding issue").map(Some)
    }

    /// Whether `path` exists in the repository, on `branch` or the default
    /// branch, through the contents API: one request, no clone (`ssf
    /// doctor` looks for the project notes this way). A 404 is `false`.
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
        Self::check(resp, &format!("looking for {path} in {owner}/{repo}")).await?;
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
        let resp = Self::check(resp, &format!("commenting on {owner}/{repo}#{number}")).await?;
        let v: Value = resp.json().await.unwrap_or(Value::Null);
        Ok(v.get("html_url")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string())
    }

    /// Full timeline for an issue, oldest first, all pages.
    pub async fn timeline(&self, owner: &str, repo: &str, number: u64) -> Result<Vec<Value>> {
        let mut url = Some(format!(
            "{}?per_page=100",
            self.url(&format!("repos/{owner}/{repo}/issues/{number}/timeline"))
        ));
        let mut events = Vec::new();
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
            let resp = Self::check(
                resp,
                &format!("fetching timeline of {owner}/{repo}#{number}"),
            )
            .await?;
            url = next_link(&resp);
            let page: Vec<Value> = resp.json().await.context("decoding timeline page")?;
            events.extend(page);
        }
        Ok(events)
    }
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
