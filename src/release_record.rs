use crate::bump::BumpRule;
use crate::package::Package;
use crate::plan::Plan;
use anyhow::{Context, Result, ensure};
use semver::Version;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use toml_edit::{DocumentMut, InlineTable, Item, Table, value};

pub const RELEASES_DIR: &str = ".prp/releases";

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BumpRuleRecord {
  #[serde(rename = "type")]
  pub kind: String,
  pub prerelease: Option<String>,
}

impl From<&BumpRule> for BumpRuleRecord {
  fn from(rule: &BumpRule) -> Self {
    Self {
      kind: rule.name().to_string(),
      prerelease: rule.prerelease().map(ToString::to_string),
    }
  }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageRecord {
  pub prev_version: String,
  pub next_version: String,
  pub bump_rule: BumpRuleRecord,
}

/// What `prp prepare` decided, for `prp release` to carry out. Stored at
/// `.prp/releases/<branch>/<id>.toml`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseRecord {
  pub packages: BTreeMap<String, PackageRecord>,
}

impl From<&Plan<'_>> for ReleaseRecord {
  fn from(plan: &Plan) -> Self {
    let packages = plan
      .packages
      .iter()
      .map(|planned| {
        let record = PackageRecord {
          prev_version: planned.package.version().to_string(),
          next_version: planned.next_version().to_string(),
          bump_rule: BumpRuleRecord::from(&planned.rule),
        };
        (planned.package.id(), record)
      })
      .collect();
    Self { packages }
  }
}

impl ReleaseRecord {
  pub fn path(root: &Path, branch: &str, id: &str) -> PathBuf {
    root
      .join(RELEASES_DIR)
      .join(branch)
      .join(format!("{id}.toml"))
  }

  pub fn to_toml(&self) -> String {
    let mut packages = Table::new();
    packages.set_implicit(true);
    for (id, package) in &self.packages {
      let mut table = Table::new();
      table["prev_version"] = value(&package.prev_version);
      table["next_version"] = value(&package.next_version);
      let mut rule = InlineTable::new();
      rule.insert("type", package.bump_rule.kind.as_str().into());
      if let Some(prerelease) = &package.bump_rule.prerelease {
        rule.insert("prerelease", prerelease.as_str().into());
      }
      table["bump_rule"] = value(rule);
      packages.insert(id, Item::Table(table));
    }
    let mut document = DocumentMut::new();
    document["packages"] = Item::Table(packages);
    document.to_string()
  }

  fn load(path: &Path) -> Result<Self> {
    let raw = std::fs::read_to_string(path)
      .with_context(|| format!("Failed to read {}", path.display()))?;
    toml::from_str(&raw).with_context(|| format!("Invalid release file {}", path.display()))
  }

  /// The packages of this record, with their release info.
  pub fn resolve<'a>(
    &'a self,
    packages: &'a [Package],
  ) -> Result<Vec<(&'a Package, &'a PackageRecord)>> {
    self
      .packages
      .iter()
      .map(|(id, record)| {
        let package = packages
          .iter()
          .find(|package| package.id() == *id)
          .with_context(|| format!("Release refers to unknown package \"{id}\""))?;
        Ok((package, record))
      })
      .collect()
  }

  fn matches_workspace(&self, packages: &[Package]) -> Result<bool> {
    for (package, record) in self.resolve(packages)? {
      let next = Version::parse(&record.next_version)
        .with_context(|| format!("Invalid version: {}", record.next_version))?;
      if package.version() != &next {
        return Ok(false);
      }
    }
    Ok(!self.packages.is_empty())
  }

  /// `id` if given, otherwise the newest record of `branch` matching the working
  /// directory. A detached HEAD considers every branch.
  pub fn select(
    root: &Path,
    branch: Option<&str>,
    id: Option<&str>,
    packages: &[Package],
  ) -> Result<(String, Self)> {
    let dir = root.join(RELEASES_DIR);
    let search = branch.map_or(dir.clone(), |branch| dir.join(branch));
    let mut files = vec![];
    if search.exists() {
      for entry in walkdir::WalkDir::new(&search) {
        let entry = entry.with_context(|| format!("Failed to walk {}", search.display()))?;
        if entry.path().extension().is_some_and(|ext| ext == "toml") {
          let stem = entry.path().file_stem().unwrap_or_default();
          files.push((stem.to_string_lossy().to_string(), entry.into_path()));
        }
      }
    }
    // Ids are timestamps, so they sort by age.
    files.sort();

    if let Some(id) = id {
      let (_, path) = files
        .iter()
        .find(|(file_id, _)| file_id == id)
        .with_context(|| format!("Cannot find release \"{id}\" in {}", search.display()))?;
      let record = Self::load(path)?;
      ensure!(
        record.matches_workspace(packages)?,
        "Release \"{id}\" does not match the versions in the working directory"
      );
      return Ok((id.to_string(), record));
    }

    for (file_id, path) in files.iter().rev() {
      let record = Self::load(path)?;
      if record.matches_workspace(packages)? {
        return Ok((file_id.clone(), record));
      }
    }
    anyhow::bail!(
      "Cannot find a release matching the versions in the working directory (searched {})",
      search.display()
    )
  }
}
