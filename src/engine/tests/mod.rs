use super::*;
use serde_json::json;

pub(super) fn engine() -> Engine {
    let _ = rustls::crypto::ring::default_provider().install_default();
    // These tests are about everything but access: anyone may drive.
    // The allow-list tests below set their own lists.
    let mut cfg = Config::default();
    cfg.daemon.allowed_users = Some(vec!["*".into()]);
    cfg.daemon.accepted_anyone_risk = true;
    // The stand-in driver below is Orca; herdr is the default now.
    cfg.driver = Some(DriverKind::Orca);
    Engine {
        cfg,
        gh: GitHub::new("https://api.github.invalid", "t").unwrap(),
        drivers: Drivers::from_list(vec![Driver::Orca(crate::orca::Orca::new(
            crate::config::OrcaConfig {
                command: "/nonexistent/orca-for-ssf-tests".into(),
                ..Default::default()
            },
        ))]),
        down: Vec::new(),
        login: "bot".into(),
        state: State::default(),
        failures: BTreeMap::new(),
        startup_pending: Vec::new(),
        collaborators: BTreeMap::new(),
        dropped_logged: std::sync::Mutex::new(BTreeSet::new()),
        probe: std::sync::Arc::new(|_| Probe {
            state: LoginState::Unknown,
            detail: "test".into(),
            fingerprint: None,
        }),
        installed: std::sync::Arc::new(|_| true),
        probes: BTreeMap::new(),
        refetch: BTreeSet::new(),
        startup_pass: false,
        onboarding: None,
        conflict_checks: BTreeMap::new(),
        conflict_pairs: BTreeMap::new(),
        _state_lock: None,
    }
}

pub(super) fn repo() -> RepoConfig {
    RepoConfig {
        name: "o/r".into(),
        harness: "claude".into(),
        ..Default::default()
    }
}

// A real checkout with one branch that conflicts with a moved `main`,
// plus a stub session that can receive the advisory.
async fn conflict_fixture(
    name: &str,
    number: u64,
) -> (
    Engine,
    RepoConfig,
    crate::driver::StubDriver,
    crate::release::testkit::Scratch,
    String,
) {
    use crate::release::testkit::{scratch, sh};

    let s = scratch(name).await;
    let worktree_name = format!("issue-{number}-conflict");
    let (path, branch) = crate::driver::add_local_worktree(&s.work, &worktree_name, None)
        .await
        .unwrap();
    std::fs::write(std::path::Path::new(&path).join("a.txt"), "feature\n").unwrap();
    sh(&path, &["add", "a.txt"]).await;
    sh(&path, &["commit", "-q", "-m", "feature"]).await;
    std::fs::write(std::path::Path::new(&s.work).join("a.txt"), "base\n").unwrap();
    sh(&s.work, &["add", "a.txt"]).await;
    sh(&s.work, &["commit", "-q", "-m", "base"]).await;
    sh(&s.work, &["push", "-q", "origin", "main"]).await;
    // Do not rely on remote.origin.fetch: the production check supplies
    // an explicit branch refspec, which this narrow mapping exercises.
    sh(
        &s.work,
        &[
            "config",
            "remote.origin.fetch",
            "+refs/heads/other:refs/remotes/origin/other",
        ],
    )
    .await;
    sh(&s.work, &["remote", "set-head", "origin", "main"]).await;
    // Remove the stale tracking ref: only the explicit refspec in the
    // conflict check can restore it under this narrow mapping.
    sh(&s.work, &["update-ref", "-d", "refs/remotes/origin/main"]).await;

    let mut e = engine();
    let d = crate::driver::StubDriver::new(DriverKind::Orca);
    e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
    e.cfg.daemon.conflict_check_interval_secs = 1;
    let mut r = repo();
    r.path = Some(s.work.clone());
    d.seed(&format!("w{number}"), &format!("t{number}"), READY_SCREEN);
    {
        let st = e.entry(&r, number);
        st.html_url = format!("https://gh/{number}");
        st.seeded = true;
        st.active = true;
        st.worktree_id = Some(format!("w{number}"));
        st.worktree_path = Some(path.clone());
        st.repo_id = Some(s.work.clone());
        st.branch = Some(branch);
    }
    (e, r, d, s, path)
}

