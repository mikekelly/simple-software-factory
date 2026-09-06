//! Who may drive the factory: the allow-list of GitHub logins whose
//! assignments, mentions, review requests, labels and posts ssf acts on.
//!
//! Everything that reaches the bot on GitHub comes from whoever can write
//! on the repository, and a comment is relayed into a running agent's
//! terminal, so the set of logins that count is an explicit setting:
//! `daemon.allowed_users` for the whole instance, `allowed_users` on a
//! `[[repo]]` replacing it for that repository, and, with neither set,
//! the repository's collaborators with push access. The bot's own login is
//! always accepted (its sessions post as it, and whoever types as it holds
//! its token). `"*"` means anyone, and is only accepted with an explicit
//! `accepted_anyone_risk = true` next to it.

use std::collections::BTreeSet;

use serde_json::Value;

use crate::github::{Issue, value_str};
use crate::prompt::actor_of;

/// The list entry that opens the factory to everyone.
pub const ANYONE: &str = "*";

/// The config key that has to accompany a wildcard list.
pub const RISK_KEY: &str = "accepted_anyone_risk";

/// Whether a configured list contains the wildcard.
pub fn is_wildcard(list: &[String]) -> bool {
    list.iter().any(|l| l.trim() == ANYONE)
}

/// GitHub app and integration accounts (`github-actions[bot]`,
/// `github-project-automation[bot]`, ...). Listed explicitly like any other
/// login, never part of the collaborator default, and dropped quietly since
/// project automation fires on every card move.
pub fn is_bot_account(login: &str) -> bool {
    login.ends_with("[bot]")
}

/// Where the effective list came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// `allowed_users` on the `[[repo]]`.
    Repo,
    /// `daemon.allowed_users`.
    Instance,
    /// Neither set: the repository's collaborators with push access.
    Collaborators,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::Repo => "repo list",
            Source::Instance => "instance list",
            Source::Collaborators => "collaborators with push access",
        }
    }
}

/// The logins that may drive one repository.
#[derive(Debug, Clone)]
pub struct AllowList {
    bot: String,
    anyone: bool,
    /// Lower-cased.
    logins: BTreeSet<String>,
    pub source: Source,
}

impl AllowList {
    pub fn new<'a>(bot: &str, logins: impl IntoIterator<Item = &'a str>, source: Source) -> Self {
        let mut anyone = false;
        let mut set = BTreeSet::new();
        for l in logins {
            let l = l.trim();
            if l == ANYONE {
                anyone = true;
            } else if !l.is_empty() {
                set.insert(l.to_ascii_lowercase());
            }
        }
        Self {
            bot: bot.to_string(),
            anyone,
            logins: set,
            source,
        }
    }

    /// Whether a login may drive the factory here.
    pub fn allows(&self, login: &str) -> bool {
        login.eq_ignore_ascii_case(&self.bot)
            || self.anyone
            || self.logins.contains(&login.to_ascii_lowercase())
    }

    /// Whether the wildcard is in effect.
    pub fn is_anyone(&self) -> bool {
        self.anyone
    }

    /// The listed logins, sorted.
    #[cfg(test)]
    pub fn logins(&self) -> Vec<String> {
        self.logins.iter().cloned().collect()
    }

    /// One line for `doctor`, `status` and the log.
    pub fn describe(&self) -> String {
        if self.anyone {
            return format!("ANYONE on GitHub (wildcard in the {})", self.source.label());
        }
        if self.logins.is_empty() {
            return format!("nobody but the bot ({}: empty)", self.source.label());
        }
        let names: Vec<String> = self.logins.iter().map(|l| format!("@{l}")).collect();
        format!("{} ({})", names.join(", "), self.source.label())
    }
}

/// Whether `body` mentions `@login` (case-insensitive; a longer login that
/// starts the same way is not a mention).
pub fn mentions(body: &str, login: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    let needle = format!("@{}", login.to_ascii_lowercase());
    let mut from = 0;
    while let Some(i) = lower[from..].find(&needle) {
        let end = from + i + needle.len();
        let boundary = lower[end..]
            .chars()
            .next()
            .is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '-'));
        if boundary {
            return true;
        }
        from = end;
    }
    false
}

/// One way the bot was asked onto an item, and by whom.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ask {
    pub trigger: String,
    pub login: String,
}

