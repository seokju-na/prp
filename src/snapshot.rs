use crate::manifest::{Manifest, ReleasedVersions};
use crate::workspace::Workspace;
use anyhow::{Context, Result};
use chrono::NaiveDate;
use semver::{BuildMetadata, Prerelease, Version};

/// Every manifest of the workspace at its current version, with `prerelease`.
#[derive(Debug)]
pub struct Snapshot<'a> {
  pub manifests: Vec<(&'a Manifest, Version)>,
}

impl<'a> Snapshot<'a> {
  pub fn new(workspace: &'a Workspace, prerelease: &Prerelease) -> Self {
    let manifests = workspace
      .packages
      .iter()
      .flat_map(|package| package.manifests())
      .map(|manifest| (manifest, snapshot_version(manifest.version(), prerelease)))
      .collect();
    Self { manifests }
  }

  pub fn version_of(&self, manifest: &Manifest) -> Result<&Version> {
    self
      .manifests
      .iter()
      .find(|(candidate, _)| candidate.path() == manifest.path())
      .map(|(_, version)| version)
      .with_context(|| format!("{} has no snapshot version", manifest.name()))
  }

  pub fn versions(&self) -> ReleasedVersions {
    self
      .manifests
      .iter()
      .map(|(manifest, version)| {
        let key = (manifest.kind(), manifest.name().to_string());
        (key, version.clone())
      })
      .collect()
  }

  pub fn prerelease(format: &str, sha: &str, date: Option<NaiveDate>) -> Result<Prerelease> {
    let date = date.unwrap_or_else(|| chrono::Local::now().date_naive());
    let id = format
      .replace("<sha>", sha)
      .replace("<yyyymmdd>", &date.format("%Y%m%d").to_string());
    Prerelease::new(&id).with_context(|| format!("Invalid snapshot prerelease: {id}"))
  }

  /// First `-` segment of the format, e.g. "snapshot-<sha>-<yyyymmdd>" -> "snapshot".
  pub fn default_dist_tag(format: &str) -> Option<&str> {
    let prefix = format.split('-').next().unwrap_or_default();
    (!prefix.is_empty() && !prefix.contains('<')).then_some(prefix)
  }
}

/// `current` with its prerelease and build metadata replaced by `prerelease`.
fn snapshot_version(current: &Version, prerelease: &Prerelease) -> Version {
  Version {
    pre: prerelease.clone(),
    build: BuildMetadata::EMPTY,
    ..current.clone()
  }
}
