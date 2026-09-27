use crate::{
  config::Config,
  package::{Package, patterns},
};
use anyhow::{Context, Result, ensure};
use std::{
  collections::BTreeMap,
  fs,
  path::{Path, PathBuf},
};

// Artifact patterns must also be able to match build output under target/ and dist/.
fn files(root: &Path) -> Result<Vec<PathBuf>> {
  if !root.exists() {
    return Ok(vec![]);
  }
  let mut result = vec![];
  for entry in walkdir::WalkDir::new(root)
    .follow_links(false)
    .into_iter()
    .filter_entry(|e| {
      e.depth() == 0
        || !matches!(
          e.file_name().to_str(),
          Some(".git" | "node_modules" | ".prp")
        )
    })
  {
    let entry = entry?;
    if entry.file_type().is_file() {
      result.push(entry.path().strip_prefix(root)?.to_owned());
    }
  }
  result.sort();
  Ok(result)
}

pub fn collect(root: &Path, config: &Config, packages: &[Package], dry_run: bool) -> Result<()> {
  let mut copies = BTreeMap::new();
  for package in packages {
    let base = root.join(&package.path);
    let available = files(&base)?;
    for artifact in &package.config.artifacts {
      let matcher = patterns(artifact.source.values())?;
      let found: Vec<_> = available.iter().filter(|p| matcher.is_match(p)).collect();
      ensure!(
        !found.is_empty(),
        "{}: artifact patterns matched no files: {:?}",
        package.name,
        artifact.source.values()
      );
      for relative in found {
        let destination = artifact.dest.as_ref().map_or_else(
          || relative.clone(),
          |dest| Path::new(dest).join(relative.file_name().unwrap()),
        );
        let to = root
          .join(&config.artifacts.dir)
          .join(&package.path)
          .join(destination);
        let from = base.join(relative);
        if let Some(previous) = copies.insert(to.clone(), from.clone()) {
          ensure!(
            previous == from,
            "artifact destination collision: {}",
            to.display()
          );
        }
      }
    }
  }
  copy_all(&copies, dry_run)
}

pub fn restore(
  root: &Path,
  config: &Config,
  packages: &[Package],
  dry_run: bool,
  allow_missing: bool,
) -> Result<()> {
  let mut copies = BTreeMap::new();
  for package in packages {
    let base = root.join(&config.artifacts.dir).join(&package.path);
    let available = files(&base)?;
    for artifact in &package.config.artifacts {
      let matcher = if let Some(dest) = &artifact.dest {
        let patterns_list = artifact
          .source
          .values()
          .iter()
          .map(|p| {
            format!(
              "{}/{}",
              dest.trim_end_matches('/'),
              Path::new(p).file_name().unwrap().to_string_lossy()
            )
          })
          .collect::<Vec<_>>();
        patterns(&patterns_list)?
      } else {
        patterns(artifact.source.values())?
      };
      let found: Vec<_> = available.iter().filter(|p| matcher.is_match(p)).collect();
      // Local builds may already have artifacts in their final destination.
      if found.is_empty() {
        let local = files(&root.join(&package.path))?
          .iter()
          .any(|p| matcher.is_match(p));
        ensure!(
          allow_missing || local,
          "{}: missing artifacts; run `prp artifacts` in the build job and download {}",
          package.name,
          config.artifacts.dir
        );
      }
      for relative in found {
        copies.insert(root.join(&package.path).join(relative), base.join(relative));
      }
    }
  }
  copy_all(&copies, dry_run)
}

fn copy_all(copies: &BTreeMap<PathBuf, PathBuf>, dry_run: bool) -> Result<()> {
  for (to, from) in copies {
    ensure!(
      from != to,
      "artifact source and destination are identical: {}",
      to.display()
    );
    println!("copy {} -> {}", from.display(), to.display());
    if !dry_run {
      fs::create_dir_all(to.parent().context("missing artifact parent")?)?;
      fs::copy(from, to)?;
    }
  }
  Ok(())
}

pub fn assets(root: &Path, package: &Package, allow_missing: bool) -> Result<Vec<PathBuf>> {
  let base = root.join(&package.path);
  let available = files(&base)?;
  let mut result = Vec::new();
  for pattern in &package.config.assets {
    let matcher = patterns(std::slice::from_ref(pattern))?;
    let found: Vec<_> = available
      .iter()
      .filter(|p| matcher.is_match(p))
      .map(|p| base.join(p))
      .collect();
    ensure!(
      allow_missing || !found.is_empty(),
      "{}: asset pattern {pattern} matched no files",
      package.name
    );
    result.extend(found);
  }
  result.sort();
  result.dedup();
  Ok(result)
}
