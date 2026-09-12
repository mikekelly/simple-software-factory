use super::*;

#[tokio::test]
async fn conflict_simulation_allows_clean_divergence() {
    use crate::release::testkit::{scratch, sh};

    let s = scratch("clean-merge-tree").await;
    let (path, _) = crate::driver::add_local_worktree(&s.work, "issue-3-clean", None)
        .await
        .unwrap();
    std::fs::write(std::path::Path::new(&path).join("feature.txt"), "feature\n").unwrap();
    sh(&path, &["add", "feature.txt"]).await;
    sh(&path, &["commit", "-q", "-m", "feature"]).await;
    std::fs::write(std::path::Path::new(&s.work).join("base.txt"), "base\n").unwrap();
    sh(&s.work, &["add", "base.txt"]).await;
    sh(&s.work, &["commit", "-q", "-m", "base"]).await;
    sh(&s.work, &["push", "-q", "origin", "main"]).await;
    let base = conflict_git(&s.work, &["rev-parse", "refs/remotes/origin/main"])
        .await
        .unwrap();
    let head = conflict_git(&path, &["rev-parse", "HEAD"]).await.unwrap();
    assert_eq!(
        engine()
            .simulate_conflict(&s.work, &base, &head)
            .await
            .unwrap(),
        (false, Vec::new())
    );
}
#[tokio::test]
async fn conflict_check_notifies_once_and_guards_stale_branch_state() {
    use crate::release::testkit::{scratch, sh};

    let s = scratch("conflict-notice").await;
    let (path, branch) = crate::driver::add_local_worktree(&s.work, "issue-2-conflict", None)
        .await
        .unwrap();
    std::fs::write(std::path::Path::new(&path).join("a.txt"), "feature\n").unwrap();
    sh(&path, &["add", "a.txt"]).await;
    sh(&path, &["commit", "-q", "-m", "feature"]).await;
    std::fs::write(std::path::Path::new(&s.work).join("a.txt"), "base\n").unwrap();
    sh(&s.work, &["add", "a.txt"]).await;
    sh(&s.work, &["commit", "-q", "-m", "base"]).await;
    sh(&s.work, &["push", "-q", "origin", "main"]).await;

    let mut e = engine();
    let d = crate::driver::StubDriver::new(DriverKind::Orca);
    e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
    e.cfg.daemon.conflict_check_interval_secs = 1;
    let mut r = repo();
    r.path = Some(s.work.clone());
    d.seed("w2", "t2", READY_SCREEN);
    {
        let st = e.entry(&r, 2);
        st.html_url = "https://gh/2".into();
        st.seeded = true;
        st.active = true;
        st.worktree_id = Some("w2".into());
        st.worktree_path = Some(path.clone());
        st.repo_id = Some(s.work.clone());
        st.branch = Some(branch);
    }

    e.check_conflicts(&r).await.unwrap();
    let first = d.prompts();
    assert_eq!(first.len(), 1);
    assert!(first[0].contains("origin/main"));
    assert!(first[0].contains("a.txt"));
    assert!(first[0].contains("rebase"));
    assert!(e.entry(&r, 2).conflict_notice.is_some());

    // The interval is deliberately bypassed here to exercise persisted
    // pair deduplication as each daemon pass would see it.
    e.conflict_checks.clear();
    e.check_conflicts(&r).await.unwrap();
    assert!(d.prompts().is_empty(), "the same divergence was repeated");

    // A stale state branch must never make the daemon inspect or notify
    // about a different branch checked out in the agent worktree.
    sh(&path, &["checkout", "-q", "-b", "other"]).await;
    e.conflict_checks.clear();
    e.check_conflicts(&r).await.unwrap();
    assert!(d.prompts().is_empty(), "stale branch state caused a notice");
}
#[tokio::test]
async fn failed_conflict_delivery_is_retried() {
    let (mut e, r, d, _scratch, _path) = conflict_fixture("conflict-retry", 5).await;
    d.with(|s| {
        s.deliver_error = Some("delivery failed".into());
    });

    e.check_conflicts(&r).await.unwrap();
    assert!(e.entry(&r, 5).conflict_notice.is_none());
    let _ = d.prompts();

    // Once delivery is available again, the same pair must be delivered
    // and recorded.
    e.conflict_checks.clear();
    e.check_conflicts(&r).await.unwrap();
    assert!(e.entry(&r, 5).conflict_notice.is_some());
    assert_eq!(d.prompts().len(), 1);
}
#[tokio::test]
async fn conflict_notice_survives_reload_even_with_an_empty_merge_cache() {
    let (mut e, r, d, scratch, path) = conflict_fixture("conflict-reload", 6).await;
    e.check_conflicts(&r).await.unwrap();
    assert!(e.entry(&r, 6).conflict_notice.is_some());
    let _ = d.prompts();

    let saved = serde_json::to_string(&e.state).unwrap();
    let mut restarted = engine();
    restarted.cfg.daemon.conflict_check_interval_secs = 1;
    let d2 = crate::driver::StubDriver::new(DriverKind::Orca);
    restarted.drivers = Drivers::from_list(vec![Driver::Stub(d2.clone())]);
    d2.seed("w6", "t6", READY_SCREEN);
    restarted.state = serde_json::from_str(&saved).unwrap();
    assert!(restarted.conflict_pairs.is_empty());
    restarted.check_conflicts(&r).await.unwrap();
    assert!(d2.prompts().is_empty());
    assert_eq!(
        restarted.entry(&r, 6).worktree_path.as_deref(),
        Some(path.as_str())
    );
    drop(scratch);
}
#[tokio::test]
async fn changed_branch_or_base_commit_is_a_new_conflict_notice() {
    use crate::release::testkit::sh;

    let (mut e, mut r, d, scratch, path) = conflict_fixture("conflict-changed", 7).await;
    r.base_branch = Some("origin/HEAD".into());
    e.check_conflicts(&r).await.unwrap();
    let _ = d.prompts();

    std::fs::write(std::path::Path::new(&path).join("extra.txt"), "extra\n").unwrap();
    sh(&path, &["add", "extra.txt"]).await;
    sh(&path, &["commit", "-q", "-m", "extra"]).await;
    e.conflict_checks.clear();
    e.check_conflicts(&r).await.unwrap();
    assert_eq!(d.prompts().len(), 1, "changed branch SHA was not noticed");

    std::fs::write(
        std::path::Path::new(&scratch.work).join("base-extra.txt"),
        "base\n",
    )
    .unwrap();
    sh(&scratch.work, &["add", "base-extra.txt"]).await;
    sh(&scratch.work, &["commit", "-q", "-m", "base-extra"]).await;
    sh(&scratch.work, &["push", "-q", "origin", "main"]).await;
    e.conflict_checks.clear();
    e.check_conflicts(&r).await.unwrap();
    assert_eq!(d.prompts().len(), 1, "changed base SHA was not noticed");
}
#[tokio::test]
async fn inactive_session_states_do_not_fetch_or_notify() {
    let mut e = engine();
    let d = crate::driver::StubDriver::new(DriverKind::Orca);
    e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
    e.cfg.daemon.conflict_check_interval_secs = 1;
    let mut r = repo();
    r.path = Some("/no/such/checkout".into());
    for number in 10..=15 {
        d.seed(&format!("w{number}"), &format!("t{number}"), READY_SCREEN);
        let st = e.entry(&r, number);
        st.seeded = true;
        st.active = true;
        st.worktree_id = Some(format!("w{number}"));
        st.worktree_path = Some("/no/such/worktree".into());
        st.branch = Some(format!("refs/heads/bot/issue-{number}"));
    }
    e.entry(&r, 10).retired_at = Some(now_iso());
    e.entry(&r, 11).released_at = Some(now_iso());
    e.entry(&r, 12).blocked = Some(Blocked {
        reason: Blocked::LOGIN.into(),
        since: now_iso(),
        ..Default::default()
    });
    e.entry(&r, 13).handover = Some(PendingHandover {
        harness: "claude".into(),
        ..Default::default()
    });
    e.entry(&r, 14).shares_workspace_of = Some(10);
    e.entry(&r, 15).active = false;
    e.check_conflicts(&r).await.unwrap();
    assert!(d.prompts().is_empty());
    assert!(
        e.conflict_checks.is_empty(),
        "ineligible records triggered a fetch"
    );
}
#[tokio::test]
async fn disabled_conflict_checks_do_not_fetch() {
    let (mut e, r, d, _scratch, _path) = conflict_fixture("conflict-disabled", 16).await;
    e.cfg.daemon.conflict_check_interval_secs = 0;
    e.check_conflicts(&r).await.unwrap();
    assert!(d.prompts().is_empty());
    assert!(e.conflict_checks.is_empty());
}
#[tokio::test]
async fn failed_base_fetch_does_not_use_a_stale_remote_commit() {
    use crate::release::testkit::sh;

    let (mut e, r, d, scratch, _path) = conflict_fixture("conflict-fetch-fails", 17).await;
    sh(
        &scratch.work,
        &["remote", "set-url", "origin", "/no/such/origin.git"],
    )
    .await;
    assert!(e.check_conflicts(&r).await.is_err());
    assert!(e.entry(&r, 17).conflict_notice.is_none());
    assert!(d.prompts().is_empty());
}
#[tokio::test]
async fn conflict_interval_skips_a_second_fetch() {
    use crate::release::testkit::sh;

    let (mut e, r, d, scratch, _path) = conflict_fixture("conflict-interval", 18).await;
    e.cfg.daemon.conflict_check_interval_secs = 3600;
    e.check_conflicts(&r).await.unwrap();
    let _ = d.prompts();
    sh(
        &scratch.work,
        &["remote", "set-url", "origin", "/no/such/origin.git"],
    )
    .await;
    // A fetch here would fail; the configured interval makes this pass a
    // no-op, proving one fetch per repository interval.
    e.check_conflicts(&r).await.unwrap();
    assert!(d.prompts().is_empty());
}
