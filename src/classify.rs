//! Sort changed paths into categories using the configured glob patterns.

use crate::config::Paths;
use anyhow::{Context, Result};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Docs,
    Tests,
    Generated,
    Lockfiles,
    Dependencies,
    Ci,
    Security,
    Migrations,
    Infra,
}

impl Category {
    pub fn name(self) -> &'static str {
        match self {
            Category::Docs => "docs",
            Category::Tests => "tests",
            Category::Generated => "generated",
            Category::Lockfiles => "lockfiles",
            Category::Dependencies => "dependencies",
            Category::Ci => "ci",
            Category::Security => "security",
            Category::Migrations => "migrations",
            Category::Infra => "infra",
        }
    }

    /// Categories that add flat risk points when any path matches.
    pub fn is_sensitive(self) -> bool {
        matches!(
            self,
            Category::Lockfiles
                | Category::Dependencies
                | Category::Ci
                | Category::Security
                | Category::Migrations
                | Category::Infra
        )
    }
}

pub struct Classifier {
    sets: Vec<(Category, GlobSet)>,
}

impl Classifier {
    pub fn new(paths: &Paths) -> Result<Self> {
        let groups = [
            (Category::Docs, &paths.docs),
            (Category::Tests, &paths.tests),
            (Category::Generated, &paths.generated),
            (Category::Lockfiles, &paths.lockfiles),
            (Category::Dependencies, &paths.dependencies),
            (Category::Ci, &paths.ci),
            (Category::Security, &paths.security),
            (Category::Migrations, &paths.migrations),
            (Category::Infra, &paths.infra),
        ];
        let mut sets = Vec::new();
        for (cat, patterns) in groups {
            let mut builder = GlobSetBuilder::new();
            for p in patterns {
                let glob = GlobBuilder::new(p)
                    .literal_separator(true)
                    .case_insensitive(true)
                    .build()
                    .with_context(|| format!("bad {} pattern {p:?}", cat.name()))?;
                builder.add(glob);
            }
            sets.push((cat, builder.build()?));
        }
        Ok(Classifier { sets })
    }

    pub fn categories(&self, path: &str) -> Vec<Category> {
        self.sets
            .iter()
            .filter(|(_, set)| set.is_match(path))
            .map(|(cat, _)| *cat)
            .collect()
    }
}

/// Code is anything that is not docs, tests, generated output or a lockfile.
pub fn is_code(categories: &[Category]) -> bool {
    !categories.iter().any(|c| {
        matches!(
            c,
            Category::Docs | Category::Tests | Category::Generated | Category::Lockfiles
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classify(path: &str) -> Vec<Category> {
        Classifier::new(&Paths::default()).unwrap().categories(path)
    }

    #[test]
    fn default_patterns() {
        assert_eq!(classify("README.md"), vec![Category::Docs]);
        assert_eq!(classify("docs/guide/intro.html"), vec![Category::Docs]);
        assert_eq!(classify("src/lib.rs"), vec![]);
        assert!(classify("tests/cli.rs").contains(&Category::Tests));
        assert!(classify("web/app.test.ts").contains(&Category::Tests));
        assert!(classify("Cargo.lock").contains(&Category::Lockfiles));
        assert!(classify("app/pubspec.yaml").contains(&Category::Dependencies));
        assert!(classify(".github/workflows/ci.yml").contains(&Category::Ci));
        assert!(classify("server/auth/login.go").contains(&Category::Security));
        assert!(classify("db/migrations/001_init.sql").contains(&Category::Migrations));
        assert!(classify("deploy/Dockerfile").contains(&Category::Infra));
        assert!(classify("lib/model.g.dart").contains(&Category::Generated));
    }

    #[test]
    fn requirements_file_is_both_docs_and_dependency() {
        let cats = classify("requirements.txt");
        assert!(cats.contains(&Category::Docs));
        assert!(cats.contains(&Category::Dependencies));
    }

    #[test]
    fn code_excludes_docs_tests_lockfiles() {
        assert!(is_code(&classify("src/main.rs")));
        assert!(is_code(&classify("Cargo.toml")));
        assert!(!is_code(&classify("README.md")));
        assert!(!is_code(&classify("Cargo.lock")));
        assert!(!is_code(&classify("tests/a.rs")));
    }
}
