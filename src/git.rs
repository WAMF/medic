//! Thin wrapper around the `git` binary. Every repository read goes through
//! here; medic never links a git library and never calls a hosting API.

use anyhow::{anyhow, bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A repository that medic shells out to.
#[derive(Debug, Clone)]
pub struct Git {
    dir: PathBuf,
}

/// One changed path in a three-dot diff.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct FileChange {
    /// Path on the head side.
    pub path: String,
    /// Path on the base side when the file was renamed or copied.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_path: Option<String>,
    /// Single-letter git status: A, M, D, R, C, T.
    pub status: char,
    /// Lines added; `None` for binary files.
    pub added: Option<u64>,
    /// Lines deleted; `None` for binary files.
    pub deleted: Option<u64>,
}

impl FileChange {
    pub fn is_binary(&self) -> bool {
        self.added.is_none()
    }

    pub fn lines(&self) -> u64 {
        self.added.unwrap_or(0) + self.deleted.unwrap_or(0)
    }

    /// Every path this change touches. A rename touches both sides.
    pub fn paths(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.path.as_str()).chain(self.old_path.as_deref())
    }
}

/// Result of an in-memory merge done by `git merge-tree --write-tree`.
#[derive(Debug, Clone)]
pub struct MergeResult {
    pub tree: String,
    pub conflicts: Vec<String>,
    pub clean: bool,
}

impl Git {
    pub fn open(dir: &Path) -> Result<Self> {
        let git = Git {
            dir: dir.to_path_buf(),
        };
        git.run(&["rev-parse", "--git-dir"])
            .with_context(|| format!("{} is not inside a git repository", dir.display()))?;
        Ok(git)
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new("git");
        cmd.arg("-C")
            .arg(&self.dir)
            .args(["-c", "core.quotepath=off", "-c", "color.ui=never"])
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("LC_ALL", "C");
        cmd
    }

    fn output(&self, args: &[&str]) -> Result<Output> {
        self.command(args)
            .output()
            .with_context(|| format!("could not run git {}", args.join(" ")))
    }

