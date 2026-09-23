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
