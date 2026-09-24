use super::*;
use crate::engine::implementation::scratch::new_scratch_id;
use crate::state::ScratchState;

/// A scratch session `o/r~k3f9` with a workspace at `path`.
fn seed_scratch(e: &mut Engine, r: &RepoConfig, path: &str) {
    e.state
        .repos
        .entry(r.name.clone())
        .or_default()
        .scratch
        .insert(
            "k3f9".into(),
            ScratchState {
                id: "k3f9".into(),
                owner_login: Some("alice".into()),
                created_at: "x".into(),
                worktree_id: Some("w9".into()),
                worktree_path: Some(path.into()),
                branch: Some("scratch/k3f9".into()),
                terminal_handle: Some("t9".into()),
                agent_session_id: Some("conv-1".into()),
                ..Default::default()
            },
        );
}

fn scratch_state(e: &Engine, r: &RepoConfig) -> ScratchState {
    e.state.repos[&r.name].scratch["k3f9"].clone()
}

#[test]
fn scratch_ids_are_short_and_avoid_those_taken() {
    let id = new_scratch_id(|_| false);
    assert_eq!(id.len(), 4);
    assert!(crate::origin::Scratch::new("o/r", &id).is_some(), "{id}");
    let first = id.clone();
    let other = new_scratch_id(|c| c == first);
    assert_ne!(other, first);
}

#[tokio::test]
async fn scratch_release_is_guarded_and_keeps_the_conversation() {
    use crate::release::testkit::scratch;

    let clean = scratch("scratch-release").await;
    let mut e = engine();
    let d = crate::driver::StubDriver::new(DriverKind::Herdr);
    e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
    let r = repo();
    e.cfg.repos = vec![r.clone()];
    seed_scratch(&mut e, &r, &clean.work);
    d.seed("w9", "t9", READY_SCREEN);

    // Unsaved work refuses an ordinary release and reports why.
    std::fs::write(std::path::Path::new(&clean.work).join("b.txt"), "b\n").unwrap();
    let refused = e
        .handle_request(Request::Release {
            session: "o/r~k3f9".into(),
            force: false,
        })
        .await;
    assert!(refused.ok, "{:?}", refused.error);
    assert_eq!(refused.data["released"], false);
    assert_eq!(refused.data["check"]["safe"], false);
    assert!(!scratch_state(&e, &r).release_pending);
    e.run_scratch_cleanups(&r).await;
    assert!(d.log().is_empty(), "nothing removed without the guard");

    // A clean checkout is released on the next pass.
    std::fs::remove_file(std::path::Path::new(&clean.work).join("b.txt")).unwrap();
    let accepted = e
        .handle_request(Request::Release {
            session: "o/r~k3f9".into(),
            force: false,
        })
        .await;
    assert!(accepted.ok, "{:?}", accepted.error);
    assert_eq!(accepted.data["pending"], true);
    assert_eq!(accepted.data["check"]["safe"], true);
    e.run_scratch_cleanups(&r).await;
    assert_eq!(d.log(), vec!["remove:w9"]);
    let st = scratch_state(&e, &r);
    assert!(st.worktree_id.is_none());
    assert!(st.released_at.is_some());
    // The record, its branch and its conversation stay for a resume.
    assert_eq!(st.agent_session_id.as_deref(), Some("conv-1"));
    assert_eq!(st.branch.as_deref(), Some("scratch/k3f9"));

    // Unknown scratch sessions are refused as conflicts.
    let unknown = e
        .handle_request(Request::Release {
            session: "o/r~zzzz".into(),
            force: false,
        })
        .await;
    assert!(!unknown.ok);
    assert_eq!(unknown.kind, Some(crate::ipc::RefusalKind::Conflict));
}

#[tokio::test]
async fn scratch_sessions_subscribe_and_appear_in_status() {
    let stub = GitHubStub::start().await;
    stub.set_issue(
        5,
        json!({"number": 5, "title": "Five", "body": "", "html_url": "https://gh/5",
               "state": "open", "user": {"login": "alice"},
               "created_at": "x", "updated_at": "x"}),
    );
    let mut e = engine_at(&stub.base);
    let r = repo();
    e.cfg.repos = vec![r.clone()];
    seed_scratch(&mut e, &r, "/nonexistent/scratch");
    let resp = e
        .handle_request(Request::Sub {
            from: "o/r~k3f9".into(),
            target: "o/r#5".into(),
            events: crate::state::Events::All,
        })
        .await;
    assert!(resp.ok, "{:?}", resp.error);
    let resp = e
        .handle_request(Request::Sub {
            from: "o/r~nope".into(),
            target: "o/r#5".into(),
            events: crate::state::Events::All,
        })
        .await;
    assert!(!resp.ok);

    let sessions = crate::status::sessions_with(&e.cfg, &e.state, &[], &[]);
    let s = sessions.iter().find(|s| s.id == "o/r~k3f9").unwrap();
    assert_eq!(s.kind, "scratch");
    assert_eq!(s.owner_login.as_deref(), Some("alice"));
    assert_eq!(s.label(), "~k3f9");
    let v = serde_json::to_value(s).unwrap();
    assert_eq!(v["kind"], "scratch");
    assert_eq!(v["owner_login"], "alice");
    assert_eq!(v["branch"], "scratch/k3f9");
}

#[tokio::test]
async fn a_scratch_session_is_not_told_while_it_is_being_released() {
    let mut e = engine();
    let d = crate::driver::StubDriver::new(DriverKind::Herdr);
    e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
    let r = repo();
    e.cfg.repos = vec![r.clone()];
    seed_scratch(&mut e, &r, "/nonexistent/scratch");
    d.seed("w9", "t9", READY_SCREEN);
    e.state
        .repos
        .get_mut(&r.name)
        .unwrap()
        .scratch
        .get_mut("k3f9")
        .unwrap()
        .release_pending = true;
    let err = e.deliver_scratch(&r, "k3f9", "hi", None).await.unwrap_err();
    assert!(format!("{err:#}").contains("being released"), "{err:#}");
    assert!(d.log().is_empty(), "nothing relaunched");
}

#[test]
fn a_scratch_session_does_not_get_its_own_posts_back() {
    let mut e = engine();
    e.cfg.repos = vec![repo()];
    let rendered = |key: &str, origin: Option<&str>| crate::prompt::Rendered {
        key: key.into(),
        text: String::new(),
        origin: origin.map(str::to_string),
        assignee: None,
        state_change: false,
    };
    let events = vec![
        rendered("commented:1", Some("o/r~k3f9")),
        rendered("commented:2", Some("o/r#1")),
        rendered("commented:3", None),
    ];
    let keys = |v: Vec<crate::prompt::Rendered>| v.into_iter().map(|r| r.key).collect::<Vec<_>>();
    assert_eq!(
        keys(e.for_recipient(&events, "o/r~k3f9", OwnPosts::Hidden)),
        vec!["commented:2", "commented:3"]
    );
    assert_eq!(
        keys(e.for_recipient(&events, "o/r#1", OwnPosts::Hidden)).len(),
        2
    );
}

#[tokio::test]
async fn a_new_scratch_session_is_sent_no_first_prompt() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let mut e = engine_at(&stub.base);
    let d = crate::driver::StubDriver::new(DriverKind::Herdr);
    e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
    let r = repo();
    e.cfg.repos = vec![r.clone()];
    e.create_scratch(&r.name, "claude", None, None, None)
        .await
        .unwrap();
    assert_eq!(d.prompts(), vec![String::new()]);
    let st = e.state.repos[&r.name].scratch.values().next().unwrap();
    assert_eq!(st.prompts_sent, 0);
}
