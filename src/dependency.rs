use crate::manifest::ManifestKind;
use crate::serde_util;
use semver::Version;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DependencyVersion {
  Version(String),
  Workspace,
  Other,
}

impl DependencyVersion {
  pub fn from_npm(str: &str) -> DependencyVersion {
    if let Ok(ver) = semver::Version::parse(str) {
      return DependencyVersion::Version(ver.to_string());
    }
    if str.starts_with("workspace:") {
      return DependencyVersion::Workspace;
    }
    DependencyVersion::Other
  }

  pub fn from_cargo(value: &Value) -> DependencyVersion {
    if let Some(ver) = value.as_str()
      && let Ok(ver) = semver::Version::parse(ver)
    {
      return DependencyVersion::Version(ver.to_string());
    }
    if let Ok(Some(true)) = serde_util::parse_bool(value, "workspace") {
      return DependencyVersion::Workspace;
    }
    DependencyVersion::Other
  }
}

/// A package a manifest depends on, in the registry of `kind`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dependency {
  pub kind: ManifestKind,
  pub name: String,
  pub version: DependencyVersion,
}

pub type Dependencies = Vec<Dependency>;

/// `current` pointed at `next`, keeping its operator. `None` for protocols
/// (`workspace:`), wildcards and compound ranges.
pub fn next_requirement(current: &str, next: &Version) -> Option<String> {
  let start = current.find(|c: char| c.is_ascii_digit())?;
  let (operator, version) = current.split_at(start);
  let plain = version
    .chars()
    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'));
  let wildcard = version.split('.').any(|part| part == "x");
  (matches!(operator, "" | "^" | "~" | "=" | ">=") && plain && !wildcard)
    .then(|| format!("{operator}{next}"))
}
