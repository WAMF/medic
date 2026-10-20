//! Turn command-line ref arguments into resolved commits, fetching them
//! first when `--fetch <remote>` is given.

use crate::git::Git;
use anyhow::Result;
use serde::Serialize;

/// A ref as the user named it, and the commit it resolved to.
#[derive(Debug, Clone, Serialize)]
pub struct Target {
    pub label: String,
    #[serde(rename = "ref")]
    pub rev: String,
    pub commit: String,
}

/// A pull request number: `123` or `#123`.
fn pr_number(arg: &str) -> Option<&str> {
    let n = arg.strip_prefix('#').unwrap_or(arg);
    (!n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())).then_some(n)
}

/// Where a fetched ref lands locally, and the refspec that puts it there.
fn plan_fetch(arg: &str, remote: &str) -> (String, String) {
    if let Some(n) = pr_number(arg) {
        let local = format!("refs/medic/pull/{n}");
        return (format!("+refs/pull/{n}/head:{local}"), local);
    }
    if let Some(rest) = arg.strip_prefix("refs/") {
        let local = format!("refs/medic/{rest}");
        return (format!("+{arg}:{local}"), local);
    }
    // A URL is not a remote name, so it gets no refs/remotes/ namespace.
    let local = if is_remote_name(remote) {
        format!("refs/remotes/{remote}/{arg}")
    } else {
        format!("refs/medic/heads/{arg}")
    };
    (format!("+refs/heads/{arg}:{local}"), local)
}

fn is_remote_name(remote: &str) -> bool {
    !remote.is_empty()
        && remote
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// Without a fetch, a bare PR number means a ref fetched by an earlier run.
fn local_rev(git: &Git, arg: &str) -> String {
    if let Some(n) = pr_number(arg) {
        let fetched = format!("refs/medic/pull/{n}");
        if git.resolve(&fetched).is_ok() {
            return fetched;
        }
    }
    arg.to_string()
}

fn label(arg: &str) -> String {
    match pr_number(arg) {
        Some(n) => format!("#{n}"),
        None => arg.to_string(),
    }
}

/// Resolve the base and the heads. With `fetch`, all of them are fetched
/// from that remote in one `git fetch` first.
pub fn resolve(
    git: &Git,
    base: &str,
    heads: &[String],
    fetch: Option<&str>,
) -> Result<(Target, Vec<Target>)> {
    let mut args: Vec<&str> = vec![base];
    args.extend(heads.iter().map(String::as_str));
    let revs: Vec<String> = match fetch {
        Some(remote) => {
            let plans: Vec<(String, String)> = args.iter().map(|a| plan_fetch(a, remote)).collect();
            let mut specs: Vec<String> = plans.iter().map(|p| p.0.clone()).collect();
            specs.sort();
            specs.dedup();
            git.fetch(remote, &specs)?;
            plans.into_iter().map(|p| p.1).collect()
        }
        None => args.iter().map(|a| local_rev(git, a)).collect(),
    };
    let mut targets = Vec::with_capacity(args.len());
    for (arg, rev) in args.iter().zip(revs) {
        targets.push(Target {
            label: label(arg),
            commit: git.resolve(&rev)?,
            rev,
        });
    }
    let base = targets.remove(0);
    Ok((base, targets))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fetch_plans() {
        assert_eq!(
            plan_fetch("#12", "origin"),
            (
                "+refs/pull/12/head:refs/medic/pull/12".into(),
                "refs/medic/pull/12".into()
            )
        );
        assert_eq!(
            plan_fetch("main", "origin"),
            (
                "+refs/heads/main:refs/remotes/origin/main".into(),
                "refs/remotes/origin/main".into()
            )
        );
        assert_eq!(
            plan_fetch("main", "https://github.com/o/r.git"),
            (
                "+refs/heads/main:refs/medic/heads/main".into(),
                "refs/medic/heads/main".into()
            )
        );
        assert_eq!(
            plan_fetch("refs/pull/7/merge", "up"),
            (
                "+refs/pull/7/merge:refs/medic/pull/7/merge".into(),
                "refs/medic/pull/7/merge".into()
            )
        );
    }

    #[test]
    fn labels() {
        assert_eq!(label("42"), "#42");
        assert_eq!(label("#42"), "#42");
        assert_eq!(label("feature/x"), "feature/x");
        assert_eq!(pr_number("v1"), None);
    }
}
