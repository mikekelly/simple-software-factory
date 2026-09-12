use super::prelude::*;

pub(super) async fn status(json: bool) -> Result<()> {
    let snap = status::Snapshot::collect(Config::load()?).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&snap.to_json())?);
    } else {
        print!("{}", status::render_status(&snap));
    }
    Ok(())
}

pub(super) async fn peers(json: bool, repo: Option<String>, all: bool) -> Result<()> {
    let snap = status::Snapshot::collect(Config::load()?).await?;
    let repo = repo.filter(|r| !r.is_empty());
    if let Some(r) = &repo {
        if !snap
            .cfg
            .repos
            .iter()
            .any(|c| c.name.eq_ignore_ascii_case(r))
        {
            bail!("{r} is not a watched repository (see `ssf repo list`)");
        }
    }
    let me = identity(None)?.map(|o| o.to_string());
    let sessions: Vec<status::Session> = snap
        .sessions()
        .into_iter()
        .filter(|s| repo.as_ref().is_none_or(|r| s.repo.eq_ignore_ascii_case(r)))
        .filter(|s| all || s.active)
        .collect();
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "me": me,
                "orca_available": snap.available(),
                "orca_error": snap.error(),
                "sessions": sessions,
            }))?
        );
        return Ok(());
    }
    if let Some(e) = snap.error() {
        eprintln!("driver unavailable, its agent states unknown: {e}");
    }
    if sessions.is_empty() {
        println!(
            "no {}sessions{}",
            if all { "" } else { "active " },
            repo.map(|r| format!(" on {r}")).unwrap_or_default()
        );
        return Ok(());
    }
    print!("{}", status::render_peers(&sessions, me.as_deref()));
    Ok(())
}

/// This session's identity: `--as owner/repo#N`, else the environment
/// `ssf launch` set up.
pub(super) fn identity(as_: Option<&str>) -> Result<Option<origin::Origin>> {
    match as_ {
        Some(a) => origin::Origin::parse(a)
            .map(Some)
            .with_context(|| format!("--as {a}: expected owner/repo#N")),
        None => Ok(origin::Origin::from_env()),
    }
}

/// An item or session argument: `owner/repo#N`, or a bare number on `me`'s
/// repository.
pub(super) fn item_ref(item: &str, me: Option<&origin::Origin>) -> Result<String> {
    let item = item.trim().trim_start_matches('#');
    if let Ok(n) = item.parse::<u64>() {
        return match me {
            Some(o) => Ok(format!("{}#{n}", o.repo)),
            None => {
                bail!("{item}: pass owner/repo#{item}, or --as owner/repo#N to name the repository")
            }
        };
    }
    match origin::Origin::parse(item) {
        Some(o) => Ok(o.to_string()),
        None => bail!("{item}: expected an item number or owner/repo#N"),
    }
}

pub(super) async fn sub(item: &str, as_: Option<&str>, json: bool, subscribe: bool) -> Result<()> {
    let me = identity(as_)?.context(
        "not inside an agent session (SSF_REPO/SSF_ISSUE unset); pass --as owner/repo#N",
    )?;
    let target = item_ref(item, Some(&me))?;
    let me = me.to_string();
    let req = if subscribe {
        ipc::Request::Sub {
            from: me.clone(),
            target: target.clone(),
        }
    } else {
        ipc::Request::Unsub {
            from: me.clone(),
            target: target.clone(),
        }
    };
    let v = ipc::call(&req).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    let title = v.get("title").and_then(|t| t.as_str()).unwrap_or("");
    let who = v.get("subscriber").and_then(|t| t.as_str()).unwrap_or("");
    if subscribe {
        let owner = match v.get("owner").and_then(|o| o.as_str()) {
            Some(o) => format!("owned by {o}"),
            None => "no session of its own; polled for you".into(),
        };
        let added = v.get("added").and_then(|a| a.as_bool()).unwrap_or(true);
        println!(
            "{who} {} {target} \"{title}\" ({owner}); new activity on it will arrive as [ssf] FYI messages.",
            if added {
                "subscribed to"
            } else {
                "was already subscribed to"
            }
        );
    } else {
        let removed = v.get("removed").and_then(|a| a.as_bool()).unwrap_or(true);
        println!(
            "{who} {} {target} \"{title}\"{}",
            if removed {
                "unsubscribed from"
            } else {
                "was not subscribed to"
            },
            if v.get("untracked")
                .and_then(|a| a.as_bool())
                .unwrap_or(false)
            {
                "; nobody follows it now, so it is no longer polled"
            } else {
                ""
            }
        );
    }
    Ok(())
}

