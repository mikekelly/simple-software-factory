use super::*;

fn invitation(id: u64, repository: &str, inviter: &str) -> Value {
    json!({
        "id": id,
        "repository": {
            "id": id + 100,
            "full_name": repository,
            "clone_url": format!("https://github.com/{repository}.git"),
            "ssh_url": format!("git@github.com:{repository}.git")
        },
        "inviter": {"login": inviter, "id": id + 200, "type": "User"}
    })
}

#[tokio::test]
async fn only_invitations_from_configured_users_are_accepted() {
    let stub = GitHubStub::start().await;
    stub.set_invitations(vec![
        invitation(1, "alice/one", "Alice"),
        invitation(2, "mallory/two", "mallory"),
        json!({
            "id": 4,
            "repository": {
                "id": 104, "full_name": "gone/four",
                "clone_url": "https://github.com/gone/four.git",
                "ssh_url": "git@github.com:gone/four.git"
            },
            "inviter": null
        }),
        invitation(3, "admin/three", "ACME-ADMIN"),
    ]);
    let mut engine = engine_at(&stub.base);
    engine.cfg.github.auto_accept_invitations_from = vec![" @alice ".into(), "acme-admin".into()];

    engine.accept_repository_invitations().await.unwrap();

    assert_eq!(stub.accepted_invitations(), vec![1, 3]);
    assert_eq!(
        stub.hits(),
        vec![
            "/user/repository_invitations?per_page=100",
            "/user/repository_invitations/1",
            "/user/repository_invitations/3",
        ]
    );
    assert!(engine.cfg.repos.is_empty(), "acceptance enrolled a factory");
}

#[tokio::test]
async fn one_failed_acceptance_does_not_block_later_invitations() {
    let stub = GitHubStub::start().await;
    stub.set_invitations(vec![
        invitation(1, "alice/one", "alice"),
        invitation(2, "alice/two", "alice"),
    ]);
    stub.reject_invitation(1);
    let mut engine = engine_at(&stub.base);
    engine.cfg.github.auto_accept_invitations_from = vec!["alice".into()];

    let error = engine.accept_repository_invitations().await.unwrap_err();

    assert!(format!("{error:#}").contains("alice/one"), "{error:#}");
    assert_eq!(stub.accepted_invitations(), vec![2]);
    assert_eq!(
        stub.hits(),
        vec![
            "/user/repository_invitations?per_page=100",
            "/user/repository_invitations/1",
            "/user/repository_invitations/2",
        ]
    );
}

#[tokio::test]
async fn an_empty_inviter_list_makes_no_github_request() {
    let stub = GitHubStub::start().await;
    let engine = engine_at(&stub.base);

    engine.accept_repository_invitations().await.unwrap();

    assert!(stub.hits().is_empty());
}
