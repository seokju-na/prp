use crate::dependency::Dependencies;
use crate::edit::{self, Edit, Patch};
use crate::{cargo, deno, npm, serde_util};
use anyhow::{Context, Result, ensure};
use semver::Version;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Versions going out in a release, by manifest kind and name.
pub type ReleasedVersions = HashMap<(ManifestKind, String), Version>;

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ManifestKind {
  PackageJson,
  CargoToml,
  DenoJson,
}

impl ManifestKind {
  pub fn file_names() -> Vec<&'static str> {
    let mut names = vec!["package.json", "Cargo.toml"];
    names.extend(deno::FILE_NAMES);
    names
  }
}

#[derive(Clone, Debug)]
pub struct Manifest {
  kind: ManifestKind,
  path: PathBuf,
  name: String,
  version: Version,
  publish: bool,
  dependencies: Dependencies,
  raw: String,
}

impl Manifest {
  pub fn parse(path: &Path, data: Vec<u8>) -> Result<Option<Self>> {
    match path.file_name().and_then(|x| x.to_str()) {
      Some("package.json") => Self::parse_package_json(path, data),
      Some("Cargo.toml") => Self::parse_cargo_toml(path, data),
      Some(name) if deno::FILE_NAMES.contains(&name) => Self::parse_deno_json(path, data),
      _ => anyhow::bail!("Unsupported manifest file: {}", path.display()),
    }
  }

  pub fn kind(&self) -> ManifestKind {
    self.kind
  }

  pub fn path(&self) -> &Path {
    &self.path
  }

  pub fn name(&self) -> &str {
    &self.name
  }

  pub fn version(&self) -> &Version {
    &self.version
  }

  pub fn dependencies(&self) -> &Dependencies {
    &self.dependencies
  }

  pub fn can_publish(&self) -> bool {
    self.publish
  }

  /// The directory a publish runs from.
  pub fn dir(&self) -> &Path {
    self.path.parent().unwrap_or(Path::new(""))
  }

  /// The edit moving this manifest to `next` and pointing its dependencies at
  /// `versions`.
  pub fn release_patch(&self, next: &Version, versions: &ReleasedVersions) -> Result<Patch> {
    match self.kind {
      ManifestKind::PackageJson => {
        let pkg_json: Value = serde_json::from_str(&self.raw).context("Failed to parse JSON")?;
        let mut edits: Vec<Edit> = vec![(vec!["version".into()], next.to_string())];
        edits.extend(npm::dependency_edits(&pkg_json, versions));
        edit::edit_json(&self.raw, &edits)
      }
      ManifestKind::CargoToml => {
        let cargo = cargo::parse_cargo_toml(self.raw.as_bytes())?;
        let mut edits: Vec<Edit> =
          vec![(vec!["package".into(), "version".into()], next.to_string())];
        edits.extend(cargo::dependency_edits(&cargo, versions));
        edit::edit_toml(&self.raw, &edits)
      }
      ManifestKind::DenoJson => {
        let deno_json = deno::parse_deno_json(&self.raw)?;
        let mut edits: Vec<Edit> = vec![(vec!["version".into()], next.to_string())];
        edits.extend(deno::dependency_edits(&deno_json, versions));
        edit::edit_json(&self.raw, &edits)
      }
    }
  }

  fn parse_package_json(path: &Path, bytes: Vec<u8>) -> Result<Option<Self>> {
    let raw = String::from_utf8(bytes).context("Manifest must be UTF-8")?;
    let pkg_json: Value = serde_json::from_str(&raw).context("Failed to parse JSON")?;
    pkg_json
      .as_object()
      .context("Manifest must be a JSON object")?;
    let Some(version) = serde_util::parse_version(&pkg_json, "version")? else {
      return Ok(None);
    };
    let name = serde_util::parse_required_str(&pkg_json, "name")?;
    let private = serde_util::parse_bool(&pkg_json, "private")?;
    let dependencies = npm::get_npm_dependencies(&pkg_json);
    let manifest = Self {
      kind: ManifestKind::PackageJson,
      path: path.to_path_buf(),
      name,
      version,
      publish: !private.unwrap_or(false),
      dependencies,
      raw,
    };
    Ok(Some(manifest))
  }

  fn parse_deno_json(path: &Path, bytes: Vec<u8>) -> Result<Option<Self>> {
    let raw = String::from_utf8(bytes).context("Manifest must be UTF-8")?;
    let deno_json = deno::parse_deno_json(&raw)?;
    deno_json
      .as_object()
      .context("Manifest must be a JSON object")?;
    // A workspace root configures Deno without being a package.
    let Some(version) = serde_util::parse_version(&deno_json, "version")? else {
      return Ok(None);
    };
    let manifest = Self {
      kind: ManifestKind::DenoJson,
      path: path.to_path_buf(),
      name: serde_util::parse_required_str(&deno_json, "name")?,
      version,
      publish: true,
      dependencies: deno::get_deno_dependencies(&deno_json),
      raw,
    };
    Ok(Some(manifest))
  }

  fn parse_cargo_toml(path: &Path, bytes: Vec<u8>) -> Result<Option<Self>> {
    let cargo = cargo::parse_cargo_toml(&bytes)?;
    let raw = String::from_utf8(bytes).context("Manifest must be UTF-8")?;

    // A virtual workspace manifest has no `[package]`, hence no version either.
    let Some(package) = cargo.get("package") else {
      return Ok(None);
    };
    let Some(version) = serde_util::parse_version(package, "version")? else {
      return Ok(None);
    };
    let name = serde_util::parse_required_str(package, "name")?;
    let publish = match package.get("publish") {
      None => true,
      Some(Value::Bool(publish)) => *publish,
      Some(Value::Array(registries)) => {
        ensure!(
          registries.iter().all(Value::is_string),
          "Field \"publish\" must contain registry names"
        );
        !registries.is_empty()
      }
      _ => anyhow::bail!("Field \"publish\" must be a boolean or an array of registry names"),
    };
    let dependencies = cargo::get_cargo_dependencies(&cargo);
    let manifest = Self {
      kind: ManifestKind::CargoToml,
      path: path.to_path_buf(),
      name,
      version,
      publish,
      dependencies,
      raw,
    };
    Ok(Some(manifest))
  }
}