pub(super) fn subs(as_: Option<&str>, json: bool) -> Result<()> {
    let cfg = Config::load()?;
    let st = state::State::load()?;
    let me = identity(as_)?.context(
        "not inside an agent session (SSF_REPO/SSF_ISSUE unset); pass --as owner/repo#N",
    )?;
    // The subscriber is always the owning session.
    let me_id = st
        .repos
        .get(&me.repo)
        .map(|rs| {
            let mut cur = me.number;
            let mut hops = 0;
            while let Some(next) = rs.issues.get(&cur).and_then(|s| s.shares_workspace_of) {
                if next == cur || hops > 16 {
                    break;
                }
                cur = next;
                hops += 1;
            }
            status::session_id(&me.repo, cur)
        })
        .unwrap_or_else(|| me.to_string());
    let mut following = Vec::new();
    let mut followers = Vec::new();
    for repo in &cfg.repos {
        let Some(rs) = st.repos.get(&repo.name) else {
            continue;
        };
        for item in rs.issues.values() {
            let id = status::session_id(&repo.name, item.number);
            let owner = if item.subscriber_only {
                None
            } else {
                Some(status::session_id(
                    &repo.name,
                    item.shares_workspace_of.unwrap_or(item.number),
                ))
            };
            if item
                .subscribers
                .iter()
                .any(|s| s.eq_ignore_ascii_case(&me_id))
            {
                following.push(json!({
                    "item": id,
                    "title": item.title,
                    "kind": item.kind,
                    "github_state": item.github_state,
                    "owner": owner,
                }));
            }
            if owner
                .as_deref()
                .is_some_and(|o| o.eq_ignore_ascii_case(&me_id))
                && !item.subscribers.is_empty()
            {
                followers.push(json!({
                    "item": id,
                    "title": item.title,
                    "subscribers": item.subscribers,
                }));
            }
        }
    }
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "me": me_id,
                "subscribed_to": following,
                "subscribers": followers,
            }))?
        );
        return Ok(());
    }
    println!("{me_id} follows:");
    if following.is_empty() {
        println!("  (nothing; `ssf sub <n>` to follow an item)");
    }
    for f in &following {
        println!(
            "  {:<24} {:<7} {}  ({})",
            f["item"].as_str().unwrap_or(""),
            f["github_state"].as_str().unwrap_or("?"),
            f["title"].as_str().unwrap_or(""),
            match f["owner"].as_str() {
                Some(o) => format!("owned by {o}"),
                None => "no session".into(),
            }
        );
    }
    println!("followed by other sessions:");
    if followers.is_empty() {
        println!("  (nobody)");
    }
    for f in &followers {
        let subs: Vec<&str> = f["subscribers"]
            .as_array()
            .map(|a| a.iter().filter_map(|s| s.as_str()).collect())
            .unwrap_or_default();
        println!(
            "  {:<24} {}  <- {}",
            f["item"].as_str().unwrap_or(""),
            f["title"].as_str().unwrap_or(""),
            subs.join(", ")
        );
    }
    Ok(())
}

pub(super) async fn tell(
    item: &str,
    message: Option<String>,
    as_: Option<&str>,
    json: bool,
) -> Result<()> {
    let me = identity(as_)?;
    let target = item_ref(item, me.as_ref())?;
    let text = match message {
        Some(m) => m,
        None => {
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf)?;
            buf
        }
    };
    if text.trim().is_empty() {
        bail!("nothing to say (pass the message, or pipe it in)");
    }
    let v = ipc::call(&ipc::Request::Tell {
        from: me.map(|o| o.to_string()),
        target: target.clone(),
        text,
    })
    .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    println!(
        "delivered to the session on {target} ({}){}",
        v.get("session").and_then(|s| s.as_str()).unwrap_or("?"),
        if v.get("relaunched")
            .and_then(|b| b.as_bool())
            .unwrap_or(false)
        {
            ", whose agent had to be relaunched for it"
        } else {
            ""
        }
    );
    Ok(())
}

