//! medic: git-only pull request triage.

mod classify;
mod config;
mod git;
mod overlap;
mod refs;
mod risk;

use anyhow::Result;
use clap::{Parser, Subcommand};
use config::Config;
use git::Git;
use risk::Level;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;

/// Exit code when `--fail-on` is met.
const EXIT_THRESHOLD: u8 = 3;

#[derive(Parser)]
#[command(
    name = "medic",
    version,
    about = "Git-only pull request triage: file overlap, real merge conflicts and merge risk.",
    long_about = "Git-only pull request triage. medic shells out to git and never calls a \
hosting API. Diffs are three-dot (base...head), like a GitHub pull request. The only \
network call is `git fetch`, and only with --fetch."
)]
struct Cli {
    /// Run as if medic was started in this directory.
    #[arg(short = 'C', global = true, value_name = "DIR", default_value = ".")]
    dir: PathBuf,
    /// Print JSON for agents instead of text.
    #[arg(long, global = true)]
    json: bool,
    /// Risk config file. Default: .medic.toml at the repository root, if present.
    #[arg(long, global = true, value_name = "FILE")]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List the files each change touches, flag overlaps and real conflicts,
    /// and suggest a set of changes that merge together cleanly.
    Overlap {
        /// Base ref the changes target, for example main or origin/main.
        #[arg(long, short)]
        base: String,
        /// Changes to compare: branches, refs, commits, or PR numbers (123 or #123).
        #[arg(required = true, value_name = "REF")]
        heads: Vec<String>,
        /// Fetch the base and every change from this remote first. PR numbers
        /// are fetched from refs/pull/<n>/head.
        #[arg(long, value_name = "REMOTE")]
        fetch: Option<String>,
        /// Also keep changes that share a file out of the suggested set.
        #[arg(long)]
        avoid_overlap: bool,
        /// Only merge-check pairs that share a file. Faster for large groups.
        #[arg(long)]
        overlapping_only: bool,
        /// List every changed file per change in text output.
        #[arg(long)]
        files: bool,
    },
    /// Score the merge risk of one change (base...head) as low, medium or high.
    Risk {
        /// Base ref the change targets.
        #[arg(long, short)]
        base: String,
        /// The change: a branch, ref, commit or PR number.
        #[arg(value_name = "REF")]
        head: String,
        /// Other changes to check for overlap and conflict. A conflict raises the risk.
        #[arg(long, value_name = "REF", num_args = 1..)]
        against: Vec<String>,
        /// Fetch the base, the head and --against refs from this remote first.
        #[arg(long, value_name = "REMOTE")]
        fetch: Option<String>,
        /// Exit with code 3 when the level is at or above this level.
        #[arg(long, value_enum, value_name = "LEVEL")]
        fail_on: Option<Level>,
    },
    /// Print the risk config in effect (or the built-in defaults) as TOML.
    Config {
        /// Print the built-in defaults, ignoring any config file.
        #[arg(long)]
        defaults: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("medic: {err:#}");
            ExitCode::from(1)
        }
    }
}