fn issue(number: u64, author: &str, body: Option<&str>) -> Issue {
    serde_json::from_value(json!({
        "number": number, "title": "t", "body": body, "html_url": format!("https://gh/{number}"),
        "state": "open", "user": {"login": author}, "created_at": "x", "updated_at": "x"
    }))
    .unwrap()
}

// Put an ignored item's absence (and the look it has had) `by` into
// the past, the way waiting would.
fn rewind_absence(e: &mut Engine, r: &RepoConfig, number: u64, by: Duration) {
    let then = (chrono::Utc::now() - chrono::Duration::from_std(by).unwrap())
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let at = e
        .state
        .repo_mut(&r.name)
        .ignored
        .get_mut(&number)
        .expect("an ignore record to rewind");
    if at.absent_since.is_some() {
        at.absent_since = Some(then.clone());
    }
    if at.asked_at.is_some() {
        at.asked_at = Some(then);
    }
}

// The item numbers a pass asked GitHub about by number.
fn looked_at(hits: &[String]) -> BTreeSet<u64> {
    hits.iter()
        .filter_map(|h| h.strip_prefix("/repos/o/r/issues/"))
        .filter_map(|n| n.parse().ok())
        .collect()
}

fn ignored_numbers(e: &Engine, r: &RepoConfig) -> Vec<u64> {
    e.state.repos[&r.name].ignored.keys().copied().collect()
}

pub(super) fn seeded(e: &mut Engine, number: u64, branch: Option<&str>, active: bool) {
    let st = e.entry(&repo(), number);
    st.seeded = true;
    st.active = active;
    st.branch = branch.map(|b| format!("refs/heads/{b}"));
    st.bound_at = Some(format!("2026-01-0{}T00:00:00Z", number));
}

fn pr(head: &str) -> PrInfo {
    PrInfo {
        head_ref: head.into(),
        head_repo: "o/r".into(),
        base_ref: "main".into(),
        ..Default::default()
    }
}

fn comment(id: u64, who: &str, body: &str) -> Value {
    json!({"event":"commented","id":id,"user":{"login":who},"body":body,
            "html_url":format!("u{id}"),"created_at":"t","updated_at":"t"})
}

// Replays issue #27: an item the bot opened was assigned to it just
// before the first pass saw it, but that pass met it through the
// creator listing alone (the assignee listing was a 304 against an
// ETag from before the assignment), so it was ignored as created-only
// with an `updated_at` that already reflected the assignment. When
// the assignee listing carried it on a later pass, nothing had
// "changed", and the assignment never produced a session.

// A stand-in for the GitHub API on a local port. It answers the four
// listings, records the path of every request, and fails any other
// request (an item or its timeline), so a test can say exactly what a
// pass fetched. The creator listing honours `If-None-Match` against
// an ETag the test can roll over (GitHub's do); the other three never
// answer 304, so a pass never takes the "nothing changed" shortcut.
struct GitHubStub {
    base: String,
    hits: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    /// Open items the bot opened, as the creator listing reports them.
    created: std::sync::Arc<std::sync::Mutex<Vec<Value>>>,
    /// Bumped to give the creator listing a new ETag: the next request
    /// gets a full listing, whatever it carries.
    created_etag: std::sync::Arc<std::sync::atomic::AtomicU32>,
    /// How many times the creator listing has been served in full (a
    /// request with no `If-None-Match`, or one whose ETag has moved
    /// on), rather than answered 304.
    created_fulls: std::sync::Arc<std::sync::atomic::AtomicU32>,
    /// Open items assigned to the bot, as the assignee listing reports
    /// them (a fresh ETag every time: always a full listing).
    assigned: std::sync::Arc<std::sync::Mutex<Vec<Value>>>,
    /// Timelines by item number (`[]` for an unknown item).
    timelines: std::sync::Arc<std::sync::Mutex<BTreeMap<u64, Vec<Value>>>>,
    /// Items served by number (`/repos/o/r/issues/N`), for the paths
    /// that read one item rather than a listing (a session's story).
    issues: std::sync::Arc<std::sync::Mutex<BTreeMap<u64, Value>>>,
    /// Pull requests served by number (`/repos/o/r/pulls/N`). A number
    /// that is not here answers 500, which is how a test says the
    /// fetch failed.
    pulls: std::sync::Arc<std::sync::Mutex<BTreeMap<u64, Value>>>,
    /// The collaborators endpoint: `None` answers 403 (no access), a
    /// list is served with an ETag that changes when it is set.
    collaborators: std::sync::Arc<std::sync::Mutex<Option<Vec<Value>>>>,
    collab_version: std::sync::Arc<std::sync::atomic::AtomicU32>,
    /// Comments posted (`/repos/o/r/issues/N/comments`), in order:
    /// the path and the comment body.
    posts: std::sync::Arc<std::sync::Mutex<Vec<(String, String)>>>,
}