pub(super) async fn release(
    item: Option<&str>,
    as_: Option<&str>,
    force: bool,
    json: bool,
) -> Result<()> {
    let me = identity(as_)?;
    let session = match item {
        Some(i) => item_ref(i, me.as_ref())?,
        None => me
            .as_ref()
            .map(|o| o.to_string())
            .context("not inside an agent session (SSF_REPO/SSF_ISSUE unset); name the item, or pass --as owner/repo#N")?,
    };
    // Inside a session `--force` is not the agent's to use: the checks are
    // the whole point. A person passes --as, or runs it from a plain shell.
    if force && as_.is_none() && origin::Origin::from_env().is_some() {
        bail!(
            "--force is for a person who has looked at the workspace: run `ssf release --as {session} --force` from a shell"
        );
    }
    let v = ipc::call(&ipc::Request::Release { session, force }).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&v)?);
        if v.get("released").and_then(|b| b.as_bool()) != Some(true) {
            std::process::exit(1);
        }
        return Ok(());
    }
    let session = v.get("session").and_then(|s| s.as_str()).unwrap_or("?");
    let path = v.get("path").and_then(|s| s.as_str()).unwrap_or("");
    let problems: Vec<&str> = v
        .pointer("/check/problems")
        .and_then(|p| p.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str()).collect())
        .unwrap_or_default();
    if v.get("released").and_then(|b| b.as_bool()) != Some(true) {
        let mut msg = format!(
            "not released: the workspace of {session} ({path}) holds work that is not on origin:\n"
        );
        for p in &problems {
            msg.push_str(&format!("  - {p}\n"));
        }
        msg.push_str(
            "nothing was removed. Commit, push and try again; a kept workspace costs nothing.",
        );
        bail!("{msg}");
    }
    if v.get("already_gone").and_then(|b| b.as_bool()) == Some(true) {
        println!("{session}: the workspace was already gone; recorded as released.");
        return Ok(());
    }
    let secs = v
        .get("poll_interval_secs")
        .and_then(|n| n.as_u64())
        .unwrap_or(10);
    if v.get("forced").and_then(|b| b.as_bool()) == Some(true) {
        println!("{session}: release forced.");
        if !problems.is_empty() {
            println!("Worktree checks bypassed:");
        }
        for p in &problems {
            println!("  - {p}");
        }
    } else {
        println!("{session}: clean and on origin.");
    }
    println!(
        "The workspace ({path}) is removed on the daemon's next pass (within {secs}s), with its terminal. Stop here."
    );
    Ok(())
}

/// `1,234`: a count as the messages write it.
pub(super) fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// What `ssf handover` prints once the daemon has recorded it. The second
/// paragraph is what the outgoing agent acts on, so it says plainly that
/// this session is over. Worded, like every text ssf puts on a screen,
/// without the phrases `driver::login_dialog` looks for.
#[allow(clippy::too_many_arguments)]
pub fn handover_recorded_text(
    session: &str,
    title: &str,
    harness_name: &str,
    model: Option<&str>,
    effort: Option<&str>,
    command: Option<&str>,
    summary_chars: Option<usize>,
    secs: u64,
) -> String {
    // What decides an unset model or effort, in the words the
    // `handed-over` post uses for it (`events::Launch`).
    let default = match command {
        Some(_) => "the command's",
        None => "the harness's default",
    };
    let model = match model {
        Some(m) => format!("model {m}"),
        None => format!("{default} model"),
    };
    let effort = match effort {
        Some(e) => format!("effort {e}"),
        None => format!("{default} effort"),
    };
    let summary = match summary_chars {
        Some(n) => format!("with a summary of {} chars", thousands(n)),
        None => "without a summary".to_string(),
    };
    format!(
        "Handover of {session} (\"{title}\") recorded: to {harness_name} ({model}, {effort}), \
{summary}.\nThe daemon ends this session on its next pass (within {secs}s) and starts the new \
one in the same workspace. Stop working now: do not start anything else, and do not run this \
command again."
    )
}

