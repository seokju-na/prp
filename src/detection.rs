use crate::changes::{Change, PackageChanges};
use crate::fs_util;
use crate::git::{Commit, Git};
use crate::package::Package;
use crate::release_record::RELEASES_DIR;
use crate::workspace::Workspace;
use anyhow::Result;
use globset::GlobSet;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The changes of every package since its last tag, by position in `Workspace::packages`.
pub fn detect_changes(workspace: &Workspace) -> Result<Vec<PackageChanges>> {
  ChangeDetector::new(workspace)?.detect_all()
}

/// Attributes commits to packages, by the files they touch or by a change file
/// naming the package.
struct ChangeDetector<'a> {
  workspace: &'a Workspace,
  git: &'a Git,
  change_files: Vec<Change>,
  ignores: Vec<GlobSet>,
  changed_paths: HashMap<String, Vec<PathBuf>>,
}

impl<'a> ChangeDetector<'a> {
  fn new(workspace: &'a Workspace) -> Result<Self> {
    let ignores = workspace
      .packages
      .iter()
      .map(|package| {
        let own = &package.config().changes_ignore;
        fs_util::build_glob_set(match own.is_empty() {
          true => &workspace.config.packages.changes_ignore,
          false => own,
        })
      })
      .collect::<Result<_>>()?;

    Ok(Self {
      workspace,
      git: &workspace.git,
      change_files: workspace.changes()?,
      ignores,
      changed_paths: HashMap::new(),
    })
  }

  fn detect_all(mut self) -> Result<Vec<PackageChanges>> {
    let detected = (0..self.workspace.packages.len())
      .map(|index| self.detect(index))
      .collect::<Result<Vec<_>>>()?;
    self.warn_about_unreleased_changes();
    Ok(detected)
  }

  fn detect(&mut self, index: usize) -> Result<PackageChanges> {
    let package = &self.workspace.packages[index];
    let tag = package.tag(&self.workspace.config.git, package.version());
    let since = self.git.tag_exists(&tag).then_some(tag.as_str());

    let mut changes = PackageChanges::default();
    for commit in self.git.commits(since, "HEAD")? {
      self.load_changed_paths(&commit.sha)?;
      let paths = &self.changed_paths[&commit.sha];
      if self.is_release_commit(&commit, paths) {
        continue;
      }

      let touched = self.touches(index, paths);
      let notes = self.notes_for(package, paths, touched);
      if touched || !notes.is_empty() {
        changes.notes.extend(notes);
        changes.commits.push(commit);
      }
    }
    Ok(changes)
  }

  /// Release commits are bookkeeping. The title also catches ones from before
  /// release files, or from another tool.
  fn is_release_commit(&self, commit: &Commit, paths: &[PathBuf]) -> bool {
    let title_format = &self.workspace.config.github.release_pr_title_format;
    let title_prefix = title_format.split('{').next().unwrap_or_default();
    paths.iter().any(|path| path.starts_with(RELEASES_DIR))
      || (!title_prefix.is_empty() && commit.summary.starts_with(title_prefix))
  }

  fn load_changed_paths(&mut self, sha: &str) -> Result<()> {
    if !self.changed_paths.contains_key(sha) {
      let paths = self.git.changed_paths(sha)?;
      self.changed_paths.insert(sha.to_string(), paths);
    }
    Ok(())
  }

  fn touches(&self, index: usize, paths: &[PathBuf]) -> bool {
    let packages = &self.workspace.packages;
    let package_dir = packages[index].relative_path().as_str();
    paths.iter().any(|path| {
      owner_of(packages, path) == Some(index)
        && path
          .strip_prefix(package_dir)
          .is_ok_and(|relative| !self.ignores[index].is_match(relative))
    })
  }

  /// Change files of a commit that belong to `package`: the packages they name,
  /// or else the ones their commit touches.
  fn notes_for(&self, package: &Package, paths: &[PathBuf], touched: bool) -> Vec<Change> {
    self
      .change_files
      .iter()
      .filter(|change| paths.contains(&change.path))
      .filter(|change| match change.packages.is_empty() {
        true => touched,
        false => change.packages.iter().any(|input| package.matches(input)),
      })
      .cloned()
      .collect()
  }

  fn warn_about_unreleased_changes(&self) {
    for change in &self.change_files {
      let committed = self
        .changed_paths
        .values()
        .any(|paths| paths.contains(&change.path));
      if !committed {
        eprintln!(
          "warning: {} is not part of the commits being released",
          change.path.display()
        );
      }
    }
  }
}

/// The deepest package containing `path`. `.prp` is the tool's own state and
/// belongs to nobody, even with a package at the root.
fn owner_of(packages: &[Package], path: &Path) -> Option<usize> {
  if path.starts_with(".prp") {
    return None;
  }
  packages
    .iter()
    .enumerate()
    .filter(|(_, package)| path.starts_with(package.relative_path().as_str()))
    .max_by_key(|(_, package)| {
      Path::new(package.relative_path().as_str())
        .components()
        .count()
    })
    .map(|(index, _)| index)
}
