//! Minimal GitHub REST client for the bits ssf needs: identity, assigned
//! issues (with ETag support), single issues, and issue timelines.

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

    /// Open issues assigned to `login`. Pull requests are filtered out.
    pub async fn assigned_issues(
        &self,
        owner: &str,
        repo: &str,
        login: &str,
        etag: Option<&str>,
    ) -> Result<Conditional<Vec<Issue>>> {
        let first = format!(
            "{}?assignee={}&state=open&per_page=100&sort=updated&direction=asc",
            self.url(&format!("repos/{owner}/{repo}/issues")),
            login
        );
        let mut req = self.get(&first);
        if let Some(tag) = etag {
            req = req.header(IF_NONE_MATCH, tag);
        }
        let resp = req.send().await.with_context(|| format!("GET {first}"))?;
        let resp = Self::check(resp, &format!("listing issues for {owner}/{repo}")).await?;
        if resp.status() == StatusCode::NOT_MODIFIED {
            return Ok(Conditional::NotModified);
        }
        let new_etag = header_str(&resp, ETAG);
        let mut next = next_link(&resp);
        let mut issues: Vec<Issue> = resp.json().await.context("decoding issue list")?;
        while let Some(url) = next.take() {
            let resp = self
                .get(&url)
                .send()
                .await
                .with_context(|| format!("GET {url}"))?;
            let resp = Self::check(resp, "paging issue list").await?;
            next = next_link(&resp);
            let page: Vec<Issue> = resp.json().await.context("decoding issue page")?;
            issues.extend(page);
        }
        issues.retain(|i| !i.is_pull_request());
        Ok(Conditional::Modified {
            value: issues,
            etag: new_etag,
        })
    }

    pub async fn issue(&self, owner: &str, repo: &str, number: u64) -> Result<Issue> {
        let url = self.url(&format!("repos/{owner}/{repo}/issues/{number}"));
        let resp = self
            .get(&url)
            .send()
            .await
            .with_context(|| format!("GET {url}"))?;
        let resp = Self::check(resp, &format!("fetching {owner}/{repo}#{number}")).await?;
        resp.json().await.context("decoding issue")
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
