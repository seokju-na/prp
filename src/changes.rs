use crate::bump::{self, BumpRule};
use crate::git::Commit;
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

pub const CHANGES_DIR: &str = ".prp/changes";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Frontmatter {
  title: String,
  packages: Option<String>,
  #[serde(default)]
  breaking: bool,
}

/// A notable change written with `prp change`, kept in `.prp/changes`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
  /// Relative to the workspace root.
  pub path: PathBuf,
  pub title: String,
  /// Packages named explicitly. Empty means the ones its commit touches.
  pub packages: Vec<String>,
  pub breaking: bool,
  pub body: String,
}

/// What goes into a package's release.
#[derive(Debug, Clone, Default)]
pub struct PackageChanges {
  pub notes: Vec<Change>,
  pub commits: Vec<Commit>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ChangeId {
  Note(PathBuf),
  Commit(String),
}

impl PackageChanges {
  pub fn is_empty(&self) -> bool {
    self.notes.is_empty() && self.commits.is_empty()
  }

  /// The highest bump the changes ask for, `None` without any.
  pub fn rule(&self) -> Option<BumpRule> {
    let commits = self
      .commits
      .iter()
      .map(|commit| bump::commit_bump_rule(&commit.summary, &commit.body));
    let notes = self
      .notes
      .iter()
      .filter(|note| note.breaking)
      .map(|_| BumpRule::Major);
    commits.chain(notes).reduce(BumpRule::max)
  }

  pub fn without(&self, excluded: &BTreeSet<ChangeId>) -> Self {
    Self {
      notes: self
        .notes
        .iter()
        .filter(|note| !excluded.contains(&note.id()))
        .cloned()
        .collect(),
      commits: self
        .commits
        .iter()
        .filter(|commit| !excluded.contains(&ChangeId::Commit(commit.sha.clone())))
        .cloned()
        .collect(),
    }
  }
}

impl Change {
  /// `<timestamp>.md` under the changes directory, suffixed while that is taken.
  pub fn new_path(root: &Path, timestamp: &str) -> PathBuf {
    (1..)
      .map(|n| match n {
        1 => Path::new(CHANGES_DIR).join(format!("{timestamp}.md")),
        n => Path::new(CHANGES_DIR).join(format!("{timestamp}-{n}.md")),
      })
      .find(|path| !root.join(path).exists())
      .expect("an unbounded range always finds a free name")
  }

  pub fn id(&self) -> ChangeId {
    ChangeId::Note(self.path.clone())
  }

  pub fn load_all(root: &Path) -> Result<Vec<Self>> {
    let dir = root.join(CHANGES_DIR);
    if !dir.exists() {
      return Ok(vec![]);
    }
    let mut paths = fs::read_dir(&dir)
      .with_context(|| format!("Failed to read {}", dir.display()))?
      .map(|entry| entry.map(|entry| entry.path()))
      .collect::<std::io::Result<Vec<_>>>()?;
    paths.sort();

    let mut changes = vec![];
    for path in paths {
      if path.extension().is_none_or(|ext| ext != "md") {
        continue;
      }
      let raw =
        fs::read_to_string(&path).with_context(|| format!("Failed to read {}", path.display()))?;
      let relative = path.strip_prefix(root).unwrap_or(&path);
      let change = Self::parse(relative, &raw)
        .with_context(|| format!("Invalid change file {}", path.display()))?;
      changes.push(change);
    }
    Ok(changes)
  }

  pub fn parse(path: &Path, raw: &str) -> Result<Self> {
    let raw = raw.replace("\r\n", "\n");
    let (frontmatter, body) = raw
      .strip_prefix("---\n")
      .and_then(|rest| rest.split_once("\n---"))
      .context("Change file must start with YAML frontmatter")?;
    let frontmatter: Frontmatter =
      serde_yaml_ng::from_str(frontmatter).context("Invalid frontmatter")?;
    ensure!(
      !frontmatter.title.trim().is_empty(),
      "Field \"title\" must not be empty"
    );

    let packages = frontmatter
      .packages
      .unwrap_or_default()
      .split(',')
      .map(|package| package.trim().to_string())
      .filter(|package| !package.is_empty())
      .collect();
    Ok(Self {
      path: path.to_path_buf(),
      title: frontmatter.title,
      packages,
      breaking: frontmatter.breaking,
      body: body.trim().to_string(),
    })
  }

  pub fn to_markdown(&self) -> Result<String> {
    let mut out = String::from("---\n");
    // A JSON string is a valid YAML double-quoted scalar.
    out.push_str(&format!("title: {}\n", serde_json::to_string(&self.title)?));
    if !self.packages.is_empty() {
      out.push_str(&format!("packages: {}\n", self.packages.join(", ")));
    }
    out.push_str(&format!("breaking: {}\n---\n", self.breaking));
    if !self.body.is_empty() {
      out.push_str(&format!("\n{}\n", self.body));
    }
    Ok(out)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_taken_timestamp_gets_a_suffix() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let first = Change::new_path(root, "20261107120000");
    assert_eq!(first, Path::new(".prp/changes/20261107120000.md"));

    fs::create_dir_all(root.join(CHANGES_DIR)).unwrap();
    fs::write(root.join(&first), "").unwrap();
    let second = Change::new_path(root, "20261107120000");
    assert_eq!(second, Path::new(".prp/changes/20261107120000-2.md"));
  }
}
