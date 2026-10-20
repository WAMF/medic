//! Merge-risk scoring. Gathering signals (git) is kept apart from scoring
//! them (pure), so the model can be unit-tested without a repository.

use crate::classify::{is_code, Category, Classifier};
use crate::config::{ramp, Config};
use crate::git::{FileChange, Git};
use anyhow::Result;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashMap};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Low,
    Medium,
    High,
}

impl Level {
    pub fn name(self) -> &'static str {
        match self {
            Level::Low => "low",
            Level::Medium => "medium",
            Level::High => "high",
        }
    }
}

/// Recent commits on the base branch, read once per run.
pub struct History {
    commits: Vec<(String, Vec<String>)>,
}

/// How often the busiest changed file was touched, and by how many fixes.
#[derive(Debug, Clone, Default, Serialize)]
pub struct HotspotStats {
    pub busiest_file: Option<String>,
    pub busiest_commits: u64,
    pub fix_file: Option<String>,
    pub fix_commits: u64,
}

impl History {
    pub fn load(git: &Git, rev: &str, cfg: &Config) -> Result<Self> {
        Ok(History {
            commits: git.history(rev, cfg.hotspot.days, cfg.hotspot.max_commits)?,
        })
    }

    #[cfg(test)]
    pub fn from_commits(commits: Vec<(String, Vec<String>)>) -> Self {
        History { commits }
    }

    pub fn stats(&self, files: &[FileChange], keywords: &[String]) -> HotspotStats {
        let wanted: BTreeSet<&str> = files.iter().flat_map(|f| f.paths()).collect();
        let keywords: Vec<String> = keywords.iter().map(|k| k.to_lowercase()).collect();
        let mut touches: HashMap<&str, u64> = HashMap::new();
        let mut fixes: HashMap<&str, u64> = HashMap::new();
        for (subject, paths) in &self.commits {
            let is_fix = subject_is_fix(subject, &keywords);
            for p in paths {
                if let Some(&w) = wanted.get(p.as_str()) {
                    *touches.entry(w).or_default() += 1;
                    if is_fix {
                        *fixes.entry(w).or_default() += 1;
                    }
                }
            }
        }
        // Ties break on the path so the output is stable.
        let top = |m: &HashMap<&str, u64>| {
            m.iter()
                .max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(a.0)))
                .map(|(p, n)| (p.to_string(), *n))
        };
        let (busiest_file, busiest_commits) =
            top(&touches).map_or((None, 0), |(p, n)| (Some(p), n));
        let (fix_file, fix_commits) = top(&fixes).map_or((None, 0), |(p, n)| (Some(p), n));
        HotspotStats {
            busiest_file,
            busiest_commits,
            fix_file,
            fix_commits,
        }
    }
}

/// Inflections a fix keyword may carry: `fix` matches `fixes` and `fixed`
/// but not `fixture` or `prefix`.
const SUFFIXES: [&str; 6] = ["", "s", "es", "ed", "d", "ing"];

fn subject_is_fix(subject: &str, keywords: &[String]) -> bool {
    subject
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .any(|word| {
            keywords.iter().any(|k| {
                !k.is_empty()
                    && word
                        .strip_prefix(k.as_str())
                        .is_some_and(|rest| SUFFIXES.contains(&rest))
            })
        })
}

