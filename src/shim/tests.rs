use super::*;

fn o() -> Origin {
    Origin::new("acme/widgets", 12).unwrap()
}

/// The first line of a post on the session's own repository.
fn line() -> String {
    o().first_line(Some("acme/widgets"), false)
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
