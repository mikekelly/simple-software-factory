use super::*;

// #641: CI on a session's pull request is delivered once when its checks
// start and once when they settle, per head commit, and a re-run says
// something again only when it ends differently.

const SHA: &str = "aaaaaaa1111";
const NEXT: &str = "bbbbbbb2222";

/// A session on pull request #5 whose head is at `sha`.
fn on_pr(stub: &GitHubStub, sha: &str) -> (Engine, crate::driver::StubDriver) {
    let (mut e, d) = blocked_setup(stub, READY_SCREEN);
    e.entry(&repo(), 5).pr = Some(pr("bot/issue-5"));
    let mut item = assigned_item(5, "alice", "u1");
    item["pull_request"] = json!({});
    stub.set_issue(5, item);
    push(stub, sha);
    (e, d)
}

fn push(stub: &GitHubStub, sha: &str) {
    stub.set_pull(
        5,
        json!({"number": 5, "head": {"ref": "bot/issue-5", "sha": sha,
               "repo": {"full_name": "o/r"}}, "base": {"ref": "main"}}),
    );
}

async fn poll(e: &mut Engine) {
    e.watch_checks(&repo(), "o", "r").await.unwrap();
}

#[tokio::test]
async fn a_pass_is_said_once_and_not_again_after_a_restart() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let (mut e, d) = on_pr(&stub, SHA);
    stub.set_ci(
        SHA,
        &[
            ("build", "completed", Some("success")),
            ("lint", "completed", Some("skipped")),
        ],
        &[("ci/legacy", "success")],
    );
    poll(&mut e).await;
    let prompts = d.prompts();
    assert_eq!(prompts.len(), 1, "{prompts:?}");
    let p = &prompts[0];
    assert!(p.starts_with("[ssf] New activity on #5"), "{p}");
    assert!(p.contains("<new-activity>") && p.contains("<event>"), "{p}");
    assert!(
        p.contains("CI passed on commit `aaaaaaa`: all 3 checks passed"),
        "{p}"
    );
    assert!(!p.contains("CI started"), "complete at first sight: {p}");

    // Unchanged: every read is a 304 and nothing is said.
    let _ = stub.hits();
    poll(&mut e).await;
    assert!(d.prompts().is_empty());
    assert!(
        stub.hits()
            .iter()
            .all(|h| !h.starts_with("/repos/o/r/issues/5"))
    );

    // A restart reads everything again, and says nothing again.
    let mut again = engine_at(&stub.base);
    again.drivers = e.drivers.clone();
    again.cfg.repos = vec![repo()];
    again.state = e.state.clone();
    poll(&mut again).await;
    assert!(d.prompts().is_empty());
}

#[tokio::test]
async fn a_failure_names_each_failing_check_and_links_its_run() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let (mut e, d) = on_pr(&stub, SHA);
    stub.set_ci(
        SHA,
        &[
            ("build", "completed", Some("failure")),
            ("test", "completed", Some("success")),
        ],
        &[("deploy/preview", "error")],
    );
    poll(&mut e).await;
    let prompts = d.prompts();
    assert_eq!(prompts.len(), 1, "{prompts:?}");
    let p = &prompts[0];
    assert!(
        p.contains("CI failed on commit `aaaaaaa`: 2 of 3 checks failed:"),
        "{p}"
    );
    assert!(p.contains("  - build (https://gh/runs/build)"), "{p}");
    assert!(
        p.contains("  - deploy/preview (https://ci/deploy/preview)"),
        "{p}"
    );
    assert!(!p.contains("- test ("), "{p}");
}

#[tokio::test]
async fn pending_checks_say_started_once_then_the_failure() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let (mut e, d) = on_pr(&stub, SHA);
    stub.set_ci(
        SHA,
        &[
            ("build", "in_progress", None),
            ("lint", "completed", Some("success")),
        ],
        &[],
    );
    poll(&mut e).await;
    let prompts = d.prompts();
    assert_eq!(prompts.len(), 1, "{prompts:?}");
    assert!(
        prompts[0].contains("CI started on commit `aaaaaaa`: 1 check running (build)"),
        "{}",
        prompts[0]
    );

    // Still running, with another check queued: nothing more.
    stub.set_ci(
        SHA,
        &[
            ("build", "in_progress", None),
            ("lint", "completed", Some("success")),
            ("e2e", "queued", None),
        ],
        &[],
    );
    poll(&mut e).await;
    assert!(d.prompts().is_empty());

    stub.set_ci(
        SHA,
        &[
            ("build", "completed", Some("failure")),
            ("lint", "completed", Some("success")),
            ("e2e", "completed", Some("success")),
        ],
        &[],
    );
    poll(&mut e).await;
    let prompts = d.prompts();
    assert_eq!(prompts.len(), 1, "{prompts:?}");
    assert!(
        prompts[0].contains("1 of 3 checks failed:\n  - build"),
        "{}",
        prompts[0]
    );
    let notice = e.entry(&repo(), 5).ci_notice.clone().unwrap();
    assert_eq!(notice.sha, SHA);
    assert_eq!(notice.result.as_deref(), Some("fail:build"));
}

#[tokio::test]
async fn a_rerun_speaks_only_when_the_result_changes() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let (mut e, d) = on_pr(&stub, SHA);
    stub.set_ci(SHA, &[("build", "completed", Some("failure"))], &[]);
    poll(&mut e).await;
    assert_eq!(d.prompts().len(), 1);

    // Re-run: running again (no second "started"), then the same failure.
    stub.set_ci(SHA, &[("build", "in_progress", None)], &[]);
    poll(&mut e).await;
    assert!(d.prompts().is_empty());
    stub.set_ci(SHA, &[("build", "completed", Some("failure"))], &[]);
    poll(&mut e).await;
    assert!(d.prompts().is_empty());

    // Another re-run that passes is news.
    stub.set_ci(SHA, &[("build", "completed", Some("success"))], &[]);
    poll(&mut e).await;
    let prompts = d.prompts();
    assert_eq!(prompts.len(), 1, "{prompts:?}");
    assert!(prompts[0].contains("CI passed"), "{}", prompts[0]);
}

#[tokio::test]
async fn a_new_push_starts_afresh_and_no_checks_says_nothing() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let (mut e, d) = on_pr(&stub, SHA);
    stub.set_ci(SHA, &[("build", "completed", Some("success"))], &[]);
    poll(&mut e).await;
    assert_eq!(d.prompts().len(), 1);

    // A push with no CI at all yet: nothing.
    push(&stub, NEXT);
    poll(&mut e).await;
    assert!(d.prompts().is_empty());

    // Its checks start, then pass: both are said, for the new commit.
    stub.set_ci(NEXT, &[("build", "queued", None)], &[]);
    poll(&mut e).await;
    let prompts = d.prompts();
    assert_eq!(prompts.len(), 1, "{prompts:?}");
    assert!(
        prompts[0].contains("CI started on commit `bbbbbbb`"),
        "{}",
        prompts[0]
    );
    stub.set_ci(NEXT, &[("build", "completed", Some("success"))], &[]);
    poll(&mut e).await;
    let prompts = d.prompts();
    assert_eq!(prompts.len(), 1, "{prompts:?}");
    assert!(
        prompts[0].contains("CI passed on commit `bbbbbbb`"),
        "{}",
        prompts[0]
    );
}