/// Why a summary that quotes a harness's sign-in screen is refused. The
/// summary is pasted into the new session's terminal, where ssf reads
/// the bottom of the screen for exactly those phrases, so such a summary
/// would hold the new session's deliveries for the whole backoff. `line`
/// is the offending line with the phrases already redacted
/// (`driver::redact_login_phrases`), so this text is safe on a screen
/// itself.
pub fn summary_quotes_a_sign_in_screen_text(line: &str) -> String {
    format!(
        "the summary would read as a harness's own sign-in screen where it says \"{line}\" (the \
phrase is left out here): pasted into the new session's terminal it would hold that session's \
deliveries. Reword that line -- name the command in prose rather than quoting the screen -- and \
hand over again."
    )
}

/// The summary a handover carries: `--summary`, the contents of
/// `--summary-file`, or nothing for `--no-summary`. Checked here, where
/// the person or agent that wrote it can fix it, rather than in the daemon.
pub(super) fn handover_summary(
    summary: Option<String>,
    file: Option<&Path>,
    no_summary: bool,
) -> Result<Option<String>> {
    let text = match (summary, file) {
        (Some(t), _) => t,
        (None, Some(p)) => std::fs::read_to_string(p)
            .with_context(|| format!("reading the summary from {}", p.display()))?,
        (None, None) if no_summary => return Ok(None),
        (None, None) => bail!(
            "say what the new session is told: --summary \"<text>\", --summary-file <path>, or \
--no-summary"
        ),
    };
    if text.trim().is_empty() {
        bail!("the summary is empty: write a summary or pass --no-summary");
    }
    let n = text.chars().count();
    if n > ipc::MAX_SUMMARY_CHARS {
        bail!(
            "the summary is {} characters; the most a handover carries is {}. Shorten it, or say \
the rest on the item",
            thousands(n),
            thousands(ipc::MAX_SUMMARY_CHARS)
        );
    }
    if let Some(line) = driver::login_prompt_line(&text) {
        bail!(
            "{}",
            summary_quotes_a_sign_in_screen_text(&driver::redact_login_phrases(&line))
        );
    }
    Ok(Some(text))
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn handover(
    item: Option<&str>,
    cancel: bool,
    harness: Option<&str>,
    model: Option<&str>,
    effort: Option<&str>,
    summary: Option<String>,
    summary_file: Option<&Path>,
    no_summary: bool,
    as_: Option<&str>,
    json: bool,
) -> Result<()> {
    let me = identity(as_)?;
    let session = match item {
        Some(i) => item_ref(i, me.as_ref())?,
        None => me
            .as_ref()
            .map(|o| o.to_string())
            .context("not inside an agent session (SSF_REPO/SSF_ISSUE unset); name the item, or pass --as owner/repo#N")?,
    };
    if cancel {
        return cancel_handover(&session, json).await;
    }
    let harness = harness.context("--harness is required")?.trim();
    if !agents::is_known(harness) {
        bail!(
            "{harness} is not a harness ssf knows; `ssf agents` lists the ids ({})",
            agents::list()
                .iter()
                .map(|a| a.id.clone())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    models::validate(harness, model, effort)?;
    let summary = handover_summary(summary, summary_file, no_summary)?;
    let v = ipc::call(&ipc::Request::Handover {
        session,
        harness: harness.to_string(),
        model: model.map(str::to_string),
        effort: effort.map(str::to_string),
        summary,
        by: me.map(|o| o.to_string()),
    })
    .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    let s = |p: &str| v.pointer(p).and_then(|x| x.as_str()).map(str::to_string);
    println!(
        "{}",
        handover_recorded_text(
            v.get("session").and_then(|x| x.as_str()).unwrap_or("?"),
            v.get("title").and_then(|x| x.as_str()).unwrap_or(""),
            &login::display_name(s("/to/harness").as_deref().unwrap_or(harness)),
            s("/to/model").as_deref(),
            s("/to/effort").as_deref(),
            s("/to/command").as_deref(),
            v.get("summary_chars")
                .and_then(|x| x.as_u64())
                .map(|n| n as usize),
            v.get("poll_interval_secs")
                .and_then(|n| n.as_u64())
                .unwrap_or(10),
        )
    );
    Ok(())
}

/// `ssf handover --cancel`: the pending handover is dropped and the
/// session that is there keeps the item.
pub(super) async fn cancel_handover(session: &str, json: bool) -> Result<()> {
    let v = ipc::call(&ipc::Request::CancelHandover {
        session: session.to_string(),
    })
    .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    println!(
        "{}",
        handover_cancelled_text(
            &s("session"),
            &s("title"),
            &s("harness_name"),
            v.get("told").and_then(|x| x.as_bool()).unwrap_or(false),
        )
    );
    Ok(())
}

/// What `ssf handover --cancel` prints. Worded, like every text ssf puts
/// on a screen, without the phrases `driver::login_dialog` looks for.
pub fn handover_cancelled_text(session: &str, title: &str, harness: &str, told: bool) -> String {
    let told = if told {
        " The session on it has been told to carry on."
    } else {
        ""
    };
    format!(
        "Handover of {session} (\"{title}\") to {harness} cancelled; nothing about the item \
changed.{told}"
    )
}

pub(crate) async fn purge(
    dry_run: bool,
    older_than: Option<u64>,
    force: bool,
    json: bool,
) -> Result<()> {
    let v = ipc::call(&ipc::Request::Purge {
        dry_run,
        older_than_days: older_than,
        force,
    })
    .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    let rows = v
        .get("workspaces")
        .and_then(|w| w.as_array())
        .cloned()
        .unwrap_or_default();
    if rows.is_empty() {
        println!(
            "no workspaces to purge: none belongs to a closed item{}",
            older_than
                .map(|d| format!(" retired more than {d} days ago"))
                .unwrap_or_default()
        );
        return Ok(());
    }
    let mut removed = 0;
    let mut kept = 0;
    let mut forceable = 0;
    for r in &rows {
        let s = |k: &str| r.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
        let did = r.get("removed").and_then(|b| b.as_bool()).unwrap_or(false);
        let state = s("state");
        let gone = state == "already gone";
        let verb = if did {
            removed += 1;
            if gone { "forgot" } else { "removed" }
        } else if dry_run {
            if gone {
                "would forget"
            } else if state == "clean and pushed" || (force && state != "agent running") {
                "would remove"
            } else {
                "would keep"
            }
        } else {
            kept += 1;
            if state != "agent running" {
                forceable += 1;
            }
            "kept"
        };
        let title = s("title");
        let given_up = r
            .get("release_given_up")
            .and_then(|b| b.as_bool())
            .unwrap_or(false);
        let workspace_gone = s("workspace") == "gone";
        println!(
            "{:<13} {} \"{}\"  [{}]{}{}  {}",
            verb,
            s("session"),
            status::one_line(&title, 50),
            s("state"),
            if workspace_gone {
                " (workspace gone, checkout still on disk)"
            } else {
                ""
            },
            if given_up { " (release given up)" } else { "" },
            s("path")
        );
        if let Some(problems) = r.get("problems").and_then(|p| p.as_array()) {
            for p in problems.iter().filter_map(|p| p.as_str()) {
                println!("              - {p}");
            }
        }
        if let Some(e) = r.get("error").and_then(|e| e.as_str()) {
            println!("              error: {e}");
        }
    }
    if dry_run {
        println!("dry run: nothing was removed.");
    } else {
        println!(
            "{removed} removed, {kept} kept{}.",
            if forceable > 0 && !force {
                "; `ssf purge --force` removes the kept ones too, losing what is in them"
            } else {
                ""
            }
        );
    }
    Ok(())
}