/// Everything the scorer needs to know about one change.
#[derive(Debug, Clone, Default)]
pub struct Signals {
    pub files: Vec<FileChange>,
    pub hotspot: HotspotStats,
    pub author: Option<String>,
    /// Earlier commits by the author on the base branch.
    pub author_commits: Option<u64>,
    /// `Some(paths)` when the change does not merge cleanly into the base.
    pub base_conflict: Option<Vec<String>>,
    /// Other changes this one conflicts with, and the paths involved.
    pub conflicts_with: Vec<(String, Vec<String>)>,
    /// Other changes that share paths with this one without a conflict.
    pub overlaps_with: Vec<(String, Vec<String>)>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Factor {
    pub name: &'static str,
    pub points: f64,
    pub max: f64,
    pub reason: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Metrics {
    pub files: usize,
    pub code_files: usize,
    pub test_files: usize,
    pub doc_files: usize,
    pub binary_files: usize,
    pub lines: u64,
    pub code_lines: u64,
    pub test_lines: u64,
    pub directories: usize,
    /// Paths per category, for the categories that matched anything.
    pub categories: BTreeMap<&'static str, Vec<String>>,
    /// True when any path is in a sensitive category.
    #[serde(skip)]
    pub sensitive: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Assessment {
    pub score: f64,
    pub level: Level,
    pub docs_only: bool,
    /// Factors that added points, highest first.
    pub reasons: Vec<String>,
    pub factors: Vec<Factor>,
    pub metrics: Metrics,
    pub hotspot: HotspotStats,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author_commits: Option<u64>,
}

/// Collect the git-side signals for `base...head`. Conflict signals are left
/// empty for the caller to fill in.
pub fn gather(
    git: &Git,
    base: &str,
    head: &str,
    files: Vec<FileChange>,
    history: &History,
    cfg: &Config,
) -> Result<Signals> {
    let hotspot = history.stats(&files, &cfg.bugfix.keywords);
    let merge_base = git.merge_base(base, head)?;
    let author = git.authors(&merge_base, head)?.into_iter().next();
    let author_commits = match &author {
        Some(email) => Some(git.author_commit_count(
            &merge_base,
            email,
            cfg.experience.high.max(1.0) as u64,
        )?),
        None => None,
    };
    Ok(Signals {
        files,
        hotspot,
        author,
        author_commits,
        ..Signals::default()
    })
}

fn round1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

fn metrics(classifier: &Classifier, files: &[FileChange]) -> Metrics {
    let mut m = Metrics {
        files: files.len(),
        ..Metrics::default()
    };
    let mut dirs = BTreeSet::new();
    for f in files {
        // A rename has two sides. It is docs only if both sides are docs,
        // and code if either side is code, so moving code into docs/ still
        // counts as code. Other categories take the union.
        let sides: Vec<Vec<Category>> = f.paths().map(|p| classifier.categories(p)).collect();
        let mut cats: Vec<Category> = sides.iter().flatten().copied().collect();
        cats.sort();
        cats.dedup();
        let docs = sides.iter().all(|s| s.contains(&Category::Docs));
        let code = sides.iter().any(|s| is_code(s));
        m.lines += f.lines();
        if f.is_binary() {
            m.binary_files += 1;
        }
        m.sensitive |= cats.iter().any(|c| c.is_sensitive());
        for c in &cats {
            m.categories
                .entry(c.name())
                .or_default()
                .push(f.path.clone());
        }
        if docs {
            m.doc_files += 1;
        }
        if cats.contains(&Category::Tests) {
            m.test_files += 1;
            m.test_lines += f.lines();
        }
        if code {
            m.code_files += 1;
            m.code_lines += f.lines();
            let dir = f.path.rsplit_once('/').map_or(".", |(d, _)| d);
            dirs.insert(dir.to_string());
        }
    }
    m.directories = dirs.len();
    m
}

fn paths_note(paths: &[String]) -> String {
    const SHOWN: usize = 3;
    let mut s = paths
        .iter()
        .take(SHOWN)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if paths.len() > SHOWN {
        s.push_str(&format!(" and {} more", paths.len() - SHOWN));
    }
    s
}

/// Score a change. Pure: the same signals and config give the same result.
pub fn assess(cfg: &Config, classifier: &Classifier, signals: &Signals) -> Assessment {
    let m = metrics(classifier, &signals.files);
    let mut factors: Vec<Factor> = Vec::new();
    let mut add = |name: &'static str, points: f64, max: f64, reason: String| {
        factors.push(Factor {
            name,
            points: round1(points),
            max,
            reason,
        });
    };

    let size = &cfg.size;
    add(
        "size",
        size.weight * size.fraction(m.code_lines as f64),
        size.weight,
        format!(
            "{} changed lines in code files ({} in total)",
            m.code_lines, m.lines
        ),
    );
    let files = &cfg.files;
    add(
        "files",
        files.weight * files.fraction(m.code_files as f64),
        files.weight,
        format!(
            "{} code files changed ({} files in total)",
            m.code_files, m.files
        ),
    );
    let spread = &cfg.spread;
    add(
        "spread",
        spread.weight * spread.fraction(m.directories as f64),
        spread.weight,
        format!("code changes span {} directories", m.directories),
    );

    let t = &cfg.tests;
    let (test_points, test_reason) = if m.code_lines < t.min_code_lines {
        (
            0.0,
            format!("only {} code lines changed; no test check", m.code_lines),
        )
    } else {
        let ratio = m.test_lines as f64 / m.code_lines as f64;
        let cover = if t.target_ratio > 0.0 {
            (ratio / t.target_ratio).min(1.0)
        } else {
            1.0
        };
        let reason = if m.test_lines == 0 {
            format!("{} code lines changed with no test changes", m.code_lines)
        } else {
            format!(
                "test-to-code line ratio {:.2} (target {:.2})",
                ratio, t.target_ratio
            )
        };
        (t.weight * (1.0 - cover), reason)
    };
    add("tests", test_points, t.weight, test_reason);

    let h = &cfg.hotspot;
    let hot = &signals.hotspot;
    let hot_frac = ramp(h.low, h.high, hot.busiest_commits as f64);
    add(
        "hotspot",
        h.weight * hot_frac,
        h.weight,
        match &hot.busiest_file {
            Some(p) => format!(
                "{p} changed in {} base commits in the last {} days",
                hot.busiest_commits, h.days
            ),
            None => format!(
                "no changed file was touched on the base in the last {} days",
                h.days
            ),
        },
    );
    let b = &cfg.bugfix;
    let fix_frac = ramp(b.low, b.high, hot.fix_commits as f64);
    add(
        "bugfix_history",
        b.weight * fix_frac,
        b.weight,
        match &hot.fix_file {
            Some(p) => format!("{p} had {} fix commits in the window", hot.fix_commits),
            None => "no recent fix commits on changed files".to_string(),
        },
    );

    let e = &cfg.experience;
    if let Some(n) = signals.author_commits {
        // Inverted ramp: little history with the base earns the points.
        add(
            "experience",
            e.weight * (1.0 - e.fraction(n as f64)),
            e.weight,
            format!(
                "author {} has {}{} earlier commits on the base",
                signals.author.as_deref().unwrap_or("?"),
                n,
                if n as f64 >= e.high { "+" } else { "" }
            ),
        );
    }

    let s = &cfg.sensitive;
    let sensitive = [
        (Category::Ci, s.ci, "CI or workflow config"),
        (Category::Security, s.security, "security-sensitive paths"),
        (
            Category::Migrations,
            s.migrations,
            "database migrations or schema",
        ),
        (
            Category::Dependencies,
            s.dependencies,
            "dependency manifests",
        ),
        (Category::Lockfiles, s.lockfiles, "lockfiles"),
        (
            Category::Infra,
            s.infra,
            "infrastructure or deployment config",
        ),
    ];
    for (cat, weight, what) in sensitive {
        let points = if m.categories.contains_key(cat.name()) {
            weight
        } else {
            0.0
        };
        let reason = match m.categories.get(cat.name()) {
            Some(paths) => format!("touches {what}: {}", paths_note(paths)),
            None => format!("no {what}"),
        };
        add(cat.name(), points, weight, reason);
    }
    let binaries: Vec<String> = signals
        .files
        .iter()
        .filter(|f| f.is_binary())
        .map(|f| f.path.clone())
        .collect();
    add(
        "binary",
        if binaries.is_empty() { 0.0 } else { s.binary },
        s.binary,
        if binaries.is_empty() {
            "no binary files".to_string()
        } else {
            format!(
                "binary files cannot be reviewed as text: {}",
                paths_note(&binaries)
            )
        },
    );

    let c = &cfg.conflict;
    add(
        "base_conflict",
        if signals.base_conflict.is_some() {
            c.base
        } else {
            0.0
        },
        c.base,
        match &signals.base_conflict {
            Some(p) if !p.is_empty() => {
                format!("does not merge cleanly into the base: {}", paths_note(p))
            }
            Some(_) => "does not merge cleanly into the base".to_string(),
            None => "merges cleanly into the base".to_string(),
        },
    );
    let names =
        |v: &[(String, Vec<String>)]| v.iter().map(|x| x.0.clone()).collect::<Vec<_>>().join(", ");
    add(
        "pr_conflict",
        if signals.conflicts_with.is_empty() {
            0.0
        } else {
            c.other
        },
        c.other,
        if signals.conflicts_with.is_empty() {
            "no conflict with the other changes checked".to_string()
        } else {
            format!("conflicts with {}", names(&signals.conflicts_with))
        },
    );
    add(
        "pr_overlap",
        if signals.overlaps_with.is_empty() {
            0.0
        } else {
            c.overlap
        },
        c.overlap,
        if signals.overlaps_with.is_empty() {
            "shares no files with the other changes checked".to_string()
        } else {
            format!("shares files with {}", names(&signals.overlaps_with))
        },
    );

    let raw: f64 = factors.iter().map(|f| f.points).sum();
    let has_conflict = signals.base_conflict.is_some() || !signals.conflicts_with.is_empty();
    let docs_only = m.files > 0
        && m.doc_files == m.files
        && !m.sensitive
        && m.binary_files == 0
        && !has_conflict;
    let mut score = raw.min(100.0);
    if docs_only {
        score = score.min(cfg.docs_only.max_score);
    }
    let score = round1(score);
    let level = if score >= cfg.levels.high {
        Level::High
    } else if score >= cfg.levels.medium {
        Level::Medium
    } else {
        Level::Low
    };

    let mut ranked: Vec<&Factor> = factors.iter().filter(|f| f.points > 0.0).collect();
    ranked.sort_by(|a, b| b.points.total_cmp(&a.points));
    let mut reasons: Vec<String> = ranked
        .iter()
        .map(|f| format!("+{} {}: {}", f.points, f.name, f.reason))
        .collect();
    if m.files == 0 {
        reasons.push("no changes between base and head".to_string());
    } else if docs_only {
        reasons.push(format!(
            "documentation-only change with no conflict: capped at {}",
            cfg.docs_only.max_score
        ));
    }

    Assessment {
        score,
        level,
        docs_only,
        reasons,
        factors,
        metrics: m,
        hotspot: signals.hotspot.clone(),
        author: signals.author.clone(),
        author_commits: signals.author_commits,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn change(path: &str, added: u64) -> FileChange {
        FileChange {
            path: path.into(),
            old_path: None,
            status: 'M',
            added: Some(added),
            deleted: Some(0),
        }
    }

    fn score(files: Vec<FileChange>) -> Assessment {
        let cfg = Config::default();
        let classifier = Classifier::new(&cfg.paths).unwrap();
        assess(
            &cfg,
            &classifier,
            &Signals {
                files,
                ..Signals::default()
            },
        )
    }

    #[test]
    fn docs_only_is_low_and_capped() {
        let a = score(vec![change("README.md", 5000), change("docs/a.md", 400)]);
        assert!(a.docs_only);
        assert_eq!(a.level, Level::Low);
        assert!(a.score <= 10.0);
    }

    #[test]
    fn large_spread_code_change_is_high() {
        let files = (0..60)
            .map(|i| change(&format!("mod{}/src/file{i}.rs", i % 12), 40))
            .collect();
        let a = score(files);
        assert_eq!(a.level, Level::High, "{:?}", a.reasons);
    }

    #[test]
    fn small_tested_change_is_low() {
        let a = score(vec![change("src/a.rs", 10), change("tests/a.rs", 10)]);
        assert_eq!(a.level, Level::Low);
        assert!(!a.docs_only);
    }

    #[test]
    fn base_conflict_raises_risk() {
        let cfg = Config::default();
        let classifier = Classifier::new(&cfg.paths).unwrap();
        let mut s = Signals {
            files: vec![change("README.md", 1)],
            ..Signals::default()
        };
        let clean = assess(&cfg, &classifier, &s);
        s.base_conflict = Some(vec!["README.md".into()]);
        let conflicted = assess(&cfg, &classifier, &s);
        assert!(!conflicted.docs_only);
        assert!(conflicted.score > clean.score + 30.0);
        assert_eq!(conflicted.level, Level::Medium);
    }

    #[test]
    fn sensitive_docs_like_file_is_not_docs_only() {
        let a = score(vec![change("requirements.txt", 2)]);
        assert!(!a.docs_only);
        assert!(a
            .factors
            .iter()
            .any(|f| f.name == "dependencies" && f.points > 0.0));
    }

    #[test]
    fn code_moved_into_docs_is_not_docs_only() {
        let a = score(vec![FileChange {
            path: "docs/x.rs".into(),
            old_path: Some("src/x.rs".into()),
            status: 'R',
            added: Some(0),
            deleted: Some(0),
        }]);
        assert!(!a.docs_only);
        assert_eq!(a.metrics.code_files, 1);
    }

    #[test]
    fn build_txt_file_is_not_docs_only() {
        let a = score(vec![change("CMakeLists.txt", 5)]);
        assert!(!a.docs_only);
    }

    #[test]
    fn fix_keywords_match_whole_words() {
        let k = vec!["fix".to_string(), "bug".to_string()];
        assert!(subject_is_fix("Fixes crash on start", &k));
        assert!(subject_is_fix("Fixed it", &k));
        assert!(subject_is_fix("bugs in parser", &k));
        assert!(!subject_is_fix("bugfix: null check", &k));
        assert!(!subject_is_fix("Add prefix option", &k));
        assert!(!subject_is_fix("Add test fixtures", &k));
        assert!(!subject_is_fix("Wire up bugsnag", &k));
    }

    #[test]
    fn hotspot_counts_renamed_old_path() {
        let history = History::from_commits(vec![
            ("fix login".into(), vec!["old.rs".into()]),
            ("tweak".into(), vec!["old.rs".into(), "x.rs".into()]),
        ]);
        let f = FileChange {
            path: "new.rs".into(),
            old_path: Some("old.rs".into()),
            status: 'R',
            added: Some(1),
            deleted: Some(1),
        };
        let s = history.stats(&[f], &["fix".to_string()]);
        assert_eq!(s.busiest_commits, 2);
        assert_eq!(s.fix_commits, 1);
        assert_eq!(s.busiest_file.as_deref(), Some("old.rs"));
    }
}