fn run(cli: Cli) -> Result<ExitCode> {
    let git = Git::open(&cli.dir)?;
    let root = git.top_level();
    let (cfg, source) = Config::load(cli.config.as_deref(), root.as_deref())?;
    match cli.command {
        Command::Config { defaults } => {
            if defaults {
                print!("{}", Config::default().to_toml());
            } else {
                match &source {
                    Some(p) => println!("# from {}", p.display()),
                    None => println!("# built-in defaults (no {} found)", config::DEFAULT_FILE),
                }
                print!("{}", cfg.to_toml());
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Overlap {
            base,
            heads,
            fetch,
            avoid_overlap,
            overlapping_only,
            files,
        } => {
            let (base, heads) = refs::resolve(&git, &base, &heads, fetch.as_deref())?;
            let opts = overlap::Options {
                avoid_overlap,
                overlapping_only,
            };
            let report = overlap::analyse(&git, &cfg, base, heads, &opts)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print!("{}", overlap_text(&report, files));
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Risk {
            base,
            head,
            against,
            fetch,
            fail_on,
        } => {
            let mut heads = vec![head];
            heads.extend(against);
            let (base, heads) = refs::resolve(&git, &base, &heads, fetch.as_deref())?;
            let report = overlap::analyse(&git, &cfg, base, heads, &overlap::Options::default())?;
            let entry = &report.prs[0];
            if cli.json {
                let out = serde_json::json!({
                    "base": report.base,
                    "head": { "label": entry.label, "ref": entry.rev, "commit": entry.commit },
                    "merge_base": entry.merge_base,
                    "config": source.as_ref().map(|p| p.display().to_string()),
                    "risk": entry.risk,
                });
                println!("{}", serde_json::to_string_pretty(&out)?);
            } else {
                print!("{}", risk_text(&report, source.as_ref()));
            }
            let tripped = fail_on.is_some_and(|l| entry.risk.level >= l);
            Ok(if tripped {
                ExitCode::from(EXIT_THRESHOLD)
            } else {
                ExitCode::SUCCESS
            })
        }
    }
}

fn short(commit: &str) -> &str {
    &commit[..commit.len().min(10)]
}

/// Join paths for text output; long lists are cut unless `full`.
fn join(paths: &[String], full: bool) -> String {
    const SHOWN: usize = 5;
    if full || paths.len() <= SHOWN {
        return paths.join(", ");
    }
    format!(
        "{}, and {} more",
        paths[..SHOWN].join(", "),
        paths.len() - SHOWN
    )
}

fn overlap_text(r: &overlap::Report, list_files: bool) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "base {} ({})", r.base.label, short(&r.base.commit));
    let _ = writeln!(s, "\nChanges ({}):", r.prs.len());
    let width = r.prs.iter().map(|p| p.label.len()).max().unwrap_or(0);
    for p in &r.prs {
        let state = if p.base_conflict {
            "CONFLICTS WITH BASE"
        } else {
            "merges cleanly"
        };
        let _ = writeln!(
            s,
            "  {:width$}  {:>4} files  {:>6} lines  risk {:<6} {:>5}  {}",
            p.label,
            p.files.len(),
            p.lines,
            p.risk.level.name(),
            p.risk.score,
            state,
        );
        if list_files {
            for f in &p.files {
                match &f.old_path {
                    Some(old) => {
                        let _ = writeln!(s, "      {} {} -> {}", f.status, old, f.path);
                    }
                    None => {
                        let _ = writeln!(s, "      {} {}", f.status, f.path);
                    }
                }
            }
        }
        if !p.base_conflict_files.is_empty() {
            let _ = writeln!(
                s,
                "      base conflict: {}",
                join(&p.base_conflict_files, list_files)
            );
        }
    }

    let _ = writeln!(s, "\nShared files ({}):", r.shared_files.len());
    if r.shared_files.is_empty() {
        let _ = writeln!(s, "  none");
    }
    for f in &r.shared_files {
        let _ = writeln!(s, "  {}  <- {}", f.path, f.prs.join(", "));
    }

    let conflicts: Vec<_> = r
        .pairs
        .iter()
        .filter(|p| p.conflict == Some(true))
        .collect();
    let _ = writeln!(s, "\nConflicts between changes ({}):", conflicts.len());
    if conflicts.is_empty() {
        let _ = writeln!(s, "  none");
    }
    for p in &conflicts {
        let _ = writeln!(
            s,
            "  {} x {}: {}",
            p.a,
            p.b,
            join(&p.conflict_files, list_files)
        );
    }
    let overlaps: Vec<_> = r
        .pairs
        .iter()
        .filter(|p| p.conflict != Some(true))
        .collect();
    if !overlaps.is_empty() {
        let _ = writeln!(s, "\nOverlap without conflict ({}):", overlaps.len());
        for p in overlaps {
            let note = if p.conflict.is_none() {
                " (not merge-checked)"
            } else {
                ""
            };
            let _ = writeln!(
                s,
                "  {} + {}: {}{}",
                p.a,
                p.b,
                join(&p.shared_files, list_files),
                note
            );
        }
    }

    let sug = &r.suggestion;
    let _ = writeln!(
        s,
        "\nSuggested merge set ({} of {}):",
        sug.merge_order.len(),
        r.prs.len()
    );
    for (i, label) in sug.merge_order.iter().enumerate() {
        let _ = writeln!(s, "  {}. {}", i + 1, label);
    }
    if !sug.excluded.is_empty() {
        let _ = writeln!(s, "Left out:");
        for e in &sug.excluded {
            let _ = writeln!(s, "  {}: {}", e.label, e.reason);
        }
    }
    let _ = writeln!(s, "\nRule: {}", sug.rule);
    s
}

fn risk_text(r: &overlap::Report, source: Option<&PathBuf>) -> String {
    let p = &r.prs[0];
    let a = &p.risk;
    let m = &a.metrics;
    let mut s = String::new();
    let _ = writeln!(
        s,
        "{} ({}) vs {} ({})",
        p.label,
        short(&p.commit),
        r.base.label,
        short(&r.base.commit)
    );
    let _ = writeln!(
        s,
        "risk: {}  score {}/100",
        a.level.name().to_uppercase(),
        a.score
    );
    let _ = writeln!(
        s,
        "{} files ({} code, {} test, {} docs, {} binary), {} lines ({} code, {} test), {} code directories",
        m.files, m.code_files, m.test_files, m.doc_files, m.binary_files, m.lines, m.code_lines, m.test_lines, m.directories
    );
    let _ = writeln!(s, "\nReasons:");
    if a.reasons.is_empty() {
        let _ = writeln!(s, "  no risk factors scored");
    }
    for reason in &a.reasons {
        let _ = writeln!(s, "  {reason}");
    }
    let _ = writeln!(
        s,
        "\nConfig: {}",
        source.map_or("built-in defaults".to_string(), |p| p.display().to_string())
    );
    s
}