impl GitHubStub {
    async fn start() -> Self {
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::{Arc, Mutex};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let hits: Arc<Mutex<Vec<String>>> = Arc::default();
        let created: Arc<Mutex<Vec<Value>>> = Arc::default();
        let created_etag = Arc::new(AtomicU32::new(1));
        let created_fulls = Arc::new(AtomicU32::new(0));
        let assigned: Arc<Mutex<Vec<Value>>> = Arc::default();
        let timelines: Arc<Mutex<BTreeMap<u64, Vec<Value>>>> = Arc::default();
        let issues: Arc<Mutex<BTreeMap<u64, Value>>> = Arc::default();
        let pulls: Arc<Mutex<BTreeMap<u64, Value>>> = Arc::default();
        let collaborators: Arc<Mutex<Option<Vec<Value>>>> = Arc::default();
        let collab_version = Arc::new(AtomicU32::new(1));
        let posts: Arc<Mutex<Vec<(String, String)>>> = Arc::default();
        let p = posts.clone();
        let (h, c, v) = (hits.clone(), created.clone(), created_etag.clone());
        let cf = created_fulls.clone();
        let (a, t, k, kv) = (
            assigned.clone(),
            timelines.clone(),
            collaborators.clone(),
            collab_version.clone(),
        );
        let i = issues.clone();
        let pl = pulls.clone();
        tokio::spawn(async move {
            let other_etags = AtomicU32::new(1);
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                // The head, then as much body as `Content-Length` says.
                let mut buf = Vec::new();
                let mut chunk = [0u8; 1024];
                let mut head_len = None;
                loop {
                    if head_len.is_none() {
                        head_len = buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4);
                    }
                    if let Some(hl) = head_len {
                        let head = String::from_utf8_lossy(&buf[..hl]);
                        let len = head
                            .lines()
                            .find_map(|l| {
                                let (k, v) = l.split_once(':')?;
                                k.eq_ignore_ascii_case("content-length")
                                    .then(|| v.trim().parse::<usize>().ok())
                                    .flatten()
                            })
                            .unwrap_or(0);
                        if buf.len() >= hl + len {
                            break;
                        }
                    }
                    let n = sock.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                let hl = head_len.unwrap_or(buf.len());
                let head = String::from_utf8_lossy(&buf[..hl]).to_string();
                let sent = String::from_utf8_lossy(&buf[hl..]).to_string();
                let mut lines = head.lines();
                let first = lines.next().unwrap_or("").to_string();
                let method = first.split(' ').next().unwrap_or("").to_string();
                let target = first.split(' ').nth(1).unwrap_or("").to_string();
                let if_none_match = lines.find_map(|l| {
                    let (k, val) = l.split_once(':')?;
                    k.eq_ignore_ascii_case("if-none-match")
                        .then(|| val.trim().to_string())
                });
                h.lock().unwrap().push(target.clone());
                let (path, query) = target.split_once('?').unwrap_or((&target, ""));
                let (status, etag, body) = if path == "/user" {
                    (
                        "200 OK",
                        "\"user\"".to_string(),
                        r#"{"login":"bot","id":1,"type":"User"}"#.to_string(),
                    )
                } else if method == "POST" && path.ends_with("/comments") {
                    let comment = serde_json::from_str::<Value>(&sent)
                        .ok()
                        .and_then(|v| v["body"].as_str().map(str::to_string))
                        .unwrap_or(sent.clone());
                    p.lock().unwrap().push((path.to_string(), comment));
                    (
                        "201 Created",
                        "\"p\"".to_string(),
                        r#"{"html_url":"https://gh/comment"}"#.to_string(),
                    )
                } else if path == "/repos/o/r/issues" && query.starts_with("creator=") {
                    let etag = format!("\"c{}\"", v.load(Ordering::SeqCst));
                    if if_none_match.as_deref() == Some(etag.as_str()) {
                        ("304 Not Modified", etag, String::new())
                    } else {
                        cf.fetch_add(1, Ordering::SeqCst);
                        let items = Value::Array(c.lock().unwrap().clone());
                        ("200 OK", etag, items.to_string())
                    }
                } else if path == "/repos/o/r/issues" && query.starts_with("assignee=") {
                    let n = other_etags.fetch_add(1, Ordering::SeqCst);
                    let items = Value::Array(a.lock().unwrap().clone());
                    ("200 OK", format!("\"o{n}\""), items.to_string())
                } else if path == "/repos/o/r/issues" || path == "/repos/o/r/pulls" {
                    let n = other_etags.fetch_add(1, Ordering::SeqCst);
                    ("200 OK", format!("\"o{n}\""), "[]".to_string())
                } else if path == "/repos/o/r/collaborators" {
                    let etag = format!("\"k{}\"", kv.load(Ordering::SeqCst));
                    match k.lock().unwrap().clone() {
                                None => (
                                    "403 Forbidden",
                                    etag,
                                    r#"{"message":"Must have push access to view repository collaborators."}"#
                                        .to_string(),
                                ),
                                Some(_) if if_none_match.as_deref() == Some(etag.as_str()) => {
                                    ("304 Not Modified", etag, String::new())
                                }
                                Some(list) => ("200 OK", etag, Value::Array(list).to_string()),
                            }
                } else if let Some(pull) = path
                    .strip_prefix("/repos/o/r/pulls/")
                    .and_then(|n| n.parse::<u64>().ok())
                    .and_then(|n| pl.lock().unwrap().get(&n).cloned())
                {
                    ("200 OK", "\"pr\"".to_string(), pull.to_string())
                } else if let Some(item) = path
                    .strip_prefix("/repos/o/r/issues/")
                    .and_then(|n| n.parse::<u64>().ok())
                    .and_then(|n| i.lock().unwrap().get(&n).cloned())
                {
                    // A null stands for an item GitHub does not have.
                    if item.is_null() {
                        (
                            "404 Not Found",
                            "\"i\"".to_string(),
                            r#"{"message":"Not Found"}"#.to_string(),
                        )
                    } else {
                        ("200 OK", "\"i\"".to_string(), item.to_string())
                    }
                } else if let Some(n) = path
                    .strip_prefix("/repos/o/r/issues/")
                    .and_then(|rest| rest.strip_suffix("/timeline"))
                    .and_then(|n| n.parse::<u64>().ok())
                {
                    let events = t.lock().unwrap().get(&n).cloned().unwrap_or_default();
                    (
                        "200 OK",
                        "\"t\"".to_string(),
                        Value::Array(events).to_string(),
                    )
                } else {
                    (
                        "500 Internal Server Error",
                        "\"none\"".to_string(),
                        r#"{"message":"the test expected no fetch"}"#.to_string(),
                    )
                };
                let resp = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nETag: {etag}\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });
        Self {
            base,
            hits,
            created,
            created_etag,
            created_fulls,
            assigned,
            timelines,
            issues,
            pulls,
            collaborators,
            collab_version,
            posts,
        }
    }

