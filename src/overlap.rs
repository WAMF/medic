//! File overlap, real merge conflicts and a suggested non-conflicting merge
//! set for a group of changes against one base.

use crate::classify::Classifier;
use crate::config::Config;
use crate::git::{FileChange, Git};
use crate::refs::Target;
use crate::risk::{self, Assessment, History};
use anyhow::Result;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Treat a shared file like a conflict when choosing the merge set.
    pub avoid_overlap: bool,
    /// Only run merge checks for pairs that share a path. Faster on large
    /// groups; misses rare conflicts such as directory renames.
    pub overlapping_only: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct PrEntry {
    pub label: String,
    #[serde(rename = "ref")]
    pub rev: String,
    pub commit: String,
    pub merge_base: String,
    pub lines: u64,
    pub files: Vec<FileChange>,
    /// The change does not merge cleanly into the base on its own.
    pub base_conflict: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub base_conflict_files: Vec<String>,
    pub risk: Assessment,
}

#[derive(Debug, Clone, Serialize)]
pub struct Pair {
    pub a: String,
    pub b: String,
    pub shared_files: Vec<String>,
    /// `None` when the pair was not checked: one side conflicts with the
    /// base, or `--overlapping-only` skipped a pair with no shared file.
    pub conflict: Option<bool>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub conflict_files: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SharedFile {
    pub path: String,
    pub prs: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Excluded {
    pub label: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Suggestion {
    pub rule: String,
    /// Changes that merge together without conflict, in the order checked.
    pub merge_order: Vec<String>,
    pub excluded: Vec<Excluded>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub base: Target,
    pub prs: Vec<PrEntry>,
    /// Pairs that share a file or conflict.
    pub pairs: Vec<Pair>,
    /// Files changed by two or more changes.
    pub shared_files: Vec<SharedFile>,
    pub suggestion: Suggestion,
}

pub const RULE: &str = "Greedy maximal set. Drop changes that conflict with the base. \
Order the rest by fewest pairwise conflicts, then lowest risk score, then input order. \
Take each change unless it conflicts with one already taken; then merge it in memory on top \
of the taken set and keep it only if that merge is clean. No remaining change can be added \
without a conflict, but a larger set may exist (maximum independent set is NP-hard).";

/// One change merged onto the base in memory, if that merge was clean.
struct Prepared {
    target: Target,
    merge_base: String,
    files: Vec<FileChange>,
    paths: BTreeSet<String>,
    base_conflict: Option<Vec<String>>,
    /// Commit of base + change; `None` when the change conflicts with base.
    merged: Option<String>,
}

impl Prepared {
    fn into_entry(self, risk: Assessment) -> PrEntry {
        PrEntry {
            lines: self.files.iter().map(FileChange::lines).sum(),
            label: self.target.label,
            rev: self.target.rev,
            commit: self.target.commit,
            merge_base: self.merge_base,
            files: self.files,
            base_conflict: self.base_conflict.is_some(),
            base_conflict_files: self.base_conflict.unwrap_or_default(),
            risk,
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn score(
    git: &Git,
    cfg: &Config,
    classifier: &Classifier,
    history: &History,
    base: &Target,
    p: &Prepared,
    conflicts_with: Vec<(String, Vec<String>)>,
    overlaps_with: Vec<(String, Vec<String>)>,
) -> Result<Assessment> {
    let mut signals = risk::gather(
        git,
        &base.commit,
        &p.target.commit,
        p.files.clone(),
        history,
        cfg,
    )?;
    signals.base_conflict = p.base_conflict.clone();
    signals.conflicts_with = conflicts_with;
    signals.overlaps_with = overlaps_with;
    Ok(risk::assess(cfg, classifier, &signals))
}

/// Risk of one change. Unlike `analyse`, only pairs of `head` with each
/// `against` ref are merge-checked, and no merge set is suggested.
pub fn assess_one(
    git: &Git,
    cfg: &Config,
    base: &Target,
    head: Target,
    against: Vec<Target>,
) -> Result<PrEntry> {
    git.require_merge_tree()?;
    let head = prepare(git, base, head)?;
    let mut conflicts_with = Vec::new();
    let mut overlaps_with = Vec::new();
    for other in against {
        let other = prepare(git, base, other)?;
        let shared: Vec<String> = head.paths.intersection(&other.paths).cloned().collect();
        let conflict = if head.merged.is_some() && other.merged.is_some() {
            pair_conflict(git, &head, &other)?
        } else {
            None
        };
        match conflict {
            Some(files) => conflicts_with.push((other.target.label, files)),
            None if !shared.is_empty() => overlaps_with.push((other.target.label, shared)),
            None => {}
        }
    }
    let classifier = Classifier::new(&cfg.paths)?;
    let history = History::load(git, &base.commit, cfg)?;
    let risk = score(
        git,
        cfg,
        &classifier,
        &history,
        base,
        &head,
        conflicts_with,
        overlaps_with,
    )?;
    Ok(head.into_entry(risk))
}

fn prepare(git: &Git, base: &Target, head: Target) -> Result<Prepared> {
    let merge_base = git.merge_base(&base.commit, &head.commit)?;
    let files = git.changed_files(&base.commit, &head.commit)?;
    let paths = files
        .iter()
        .flat_map(|f| f.paths())
        .map(str::to_string)
        .collect();
    let merge = git.merge_tree(&base.commit, &head.commit)?;
    let (base_conflict, merged) = if merge.clean {
        let commit = git.commit_tree(&merge.tree, &[&base.commit, &head.commit])?;
        (None, Some(commit))
    } else {
        (Some(merge.conflicts), None)
    };
    Ok(Prepared {
        target: head,
        merge_base,
        files,
        paths,
        base_conflict,
        merged,
    })
}

/// Does `b` still merge cleanly once `a` is merged into the base?
fn pair_conflict(git: &Git, a: &Prepared, b: &Prepared) -> Result<Option<Vec<String>>> {
    let merged = a.merged.as_deref().expect("caller checks a is clean");
    let result = git.merge_tree(merged, &b.target.commit)?;
    Ok((!result.clean).then_some(result.conflicts))
}

pub fn analyse(
    git: &Git,
    cfg: &Config,
    base: Target,
    heads: Vec<Target>,
    opts: &Options,
) -> Result<Report> {
    git.require_merge_tree()?;
    let prepared: Vec<Prepared> = heads
        .into_iter()
        .map(|h| prepare(git, &base, h))
        .collect::<Result<_>>()?;
    let n = prepared.len();

    // Pairwise overlap and conflict.
    let mut pairs = Vec::new();
    let mut conflicts_of: Vec<Vec<(String, Vec<String>)>> = vec![Vec::new(); n];
    let mut overlaps_of: Vec<Vec<(String, Vec<String>)>> = vec![Vec::new(); n];
    let mut conflict_matrix = vec![vec![false; n]; n];
    let mut overlap_matrix = vec![vec![false; n]; n];
    for i in 0..n {
        for j in (i + 1)..n {
            let (a, b) = (&prepared[i], &prepared[j]);
            let shared: Vec<String> = a.paths.intersection(&b.paths).cloned().collect();
            let checkable = a.merged.is_some() && b.merged.is_some();
            let conflict = if !checkable || (opts.overlapping_only && shared.is_empty()) {
                None
            } else {
                Some(pair_conflict(git, a, b)?)
            };
            if !shared.is_empty() {
                overlap_matrix[i][j] = true;
                overlap_matrix[j][i] = true;
            }
            if let Some(Some(files)) = &conflict {
                conflict_matrix[i][j] = true;
                conflict_matrix[j][i] = true;
                conflicts_of[i].push((b.target.label.clone(), files.clone()));
                conflicts_of[j].push((a.target.label.clone(), files.clone()));
            } else if !shared.is_empty() {
                overlaps_of[i].push((b.target.label.clone(), shared.clone()));
                overlaps_of[j].push((a.target.label.clone(), shared.clone()));
            }
            if shared.is_empty() && !matches!(conflict, Some(Some(_))) {
                continue;
            }
            pairs.push(Pair {
                a: a.target.label.clone(),
                b: b.target.label.clone(),
                shared_files: shared,
                conflict: conflict.as_ref().map(Option::is_some),
                conflict_files: conflict.flatten().unwrap_or_default(),
            });
        }
    }

    // Risk per change, with the conflict picture folded in.
    let classifier = Classifier::new(&cfg.paths)?;
    let history = History::load(git, &base.commit, cfg)?;
    let mut risks = Vec::with_capacity(n);
    for (i, p) in prepared.iter().enumerate() {
        risks.push(score(
            git,
            cfg,
            &classifier,
            &history,
            &base,
            p,
            conflicts_of[i].clone(),
            overlaps_of[i].clone(),
        )?);
    }

    let suggestion = suggest(
        git,
        &base,
        &prepared,
        &risks,
        &conflict_matrix,
        &overlap_matrix,
        opts,
    )?;

    let mut shared: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for p in &prepared {
        for path in &p.paths {
            shared
                .entry(path.clone())
                .or_default()
                .push(p.target.label.clone());
        }
    }
    let shared_files = shared
        .into_iter()
        .filter(|(_, prs)| prs.len() > 1)
        .map(|(path, prs)| SharedFile { path, prs })
        .collect();

    let prs = prepared
        .into_iter()
        .zip(risks)
        .map(|(p, risk)| p.into_entry(risk))
        .collect();

    Ok(Report {
        base,
        prs,
        pairs,
        shared_files,
        suggestion,
    })
}

fn suggest(
    git: &Git,
    base: &Target,
    prepared: &[Prepared],
    risks: &[Assessment],
    conflicts: &[Vec<bool>],
    overlaps: &[Vec<bool>],
    opts: &Options,
) -> Result<Suggestion> {
    let n = prepared.len();
    let mut excluded = Vec::new();
    let mut order: Vec<usize> = Vec::new();
    for (i, p) in prepared.iter().enumerate() {
        if p.base_conflict.is_some() {
            excluded.push(Excluded {
                label: p.target.label.clone(),
                reason: "conflicts with the base; rebase it first".to_string(),
            });
        } else {
            order.push(i);
        }
    }
    let degree = |i: usize| (0..n).filter(|&j| conflicts[i][j]).count();
    order.sort_by(|&a, &b| {
        degree(a)
            .cmp(&degree(b))
            .then(risks[a].score.total_cmp(&risks[b].score))
            .then(a.cmp(&b))
    });

    let mut taken: Vec<usize> = Vec::new();
    let mut tip = base.commit.clone();
    for i in order {
        let label = &prepared[i].target.label;
        let blocker = taken
            .iter()
            .find(|&&t| conflicts[i][t] || (opts.avoid_overlap && overlaps[i][t]));
        if let Some(&t) = blocker {
            let how = if conflicts[i][t] {
                "conflicts with"
            } else {
                "shares files with"
            };
            excluded.push(Excluded {
                label: label.clone(),
                reason: format!(
                    "{how} {}, which is already in the set",
                    prepared[t].target.label
                ),
            });
            continue;
        }
        let merge = git.merge_tree(&tip, &prepared[i].target.commit)?;
        if !merge.clean {
            excluded.push(Excluded {
                label: label.clone(),
                reason: format!(
                    "conflicts with the combined set merged so far ({})",
                    merge.conflicts.join(", ")
                ),
            });
            continue;
        }
        tip = git.commit_tree(&merge.tree, &[&tip, &prepared[i].target.commit])?;
        taken.push(i);
    }
    let mut rule = RULE.to_string();
    if opts.avoid_overlap {
        rule.push_str(" With --avoid-overlap, a shared file also blocks a change.");
    }
    Ok(Suggestion {
        rule,
        merge_order: taken
            .iter()
            .map(|&i| prepared[i].target.label.clone())
            .collect(),
        excluded,
    })
}