    /// Run git and return stdout. A non-zero exit is an error.
    pub fn run(&self, args: &[&str]) -> Result<String> {
        let out = self.output(args)?;
        if !out.status.success() {
            bail!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    pub fn version(&self) -> Result<(u32, u32)> {
        let v = self.run(&["--version"])?;
        let nums: Vec<u32> = v
            .split_whitespace()
            .nth(2)
            .unwrap_or("")
            .split('.')
            .take(2)
            .filter_map(|p| p.parse().ok())
            .collect();
        match nums.as_slice() {
            [major, minor] => Ok((*major, *minor)),
            _ => Err(anyhow!("cannot parse git version from {v:?}")),
        }
    }

    /// `git merge-tree --write-tree` needs git 2.38 or later.
    pub fn require_merge_tree(&self) -> Result<()> {
        let (major, minor) = self.version()?;
        if (major, minor) < (2, 38) {
            bail!(
                "git {major}.{minor} is too old: medic needs git 2.38+ for merge-tree --write-tree"
            );
        }
        Ok(())
    }

    pub fn top_level(&self) -> Option<PathBuf> {
        self.run(&["rev-parse", "--show-toplevel"])
            .ok()
            .map(|s| PathBuf::from(s.trim()))
    }

    /// Resolve a ref to a commit id.
    pub fn resolve(&self, rev: &str) -> Result<String> {
        let spec = format!("{rev}^{{commit}}");
        self.run(&[
            "rev-parse",
            "--verify",
            "--quiet",
            "--end-of-options",
            &spec,
        ])
        .map(|s| s.trim().to_string())
        .map_err(|_| anyhow!("cannot resolve {rev:?} to a commit (fetch it first?)"))
    }

    pub fn merge_base(&self, a: &str, b: &str) -> Result<String> {
        self.run(&["merge-base", a, b])
            .map(|s| s.trim().to_string())
            .map_err(|_| {
                anyhow!("{a} and {b} have no merge base (shallow clone? try git fetch --unshallow)")
            })
    }

    /// Fetch `refspec` from `remote`. This is the only network call medic makes.
    pub fn fetch(&self, remote: &str, refspecs: &[String]) -> Result<()> {
        let mut args = vec!["fetch", "--quiet", "--no-tags", "--", remote];
        args.extend(refspecs.iter().map(String::as_str));
        self.run(&args).map(|_| ())
    }

    /// Files changed by `base...head`, like a GitHub pull request diff.
    pub fn changed_files(&self, base: &str, head: &str) -> Result<Vec<FileChange>> {
        let range = format!("{base}...{head}");
        let common = ["--no-ext-diff", "--no-textconv", "-M", "-z"];
        let mut status_args = vec!["diff", "--name-status"];
        status_args.extend(common);
        status_args.push(&range);
        let mut numstat_args = vec!["diff", "--numstat"];
        numstat_args.extend(common);
        numstat_args.push(&range);
        let statuses = parse_name_status(&self.run(&status_args)?)?;
        let stats: std::collections::HashMap<String, (Option<u64>, Option<u64>)> =
            parse_numstat(&self.run(&numstat_args)?)?
                .into_iter()
                .map(|(a, d, p)| (p, (a, d)))
                .collect();
        let mut files = Vec::with_capacity(statuses.len());
        for (status, old_path, path) in statuses {
            let (added, deleted) = stats.get(&path).copied().unwrap_or((Some(0), Some(0)));
            files.push(FileChange {
                path,
                old_path,
                status,
                added,
                deleted,
            });
        }
        Ok(files)
    }

    /// Merge two commits in memory. Neither the index nor the work tree is
    /// touched; git only writes new objects to the object store.
    pub fn merge_tree(&self, ours: &str, theirs: &str) -> Result<MergeResult> {
        let args = [
            "merge-tree",
            "--write-tree",
            "--name-only",
            "--no-messages",
            "-z",
            ours,
            theirs,
        ];
        let out = self.output(&args)?;
        let clean = match out.status.code() {
            Some(0) => true,
            Some(1) => false,
            _ => bail!(
                "git merge-tree {ours} {theirs} failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        };
        let stdout = String::from_utf8_lossy(&out.stdout);
        let mut parts = stdout.split('\0').filter(|s| !s.is_empty());
        let tree = parts
            .next()
            .ok_or_else(|| anyhow!("git merge-tree printed no tree id"))?
            .trim()
            .to_string();
        let mut conflicts: Vec<String> = parts.map(str::to_string).collect();
        conflicts.dedup();
        Ok(MergeResult {
            tree,
            conflicts,
            clean,
        })
    }

    /// Wrap a tree in a commit object so it can take part in a later merge.
    /// The commit is dangling: no ref points to it and `git gc` removes it.
    pub fn commit_tree(&self, tree: &str, parents: &[&str]) -> Result<String> {
        let mut args = vec!["commit-tree", tree];
        for p in parents {
            args.push("-p");
            args.push(p);
        }
        args.extend(["-m", "medic: simulated merge"]);
        let mut cmd = self.command(&args);
        cmd.env("GIT_AUTHOR_NAME", "medic")
            .env("GIT_AUTHOR_EMAIL", "medic@localhost")
            .env("GIT_COMMITTER_NAME", "medic")
            .env("GIT_COMMITTER_EMAIL", "medic@localhost");
        let out = cmd.output().context("could not run git commit-tree")?;
        if !out.status.success() {
            bail!(
                "git commit-tree failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    /// Author e-mails of the commits in `base..head`, most frequent first.
    pub fn authors(&self, base: &str, head: &str) -> Result<Vec<String>> {
        let range = format!("{base}..{head}");
        let out = self.run(&["log", "--no-merges", "--format=%ae", &range])?;
        let mut counts: Vec<(String, usize)> = Vec::new();
        for email in out.lines().filter(|l| !l.is_empty()) {
            match counts.iter_mut().find(|(e, _)| e == email) {
                Some((_, n)) => *n += 1,
                None => counts.push((email.to_string(), 1)),
            }
        }
        counts.sort_by_key(|c| std::cmp::Reverse(c.1));
        Ok(counts.into_iter().map(|(e, _)| e).collect())
    }

    /// Commits by `email` reachable from `rev`, capped at `limit`.
    pub fn author_commit_count(&self, rev: &str, email: &str, limit: u64) -> Result<u64> {
        let author = format!("--author=<{email}>");
        let max = format!("--max-count={limit}");
        let out = self.run(&[
            "log",
            "--no-merges",
            "--fixed-strings",
            &author,
            &max,
            "--format=%h",
            rev,
        ])?;
        Ok(out.lines().count() as u64)
    }

    /// Recent history of `rev`: one entry per commit with its subject and the
    /// paths it touched.
    pub fn history(&self, rev: &str, days: u64, limit: u64) -> Result<Vec<(String, Vec<String>)>> {
        let since = format!("--since={days}.days.ago");
        let max = format!("--max-count={limit}");
        let out = self.run(&[
            "log",
            "--no-merges",
            "--no-renames",
            &since,
            &max,
            "--format=%x1e%s",
            "--name-only",
            rev,
        ])?;
        Ok(out
            .split('\x1e')
            .filter(|r| !r.trim().is_empty())
            .map(|record| {
                let mut lines = record.lines();
                let subject = lines.next().unwrap_or("").to_string();
                let files = lines
                    .filter(|l| !l.is_empty())
                    .map(str::to_string)
                    .collect();
                (subject, files)
            })
            .collect())
    }
}

/// Parse `git diff --name-status -z` into (status, old path, new path).
fn parse_name_status(raw: &str) -> Result<Vec<(char, Option<String>, String)>> {
    let mut out = Vec::new();
    let mut it = raw.split('\0').filter(|s| !s.is_empty());
    while let Some(code) = it.next() {
        let status = code
            .chars()
            .next()
            .ok_or_else(|| anyhow!("empty status in git diff output"))?;
        let first = it
            .next()
            .ok_or_else(|| anyhow!("missing path after status {code}"))?
            .to_string();
        if status == 'R' || status == 'C' {
            let second = it
                .next()
                .ok_or_else(|| anyhow!("missing new path after {code}"))?
                .to_string();
            out.push((status, Some(first), second));
        } else {
            out.push((status, None, first));
        }
    }
    Ok(out)
}

type NumStat = (Option<u64>, Option<u64>, String);

/// Parse `git diff --numstat -z`. Renames print an empty path field followed
/// by the old and new paths as separate NUL-terminated fields.
fn parse_numstat(raw: &str) -> Result<Vec<NumStat>> {
    let mut out = Vec::new();
    let mut it = raw.split('\0');
    while let Some(record) = it.next() {
        if record.is_empty() {
            continue;
        }
        let mut cols = record.splitn(3, '\t');
        let added = cols.next().unwrap_or("-").parse().ok();
        let deleted = cols.next().unwrap_or("-").parse().ok();
        let path = cols.next().unwrap_or("");
        let path = if path.is_empty() {
            let _old = it.next();
            it.next()
                .ok_or_else(|| anyhow!("missing rename target in numstat"))?
                .to_string()
        } else {
            path.to_string()
        };
        out.push((added, deleted, path));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_status_handles_renames() {
        let raw = "M\0a.rs\0R087\0old.rs\0new.rs\0D\0gone.md\0";
        let parsed = parse_name_status(raw).unwrap();
        assert_eq!(parsed[0], ('M', None, "a.rs".into()));
        assert_eq!(parsed[1], ('R', Some("old.rs".into()), "new.rs".into()));
        assert_eq!(parsed[2], ('D', None, "gone.md".into()));
    }

    #[test]
    fn numstat_handles_renames_and_binary() {
        let raw = "3\t1\ta.rs\0-\t-\timg.png\x002\t0\t\0old.rs\0new.rs\0";
        let parsed = parse_numstat(raw).unwrap();
        assert_eq!(parsed[0], (Some(3), Some(1), "a.rs".into()));
        assert_eq!(parsed[1], (None, None, "img.png".into()));
        assert_eq!(parsed[2], (Some(2), Some(0), "new.rs".into()));
    }
}
