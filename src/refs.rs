//! Turn command-line ref arguments into resolved commits, fetching them
//! first when `--fetch <remote>` is given.

use crate::git::Git;
use anyhow::{Context, Result};
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

/// A commit id (7 to 40 hex digits, not all digits) or `HEAD`: something
/// that is already local and has no branch to fetch.
fn is_local_only(arg: &str) -> bool {
    arg == "HEAD"
        || ((7..=40).contains(&arg.len())
            && arg.bytes().all(|b| b.is_ascii_hexdigit())
            && !arg.bytes().all(|b| b.is_ascii_digit()))
}

/// The refspec to fetch for `arg`, and the local ref it lands in. `None`
/// means nothing to fetch: the argument is used as it is.
fn plan_fetch(arg: &str, remote: &str) -> Option<(String, String)> {
    if let Some(n) = pr_number(arg) {
        let local = format!("refs/medic/pull/{n}");
        return Some((format!("+refs/pull/{n}/head:{local}"), local));
    }
    if let Some(rest) = arg.strip_prefix("refs/") {
        let local = format!("refs/medic/{rest}");
        return Some((format!("+{arg}:{local}"), local));
    }
    if is_local_only(arg) {
        return None;
    }
    // `origin/main` with `--fetch origin` names the branch `main`.
    let branch = arg
        .strip_prefix(remote)
        .and_then(|rest| rest.strip_prefix('/'))
        .unwrap_or(arg);
    // A URL is not a remote name, so it gets no refs/remotes/ namespace.
    let local = if is_remote_name(remote) {
        format!("refs/remotes/{remote}/{branch}")
    } else {
        format!("refs/medic/heads/{branch}")
    };
    Some((format!("+refs/heads/{branch}:{local}"), local))
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
            let plans: Vec<Option<(String, String)>> =
                args.iter().map(|a| plan_fetch(a, remote)).collect();
            let mut specs: Vec<String> = plans.iter().flatten().map(|p| p.0.clone()).collect();
            specs.sort();
            specs.dedup();
            if !specs.is_empty() {
                git.fetch(remote, &specs).context(
                    "fetch failed; with --fetch, name branches, PR numbers, refs/... or commit ids",
                )?;
            }
            plans
                .into_iter()
                .zip(&args)
                .map(|(plan, arg)| plan.map_or_else(|| arg.to_string(), |p| p.1))
                .collect()
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
        let plan = |arg: &str, remote: &str| plan_fetch(arg, remote).map(|p| (p.0, p.1));
        let some = |spec: &str, local: &str| Some((spec.to_string(), local.to_string()));
        assert_eq!(
            plan("#12", "origin"),
            some(
                "+refs/pull/12/head:refs/medic/pull/12",
                "refs/medic/pull/12"
            )
        );
        assert_eq!(
            plan("main", "origin"),
            some(
                "+refs/heads/main:refs/remotes/origin/main",
                "refs/remotes/origin/main"
            )
        );
        assert_eq!(
            plan("origin/main", "origin"),
            some(
                "+refs/heads/main:refs/remotes/origin/main",
                "refs/remotes/origin/main"
            )
        );
        assert_eq!(
            plan("main", "https://github.com/o/r.git"),
            some(
                "+refs/heads/main:refs/medic/heads/main",
                "refs/medic/heads/main"
            )
        );
        assert_eq!(
            plan("refs/pull/7/merge", "up"),
            some(
                "+refs/pull/7/merge:refs/medic/pull/7/merge",
                "refs/medic/pull/7/merge"
            )
        );
        assert_eq!(plan("HEAD", "origin"), None);
        assert_eq!(plan("a64875e7", "origin"), None);
        // All digits is a PR number, never a commit id.
        assert!(plan("1234567", "origin").is_some());
    }

    #[test]
    fn labels() {
        assert_eq!(label("42"), "#42");
        assert_eq!(label("#42"), "#42");
        assert_eq!(label("feature/x"), "feature/x");
        assert_eq!(pr_number("v1"), None);
    }
}