    /// The comment endpoints posted to since the last call.
    fn posts(&self) -> Vec<String> {
        self.post_bodies().into_iter().map(|(p, _)| p).collect()
    }

    /// The comments posted since the last call: endpoint and body.
    fn post_bodies(&self) -> Vec<(String, String)> {
        std::mem::take(&mut *self.posts.lock().unwrap())
    }

    fn set_collaborators(&self, list: Option<Vec<Value>>) {
        *self.collaborators.lock().unwrap() = list;
        self.collab_version
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    /// Serve one item by number, for the paths that read it directly.
    fn set_issue(&self, number: u64, item: Value) {
        self.issues.lock().unwrap().insert(number, item);
    }

    /// Answer 404 for one item, as GitHub does for one that was
    /// deleted, or that this token may not read any more.
    fn set_missing(&self, number: u64) {
        self.issues.lock().unwrap().insert(number, Value::Null);
    }

    /// Serve one pull request by number. A number never set answers
    /// 500, so a test can say the fetch failed.
    fn set_pull(&self, number: u64, pull: Value) {
        self.pulls.lock().unwrap().insert(number, pull);
    }

    fn set_timeline(&self, number: u64, events: Vec<Value>) {
        self.timelines.lock().unwrap().insert(number, events);
    }

    fn set_assigned(&self, items: Vec<Value>) {
        *self.assigned.lock().unwrap() = items;
    }

    /// The request paths since the last call.
    fn hits(&self) -> Vec<String> {
        std::mem::take(&mut *self.hits.lock().unwrap())
    }

    /// How many full creator listings have been served so far.
    fn created_fulls(&self) -> u32 {
        self.created_fulls.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn bump_created_etag(&self) {
        self.created_etag
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

fn engine_at(api_url: &str) -> Engine {
    let mut e = engine();
    e.gh = GitHub::new(api_url, "t").unwrap();
    e
}

// Every hit is one of the four listings: nothing was fetched by number.
fn assert_listings_only(hits: &[String]) {
    let fetched: Vec<&String> = hits
        .iter()
        .filter(|h| {
            let path = h.split('?').next().unwrap_or("");
            path != "/repos/o/r/issues" && path != "/repos/o/r/pulls"
        })
        .collect();
    assert!(fetched.is_empty(), "fetched by number: {fetched:?}");
    assert_eq!(hits.len(), 4, "four listings expected: {hits:?}");
}

// Replays issue #34: the ignore records used to live only in memory
// while the listing ETags are persisted, so after a daemon restart
// every listing answered 304 until something changed, and the first
// pass that saw a change fetched every ignored item (issue and
// timeline) again to find nothing new. An ignored item is fetched
// again only when its `updated_at` moves or it appears on a listing
// it was not on before: not for a full listing with unchanged content
// (GitHub rolled its ETag over), not for a 304 with another listing
// changed, and not after a restart.

// Replays issue #141: `forget_etags` armed the `refetch` flag and
// `tick_repo` spent the flag by calling `forget_etags` again, which
// armed it afresh, so one `unblock` made the repository fetch all
// four listings in full on every pass for the life of the daemon.
// One unblock owes exactly one full fetch, and the pass after it is
// back to conditional requests.

// Resetting the owed `refetch` with `tick`'s per-pass state loses the
// full listing after a session comes back late in the preceding pass.
// Exercise the public pass boundary with cached ETags restored: the
// owed pass is full once, and the pass after it is conditional again.

// The case the `refetch` flag exists for at the repository-pass level:
// a session that comes back after the pass has read its listings
// (`reconcile_issue`, rather than `check_logins`). That pass
// stores the ETags it read at its end, putting back the ones the
// unblock cleared, so only the flag can make the next pass a full one
// — and only the next one.

// A bot-opened item with no usable origin tag is onboarded once and
// then skipped, not re-onboarded on every pass. This is what issue
// #138 suspected was broken; it was not, and this guards the path it
// named (it passes without the rest of this change).

// A bot-opened item with no usable origin tag, on a stub that serves
// it from the `creator` listing and by number.
fn untagged_listed(n: u64, state: &str) -> Value {
    json!({
        "number": n, "title": "t", "body": "no tag here",
        "html_url": format!("https://gh/{n}"), "state": state,
        "user": {"login": "bot"}, "created_at": "x", "updated_at": "x"
    })
}

// Issue #138: the daemon onboarded the same 21 bot-opened items every
// half hour. The rejection records the ignore (and did before this),
// but the prune at the end of a pass dropped every record whose item
// was missing from the listings that pass, and GitHub's filtered
// listings come back short now and then — `retire_issue` has guarded
// sessions against exactly that lag from the start. So: a listing
// that comes back short costs no re-onboarding when it recovers.

// The other half of the same prune: an item that is really gone loses
// its record, and an absence is asked about once rather than on every
// pass it lasts.

// The record of an item GitHub 404s for (deleted, or no longer
// readable with this token) goes; a fetch that fails
// answers nothing, so that record stands and is not asked about
// again until the backoff is up.

// More absent records than a pass looks at: the looks are rationed
// and rotate, so every record's turn comes round, while giving up
// costs no request and so waits for nothing.

// A clock in the record that cannot be read — a hand-edited state
// file, or a machine clock that went backwards — is replaced rather
// than believed. `age` reads an unreadable stamp as "just now",
// which would freeze both the look and the giving up for ever.

// A record that names two listings is not re-onboarded when one of
// them comes back short: the item is still on the other, so the
// prune never sees it, and only a listing it has *joined* counts as
// a change (issue #138, which the trigger-set comparison would
// otherwise reintroduce for every gate refusal an item collects two
// triggers from).

// The prune asks whether the item is still there, not whose it is, so
// a record made by the gate (`refuse`) survives a short listing too:
// an item a stranger mentioned the bot on is neither assigned to the
// bot nor opened by it, and used to lose its record on the first
// listing that came back without it.

// A workspace closed by hand leaves its checkout on disk. Purge used
// to call that "already gone" and forget the record, leaving the
// directory, and whatever only it held, for nobody to find.

// ---- a driver switch --------------------------------------------------

// Replays issue #105: after `driver` went from Orca to herdr, an item
// from before the switch still carried Orca's repo id, which the herdr
// driver took for a checkout path. Each delivery failed on it, and the
// item recovered only once five failures had it re-onboarded. Now the
// first delivery re-creates the workspace on the current driver.

// A record from before the driver was written down whose repo id fits
// the current driver is left alone: no re-creation for its own sake.
// ---- sessions blocked on a login ------------------------------------

const LOGIN_SCREEN: &[&str] = &[
    "❯ [ssf] New activity on #5:",
    "",
    "  Login expired · Please run /login",
    "",
    "❯ ",
    "  ⏵⏵ bypass permissions on (shift+tab to cycle)",
];
const READY_SCREEN: &[&str] = &["⏺ Done.", "", "❯ ", "  ⏵⏵ bypass permissions on"];
// Pi's sign-in prompt, which reads nothing like Claude Code's.
const PI_LOGIN_SCREEN: &[&str] = &["  Use /login to log into a provider", "❯ "];

// An engine on the stub driver with item 5 seeded on workspace `w5`,
// its agent live in terminal `t5` showing `screen`.
fn blocked_setup(stub: &GitHubStub, screen: &[&str]) -> (Engine, crate::driver::StubDriver) {
    let mut e = engine_at(&stub.base);
    let d = crate::driver::StubDriver::new(DriverKind::Orca);
    e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
    e.cfg.repos = vec![repo()];
    seeded(&mut e, 5, Some("bot/issue-5"), true);
    let st = e.entry(&repo(), 5);
    st.title = "Fix the widget".into();
    st.html_url = "https://gh/5".into();
    st.worktree_id = Some("w5".into());
    st.worktree_path = Some("/w/5".into());
    st.terminal_handle = Some("t5".into());
    st.agent_session_id = Some("sess-5".into());
    st.updated_at = Some("u1".into());
    d.seed("w5", "t5", screen);
    (e, d)
}

// A new answer from the login check; `tick` clears the per-pass cache
// but these tests drive `tick_repo` directly.
fn probe_returning(e: &mut Engine, state: LoginState, fingerprint: Option<&str>) {
    e.probes.clear();
    let fp = fingerprint.map(str::to_string);
    e.probe = std::sync::Arc::new(move |_| Probe {
        state,
        detail: "test".into(),
        fingerprint: fp.clone(),
    });
}

// The `attached` post on onboarding: one per start, with the launch
// as configured, and never again for a pass or a daemon restart that
// finds the item as it was.

// An item bound to another item's session hears which one took it.

// A daemon event post on a timeline reaches no agent: not the item's
// own session, not a subscriber.

// `event_comments` off, for the instance or the repository, posts
// nothing and changes nothing else.

// A workspace removed by `ssf release` or `ssf purge` is told of on
// the owning item, once, with who did it; one already gone is not.

// The fifth failure in a row drops the binding and says so once,
// with the error on one line.

// A relaunch that is not the target's own onboarding (a bound item's
// delivery bringing back an owner whose binding was given up) is a
// `resumed` on the owner like any other.

// An item onboarded onto a workspace it already had (its binding was
// dropped, or the state file was lost) is told it was attached again
// to a kept workspace, once: the relaunch inside is not a `resumed`.

// A workspace that is gone at delivery time is re-created and the
// item told, with `workspace gone` as the reason.

// A pull request bound to a session's workspace says so as one.

// An item a session handed off gets a session of its own, and its
// `attached` names the parent.

// ---- the resume path (#131) -------------------------------------------

fn resumed_block(conversation: &str) -> String {
    format!(
        "🤖 ssf <!-- ssf: origin=o/r#5 event=resumed -->\n\n\
             ```ssf\n\
             ssf resuming agent on issue:\n\
             harness: Claude Code\n\
             conversation: {conversation}\n\
             after: lost terminal\n\
             ```"
    )
}

// The engine's side of #131 (the driver's decision itself is
// `herdr::resume_verdict`, tested there): a delivery the driver
// reports as resumed is the one launch there is, the conversation id
// is kept, and the block says `resumed`.

// A delivery the driver reports as fresh after a resume it gave up
// on: the conversation id goes, the fresh harness gets the whole
// story, and the block says `fresh` because that is what happened.

// The shape #133 asks a test for: the driver kept an agent that was
// alive when the wait ran out, and the engine ends with one handle,
// `resumed`, the conversation id kept and no second launch. (The
// stub's `Unsettled` and `Settles` reach the engine as the same
// delivery; the difference is the driver's, in `resume_verdict`.)

// The startup pass says `after: restart`; a relaunch at delivery
// time says `after: lost terminal`.

// Every text ssf puts on a screen or that agents read must stay free
// of the phrases `driver::login_dialog` looks for, or a healthy
// session would be blocked again by its own echo.
// A launch for the texts checked below; the harness is what is
// under test.
fn handover_launch(harness: &str) -> events::Launch {
    events::Launch {
        harness: harness.to_string(),
        model: None,
        effort: None,
        command: None,
        driver: "herdr".into(),
        branch: Some("refs/heads/bot/issue-5".into()),
    }
}

// ---- handovers ------------------------------------------------------

// An item ready to be handed over: item 5 on `w5` with a live agent,
// and enough on the GitHub stub for the new session's story.

fn handover_setup(stub: &GitHubStub) -> (Engine, crate::driver::StubDriver) {
    let (e, d) = blocked_setup(stub, READY_SCREEN);
    stub.set_issue(5, assigned_item(5, "alice", "u1"));
    stub.set_timeline(5, vec![assigned_by(1, "alice")]);
    (e, d)
}

// The whole path of `ssf handover` with a summary: recorded
// synchronously, carried out on the next pass in the same workspace,
// with the two posts and the overrides left on the item.

// Without a summary, and asked for by a person at a shell: the post
// says both, and the new session is told to read the item.

// The new session is told the item's whole story, so what happened
// between the request and the pass is in that first message and is
// not delivered to it a second time by the pass that follows.

// Every launch of the item after a handover uses its overrides: the
// re-created workspace, and the startup pass.

// The same harness with another model: the repository's own command
// still starts the agent, the effort the repository set carries over,
// and the post names the command both ends run under (without it,
// `the command's` in the model line refers to nothing).

// A handover asked for on an item bound to another session's
// workspace is the owning session's: one workspace, one harness in
// it, and the bound item shows what its owner runs.

// Each synchronous refusal, with its reason.

// A handover the pass cannot carry out: nothing changes, the item
// says so, and the agent that asked is told to carry on.

// The daemon restarting between the request and the pass changes
// nothing: the pending handover is in the state file.

// The new harness comes up at its own sign-in prompt: the session is
// blocked as any other, and the old one is not brought back.

// The new harness cannot be started at all (a model id it refuses,
// a binary that exits at once): the handover stands, the item is
// blocked with the usual post and the usual recovery, and no
// `attached` claims a session that is not there.

// `ssf handover --cancel`: the way back out of a pending handover,
// which otherwise refuses every other command on the item.

// A session blocked on its harness's sign-in prompt may hand over --
// that is a way out of the block -- and the hold on the item is
// closed when it does, rather than standing over a session that is
// no longer there.

// A harness that would not start and is not signed in where the
// daemon runs is recorded as the sign-in block it really is: that is
// the thing to fix, and the recovery from #85 is the one that fits.

// The summary is the point of a handover, so it outlives a new
// harness that will not come up: it waits on the item until a
// session has read it, and the restart carries it.

// A `start` that failed on a harness that is running all the same
// (the pane came up but never settled): the block is not lifted on
// the screen alone, the session is given what it was never told.

// A handover whose harness comes up at its sign-in prompt: the first
// message went into that screen, so nothing has read the summary.
// A person signing in at the terminal is not enough to lift the
// block on its own -- the session still has to be told.

// Telling a harness that is running costs a read of the item, so it
// is not tried on every pass while it fails: the attempt is noted
// and the next one waits for the backoff.

// The telling waits on its own backoff, not the restart's: a
// restart that came back to the prompt a moment ago says nothing
// about a person who has just signed in at the pane it left, and
// that person is answered on the next pass.

// The pane dies as the message goes out and the harness started in
// its place comes up at a sign-in screen: what that screen said is
// the block from now on, but it is the same hold -- reported once,
// held from when it began, with both backoffs where they were.

// A second handover that brings no summary of its own does not
// destroy the one still waiting: the session that wrote it is long
// gone, and the harness starting now is the first that can act on
// it. One that does bring a summary replaces it (the newer account
// of where the item stands), which is what the test above shows.

// A second handover on an item whose first one never ran: the words
// the new session is given still name the session that did the work,
// not the harness that failed to come up.

// What a handover retires: the conversation on the record and the
// last one its harness wrote in the workspace. The second covers the
// session ssf never captured an id for, and the transcript flushed
// on the way out inside the second `handed_over_at` is stamped to.

// Where `capture_sessions` starts looking for a transcript: a moment
// before the launch, but never back past a handover, whose outgoing
// agent wrote its own transcript in those same seconds.

fn assigned_item(number: u64, author: &str, updated_at: &str) -> Value {
    json!({
        "number": number, "title": "t", "body": "do it", "html_url": format!("https://gh/{number}"),
        "state": "open", "user": {"login": author}, "assignees": [{"login": "bot"}],
        "created_at": "x", "updated_at": updated_at
    })
}

fn assigned_by(id: u64, who: &str) -> Value {
    json!({"event":"assigned","id":id,"actor":{"login":who},"assignee":{"login":"bot"},"created_at":"t"})
}

// A stamp far enough in the past that the paced re-check runs, while
// still leaving a hold for the retirement to clear.
const EXPIRED: &str = "2026-01-01T00:00:00Z";

// Replays issue #137: the mentioned listing came back without an item
// whose mention was still sitting in the issue, so the session was
// told to stop and reattached on the next pass, over and over.
// Retirement re-reads the item now, so a listing that loses it
// changes nothing while the mention is there, and the session still
// retires once the mention has really gone.

// Only the paced arm keeps the bookkeeping. An item held on something
// read straight off it clears the hold, so an assignment cannot
// suppress a mention re-check that has never run.

// A hold paces the timeline walk and nothing else. A close is still
// noticed on the next pass, an expired hold reads the item again, and
// a stamp ahead of the clock counts as expired rather than holding
// until wall-clock catches up.

// An item back on a listing settles whatever a hiccup held, so a
// later hold is a new incident rather than a stamp that never moves.

// The review-request arm of the same guard, and what a failed
// re-check does: a fetch that says nothing holds the retirement,
// because retiring is the destructive reading of missing evidence.

// The refusal names a remedy that fits the item. Unassigning helps
// only an item that is assigned, and a mention cannot be withdrawn.

mod access_and_conflicts;
#[path = "events/mod.rs"]
mod event_tests;
mod handovers;
mod listings;
#[path = "login.rs"]
mod login_tests;
mod releases;
mod state;
