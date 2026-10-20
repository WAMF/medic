//! Risk-model configuration. Every weight, threshold and path pattern can be
//! overridden from a TOML file; anything left out keeps its default.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The file medic looks for at the repository root when `--config` is absent.
pub const DEFAULT_FILE: &str = ".medic.toml";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub levels: Levels,
    pub size: Ramp,
    pub files: Ramp,
    pub spread: Ramp,
    pub tests: Tests,
    pub hotspot: Hotspot,
    pub bugfix: Bugfix,
    pub experience: Ramp,
    pub conflict: Conflict,
    pub sensitive: Sensitive,
    pub docs_only: DocsOnly,
    pub paths: Paths,
}

/// Score cut-offs. A score at or above `high` is high; at or above `medium`
/// is medium; anything lower is low.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Levels {
    pub medium: f64,
    pub high: f64,
}

/// A linear ramp: 0 points at or below `low`, `weight` points at or above
/// `high`, linear in between. For `experience` the ramp is inverted: fewer
/// prior commits give more points.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ramp {
    pub weight: f64,
    pub low: f64,
    pub high: f64,
}

impl Ramp {
    pub fn fraction(&self, value: f64) -> f64 {
        ramp(self.low, self.high, value)
    }
}

/// 0 at or below `low`, 1 at or above `high`, linear in between.
pub fn ramp(low: f64, high: f64, value: f64) -> f64 {
    if high <= low {
        return if value >= high { 1.0 } else { 0.0 };
    }
    ((value - low) / (high - low)).clamp(0.0, 1.0)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tests {
    pub weight: f64,
    /// No penalty below this many changed code lines.
    pub min_code_lines: u64,
    /// Test lines per code line that earns zero penalty.
    pub target_ratio: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hotspot {
    pub weight: f64,
    /// Commits touching the busiest changed file at which points start.
    pub low: f64,
    /// Commits touching the busiest changed file that earn full points.
    pub high: f64,
    /// History window on the base branch.
    pub days: u64,
    /// Upper bound on commits read, to keep large repositories fast.
    pub max_commits: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bugfix {
    pub weight: f64,
    pub low: f64,
    pub high: f64,
    /// Case-insensitive words that mark a commit subject as a fix. A word
    /// matches exactly or with an inflection suffix (s, es, ed, d, ing).
    pub keywords: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Conflict {
    /// The head does not merge cleanly into the base.
    pub base: f64,
    /// The head conflicts with another PR.
    pub other: f64,
    /// The head changes a file another PR also changes, without a conflict.
    pub overlap: f64,
}

/// Flat points when any changed path falls in the category.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sensitive {
    pub ci: f64,
    pub security: f64,
    pub migrations: f64,
    pub dependencies: f64,
    pub lockfiles: f64,
    pub infra: f64,
    pub binary: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocsOnly {
    /// Score cap for a change that only touches documentation, has no
    /// sensitive path and has no conflict.
    pub max_score: f64,
}

/// Glob patterns (gitignore-like, `**` crosses directories) per category.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Paths {
    pub docs: Vec<String>,
    pub tests: Vec<String>,
    pub generated: Vec<String>,
    pub lockfiles: Vec<String>,
    pub dependencies: Vec<String>,
    pub ci: Vec<String>,
    pub security: Vec<String>,
    pub migrations: Vec<String>,
    pub infra: Vec<String>,
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

impl Default for Config {
    fn default() -> Self {
        Config {
            levels: Levels::default(),
            size: Ramp {
                weight: 25.0,
                low: 50.0,
                high: 1000.0,
            },
            files: Ramp {
                weight: 15.0,
                low: 5.0,
                high: 40.0,
            },
            spread: Ramp {
                weight: 10.0,
                low: 2.0,
                high: 10.0,
            },
            tests: Tests::default(),
            hotspot: Hotspot::default(),
            bugfix: Bugfix::default(),
            experience: Ramp {
                weight: 5.0,
                low: 5.0,
                high: 50.0,
            },
            conflict: Conflict::default(),
            sensitive: Sensitive::default(),
            docs_only: DocsOnly::default(),
            paths: Paths::default(),
        }
    }
}

impl Default for Levels {
    fn default() -> Self {
        Levels {
            medium: 30.0,
            high: 60.0,
        }
    }
}

impl Default for Tests {
    fn default() -> Self {
        Tests {
            weight: 10.0,
            min_code_lines: 20,
            target_ratio: 0.5,
        }
    }
}

impl Default for Hotspot {
    fn default() -> Self {
        Hotspot {
            weight: 10.0,
            low: 3.0,
            high: 20.0,
            days: 90,
            max_commits: 5000,
        }
    }
}

impl Default for Bugfix {
    fn default() -> Self {
        Bugfix {
            weight: 5.0,
            low: 0.0,
            high: 5.0,
            keywords: strings(&[
                "fix",
                "bug",
                "bugfix",
                "hotfix",
                "revert",
                "regression",
                "broken",
            ]),
        }
    }
}

impl Default for Conflict {
    fn default() -> Self {
        Conflict {
            base: 40.0,
            other: 20.0,
            overlap: 5.0,
        }
    }
}

impl Default for Sensitive {
    fn default() -> Self {
        Sensitive {
            ci: 10.0,
            security: 15.0,
            migrations: 15.0,
            dependencies: 8.0,
            lockfiles: 4.0,
            infra: 8.0,
            binary: 5.0,
        }
    }
}

impl Default for DocsOnly {
    fn default() -> Self {
        DocsOnly { max_score: 10.0 }
    }
}

impl Default for Paths {
    fn default() -> Self {
        Paths {
            docs: strings(&[
                "**/*.md",
                "**/*.mdx",
                "**/*.rst",
                "**/*.adoc",
                "docs/**/*.{md,mdx,rst,adoc,txt,html,png,jpg,jpeg,gif,svg,webp}",
                "doc/**/*.{md,mdx,rst,adoc,txt,html,png,jpg,jpeg,gif,svg,webp}",
                "**/LICENSE*",
                "**/CHANGELOG*",
                "**/AUTHORS*",
            ]),
            tests: strings(&[
                "**/test/**",
                "**/tests/**",
                "**/__tests__/**",
                "**/spec/**",
                "**/integration_test/**",
                "**/testdata/**",
                "**/*_test.*",
                "**/*.test.*",
                "**/*.spec.*",
                "**/test_*.py",
            ]),
            generated: strings(&[
                "**/*.g.dart",
                "**/*.freezed.dart",
                "**/*.pb.go",
                "**/*_pb2.py",
                "**/*.min.js",
                "**/*.min.css",
                "**/generated/**",
                "**/_generated/**",
                "**/vendor/**",
                "**/node_modules/**",
            ]),
            lockfiles: strings(&[
                "**/Cargo.lock",
                "**/package-lock.json",
                "**/npm-shrinkwrap.json",
                "**/yarn.lock",
                "**/pnpm-lock.yaml",
                "**/bun.lock",
                "**/bun.lockb",
                "**/pubspec.lock",
                "**/go.sum",
                "**/poetry.lock",
                "**/uv.lock",
                "**/Pipfile.lock",
                "**/Gemfile.lock",
                "**/composer.lock",
                "**/Podfile.lock",
                "**/gradle.lockfile",
                "**/flake.lock",
            ]),
            dependencies: strings(&[
                "**/Cargo.toml",
                "**/package.json",
                "**/pubspec.yaml",
                "**/go.mod",
                "**/requirements*.txt",
                "**/pyproject.toml",
                "**/setup.py",
                "**/Pipfile",
                "**/Gemfile",
                "**/composer.json",
                "**/Podfile",
                "**/pom.xml",
                "**/build.gradle",
                "**/build.gradle.kts",
                "**/*.csproj",
            ]),
            ci: strings(&[
                ".github/**",
                ".gitlab-ci.yml",
                ".gitlab/**",
                ".circleci/**",
                ".buildkite/**",
                "**/Jenkinsfile",
                "azure-pipelines.yml",
                "bitbucket-pipelines.yml",
                "codemagic.yaml",
            ]),
            security: strings(&[
                "**/*auth*/**",
                "**/*auth*",
                "**/*security*/**",
                "**/*security*",
                "**/*crypto*",
                "**/*secret*",
                "**/*password*",
                "**/*permission*",
                "**/*credential*",
                "**/*token*",
                "**/*session*",
                "**/CODEOWNERS",
            ]),
            migrations: strings(&[
                "**/migrations/**",
                "**/migration/**",
                "**/migrate/**",
                "**/*migration*",
                "**/*.sql",
                "**/schema.*",
            ]),
            infra: strings(&[
                "**/Dockerfile*",
                "**/docker-compose*",
                "**/*.tf",
                "**/*.tfvars",
                "**/terraform/**",
                "**/k8s/**",
                "**/helm/**",
                "**/charts/**",
                "**/.env*",
            ]),
        }
    }
}

impl Config {
    /// Load `explicit` if given, else `<repo root>/.medic.toml` if present,
    /// else the defaults. Returns the config and the file it came from.
    pub fn load(
        explicit: Option<&Path>,
        repo_root: Option<&Path>,
    ) -> Result<(Self, Option<PathBuf>)> {
        let path = match explicit {
            Some(p) => Some(p.to_path_buf()),
            None => repo_root
                .map(|r| r.join(DEFAULT_FILE))
                .filter(|p| p.is_file()),
        };
        match path {
            Some(p) => {
                let text = std::fs::read_to_string(&p)
                    .with_context(|| format!("cannot read config {}", p.display()))?;
                let cfg = Config::from_toml(&text)
                    .with_context(|| format!("invalid config {}", p.display()))?;
                Ok((cfg, Some(p)))
            }
            None => Ok((Config::default(), None)),
        }
    }

    /// Parse a config file. Its values are laid over the defaults key by key,
    /// so a file may set a single weight and keep every other default.
    pub fn from_toml(text: &str) -> Result<Self> {
        let user: toml::Value = toml::from_str(text)?;
        let mut merged = toml::Value::try_from(Config::default())?;
        overlay(&mut merged, user);
        Ok(merged.try_into()?)
    }

    pub fn to_toml(&self) -> String {
        toml::to_string_pretty(self).expect("config serialises")
    }
}

fn overlay(base: &mut toml::Value, user: toml::Value) {
    match (base, user) {
        (toml::Value::Table(b), toml::Value::Table(u)) => {
            for (key, value) in u {
                match b.get_mut(&key) {
                    Some(slot) => overlay(slot, value),
                    None => {
                        b.insert(key, value);
                    }
                }
            }
        }
        (slot, value) => *slot = value,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_file_keeps_other_defaults() {
        let cfg = Config::from_toml("[size]\nweight = 40\n").unwrap();
        assert_eq!(cfg.size.weight, 40.0);
        assert_eq!(cfg.size.high, Config::default().size.high);
        assert_eq!(cfg.files.weight, Config::default().files.weight);
    }

    #[test]
    fn unknown_keys_are_rejected() {
        assert!(Config::from_toml("[size]\nweigth = 1\n").is_err());
        assert!(Config::from_toml("[sizes]\nweight = 1\n").is_err());
    }

    #[test]
    fn defaults_round_trip() {
        let text = Config::default().to_toml();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back.levels.high, 60.0);
    }

    #[test]
    fn ramp_is_clamped_and_linear() {
        let r = Ramp {
            weight: 10.0,
            low: 0.0,
            high: 10.0,
        };
        assert_eq!(r.fraction(-5.0), 0.0);
        assert_eq!(r.fraction(5.0), 0.5);
        assert_eq!(r.fraction(50.0), 1.0);
    }
}
