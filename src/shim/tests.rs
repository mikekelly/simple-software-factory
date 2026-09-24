use super::*;

fn o() -> Origin {
    Origin::new("acme/widgets", 12).unwrap()
}

/// The first line of a post on the session's own repository.
fn line() -> String {
    o().first_line(Some("acme/widgets"), false, None)
}

fn args(s: &[&str]) -> Vec<String> {
    s.iter().map(|x| x.to_string()).collect()
}

fn no_files(_: &str) -> std::io::Result<String> {
    Err(std::io::Error::other("no files in tests"))
}

fn same_repo() -> Option<String> {
    Some("acme/widgets".into())
}

fn unknown() -> Option<String> {
    None
}

/// A shim in a checkout of the session's own repository, no `GH_REPO`.
fn shim(origin: &Origin) -> Shim<'_> {
    Shim {
        origin,
        bot: None,
        gh_repo: None,
        stack: None,
        read: &no_files,
        checkout: &same_repo,
    }
}

fn rewrite(a: Vec<String>) -> Vec<String> {
    let o = o();
    shim(&o).rewrite(a)
}

mod attribution;
mod bodies_and_reviews;
mod delegation_and_install;
mod flags;

#[test]
fn only_a_possible_post_reads_the_context() {
    assert!(may_post(&args(&["-R", "o/r", "issue", "comment", "1"])));
    assert!(may_post(&args(&["pr", "review", "--approve"])));
    assert!(!may_post(&args(&["pr", "view", "1"])));
    assert!(!may_post(&args(&["api", "repos/o/r/issues"])));
    assert!(!may_post(&args(&["comment", "issue"])));
}