/// Who asked the bot onto an item, per trigger, read from its timeline:
///
/// - `assigned`: the actor of the latest assignment of the bot (the author
///   when there is no such event);
/// - `mentioned`: everyone whose body, comment or review mentions the bot;
/// - `review_requested`: the actor of the latest review request naming the
///   bot (the author when there is none);
/// - `review_label`: the actor of the latest addition of the review label;
/// - `created`: the bot itself.
pub fn askers(
    issue: &Issue,
    timeline: &[Value],
    triggers: &[String],
    bot: &str,
    review_label: Option<&str>,
) -> Vec<Ask> {
    let mut out = Vec::new();
    let mut push = |trigger: &str, login: &str| {
        let ask = Ask {
            trigger: trigger.to_string(),
            login: login.to_string(),
        };
        if !out.contains(&ask) {
            out.push(ask);
        }
    };
    for trigger in triggers {
        match trigger.as_str() {
            "assigned" => {
                let actor = timeline
                    .iter()
                    .rev()
                    .find(|ev| {
                        value_str(ev, &["event"]) == Some("assigned")
                            && value_str(ev, &["assignee", "login"])
                                .is_some_and(|l| l.eq_ignore_ascii_case(bot))
                    })
                    .map(actor_of)
                    .unwrap_or_else(|| issue.author().to_string());
                push("assigned", &actor);
            }
            "mentioned" => {
                if issue.body.as_deref().is_some_and(|b| mentions(b, bot)) {
                    push("mentioned", issue.author());
                }
                for ev in timeline {
                    match value_str(ev, &["event"]) {
                        Some("commented" | "reviewed") => {
                            if value_str(ev, &["body"]).is_some_and(|b| mentions(b, bot)) {
                                push("mentioned", &actor_of(ev));
                            }
                        }
                        Some("line-commented" | "commit-commented") => {
                            for c in ev
                                .get("comments")
                                .and_then(Value::as_array)
                                .into_iter()
                                .flatten()
                            {
                                if value_str(c, &["body"]).is_some_and(|b| mentions(b, bot)) {
                                    push(
                                        "mentioned",
                                        value_str(c, &["user", "login"]).unwrap_or("unknown"),
                                    );
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            "review_requested" => {
                let actor = timeline
                    .iter()
                    .rev()
                    .find(|ev| {
                        value_str(ev, &["event"]) == Some("review_requested")
                            && value_str(ev, &["requested_reviewer", "login"])
                                .is_some_and(|l| l.eq_ignore_ascii_case(bot))
                    })
                    .map(actor_of)
                    .unwrap_or_else(|| issue.author().to_string());
                push("review_requested", &actor);
            }
            "review_label" => {
                if let Some(label) = review_label
                    && let Some(ev) = timeline.iter().rev().find(|ev| {
                        value_str(ev, &["event"]) == Some("labeled")
                            && value_str(ev, &["label", "name"])
                                .is_some_and(|n| n.eq_ignore_ascii_case(label))
                    })
                {
                    push("review_label", &actor_of(ev));
                }
            }
            "created" => push("created", bot),
            _ => {}
        }
    }
    out
}

/// The outcome of checking who asked against the list: `Ok` when at least
/// one ask came from an allowed login, else why not, for the log.
pub fn check(list: &AllowList, asks: &[Ask]) -> Result<(), String> {
    if asks.iter().any(|a| list.allows(&a.login)) {
        return Ok(());
    }
    if asks.is_empty() {
        return Err("nobody ssf can see asked for it".to_string());
    }
    let parts: Vec<String> = asks
        .iter()
        .map(|a| format!("{} by @{}", a.trigger, a.login))
        .collect();
    Err(format!("{}: not an allowed user", parts.join(", ")))
}

/// The logins with push access among the entries of
/// `GET /repos/{owner}/{repo}/collaborators`.
pub fn pushers(collaborators: &[Value]) -> Vec<String> {
    collaborators
        .iter()
        .filter(|c| {
            c.pointer("/permissions/push")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        })
        .filter_map(|c| value_str(c, &["login"]).map(str::to_string))
        .filter(|l| !is_bot_account(l))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn list(logins: &[&str]) -> AllowList {
        AllowList::new("bot", logins.iter().copied(), Source::Instance)
    }

    #[test]
    fn logins_compare_case_insensitively_and_the_bot_always_passes() {
        let l = list(&["MikeKelly", " alice "]);
        assert!(l.allows("mikekelly"));
        assert!(l.allows("MIKEKELLY"));
        assert!(l.allows("Alice"));
        assert!(!l.allows("bob"));
        assert!(l.allows("bot"));
        assert!(l.allows("BOT"));
        assert!(!l.is_anyone());
        assert_eq!(l.logins(), vec!["alice", "mikekelly"]);
        assert_eq!(l.describe(), "@alice, @mikekelly (instance list)");
    }

    #[test]
    fn an_empty_list_is_nobody_but_the_bot() {
        let l = list(&[]);
        assert!(!l.allows("alice"));
        assert!(l.allows("bot"));
        assert_eq!(l.describe(), "nobody but the bot (instance list: empty)");
    }

    #[test]
    fn the_wildcard_is_everyone_and_bots_are_ordinary_logins() {
        let l = list(&["*"]);
        assert!(l.is_anyone());
        assert!(l.allows("anyone-at-all"));
        assert!(l.allows("github-actions[bot]"));
        assert!(l.describe().starts_with("ANYONE on GitHub"));
        assert!(is_wildcard(&["alice".into(), " * ".into()]));
        assert!(!is_wildcard(&["alice".into()]));
        let l = list(&["alice"]);
        assert!(!l.allows("github-project-automation[bot]"));
        assert!(is_bot_account("github-project-automation[bot]"));
        assert!(!is_bot_account("alice"));
        assert!(list(&["github-actions[bot]"]).allows("GitHub-Actions[bot]"));
    }

    #[test]
    fn mentions_need_the_whole_login() {
        assert!(mentions("hey @Bot please", "bot"));
        assert!(mentions("@bot", "bot"));
        assert!(mentions("cc @bot, thanks", "bot"));
        assert!(!mentions("@bottle", "bot"));
        assert!(!mentions("@bot-2 look", "bot"));
        assert!(mentions("@bot-2 and @bot.", "bot"));
        assert!(!mentions("bot@example.com", "bot"));
    }

    fn issue(author: &str, body: &str) -> Issue {
        serde_json::from_value(json!({
            "number": 1, "title": "t", "body": body, "html_url": "u",
            "state": "open", "user": {"login": author}, "created_at": "x", "updated_at": "x"
        }))
        .unwrap()
    }

    fn ev(kind: &str, actor: &str, extra: Value) -> Value {
        let mut v = json!({"event": kind, "actor": {"login": actor}, "created_at": "t"});
        if let Some(obj) = extra.as_object() {
            for (k, x) in obj {
                v[k] = x.clone();
            }
        }
        v
    }

    fn comment(who: &str, body: &str) -> Value {
        json!({"event":"commented","id":1,"user":{"login":who},"body":body,"created_at":"t"})
    }

    #[test]
    fn askers_are_read_from_the_timeline_per_trigger() {
        let i = issue("alice", "please @bot look");
        let timeline = vec![
            ev("assigned", "mallory", json!({"assignee": {"login": "bot"}})),
            ev("assigned", "carol", json!({"assignee": {"login": "bot"}})),
            ev(
                "assigned",
                "mallory",
                json!({"assignee": {"login": "someone-else"}}),
            ),
            comment("dave", "@bot and @bob"),
            comment("erin", "no mention"),
            json!({"event":"line-commented","comments":[{"user":{"login":"frank"},"body":"@bot here"}]}),
            ev(
                "review_requested",
                "grace",
                json!({"requested_reviewer": {"login": "bot"}}),
            ),
            ev("labeled", "heidi", json!({"label": {"name": "Review"}})),
            ev("labeled", "ivan", json!({"label": {"name": "bug"}})),
        ];
        let all: Vec<String> = [
            "assigned",
            "mentioned",
            "review_requested",
            "review_label",
            "created",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let asks = askers(&i, &timeline, &all, "bot", Some("review"));
        let pairs: Vec<(&str, &str)> = asks
            .iter()
            .map(|a| (a.trigger.as_str(), a.login.as_str()))
            .collect();
        assert_eq!(
            pairs,
            vec![
                ("assigned", "carol"),
                ("mentioned", "alice"),
                ("mentioned", "dave"),
                ("mentioned", "frank"),
                ("review_requested", "grace"),
                ("review_label", "heidi"),
                ("created", "bot"),
            ]
        );
        // No assignment or review request event: the author asked.
        let asks = askers(
            &issue("alice", "no mention"),
            &[],
            &[
                "assigned".into(),
                "review_requested".into(),
                "mentioned".into(),
            ],
            "bot",
            None,
        );
        let pairs: Vec<(&str, &str)> = asks
            .iter()
            .map(|a| (a.trigger.as_str(), a.login.as_str()))
            .collect();
        assert_eq!(
            pairs,
            vec![("assigned", "alice"), ("review_requested", "alice")]
        );
    }

    #[test]
    fn one_allowed_asker_is_enough_and_refusals_name_the_logins() {
        let asks = vec![
            Ask {
                trigger: "assigned".into(),
                login: "mallory".into(),
            },
            Ask {
                trigger: "mentioned".into(),
                login: "alice".into(),
            },
        ];
        assert!(check(&list(&["alice"]), &asks).is_ok());
        assert_eq!(
            check(&list(&["bob"]), &asks).unwrap_err(),
            "assigned by @mallory, mentioned by @alice: not an allowed user"
        );
        assert_eq!(
            check(&list(&["bob"]), &[]).unwrap_err(),
            "nobody ssf can see asked for it"
        );
        assert!(check(&list(&["*"]), &asks).is_ok());
        assert!(
            check(
                &list(&[]),
                &[Ask {
                    trigger: "created".into(),
                    login: "bot".into()
                }]
            )
            .is_ok()
        );
    }

    #[test]
    fn collaborators_with_push_access_make_the_default_list() {
        let raw = vec![
            json!({"login": "owner", "permissions": {"pull": true, "push": true, "admin": true}}),
            json!({"login": "reader", "permissions": {"pull": true, "push": false, "admin": false}}),
            json!({"login": "dev", "permissions": {"pull": true, "triage": true, "push": true, "maintain": false, "admin": false}}),
            json!({"login": "some-app[bot]", "permissions": {"push": true}}),
            json!({"login": "odd"}),
        ];
        assert_eq!(pushers(&raw), vec!["owner", "dev"]);
    }
}
