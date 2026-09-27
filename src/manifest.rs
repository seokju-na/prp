use crate::{action::Edits, registry::Registries};
use anyhow::{Context, Result, ensure};
use enum_dispatch::enum_dispatch;
use semver::Version;
use serde_json::Value;
use std::{
  collections::BTreeMap,
  path::{Path, PathBuf},
};

pub const MANIFESTS: &[&str] = &["package.json", "Cargo.toml", "deno.json"];
pub type Versions = BTreeMap<(Registries, String), Version>;
type Reader<'a> = dyn Fn(&Path) -> Result<Option<String>> + 'a;

#[derive(Clone, Debug)]
pub struct Dependency {
  pub name: String,
  pub registry: Registries,
}

#[derive(Clone, Debug)]
pub struct ManifestData {
  path: PathBuf,
  name: String,
  version: Version,
  publish: bool,
  dependencies: Vec<Dependency>,
}

#[enum_dispatch]
pub trait Manifest {
  fn data(&self) -> &ManifestData;
  fn path(&self) -> &Path {
    &self.data().path
  }
  fn name(&self) -> &str {
    &self.data().name
  }
  fn version(&self) -> &Version {
    &self.data().version
  }
  fn can_publish(&self) -> bool {
    self.data().publish
  }
  fn dependencies(&self) -> &[Dependency] {
    &self.data().dependencies
  }
  fn registry(&self) -> Registries;
  fn workspace(&self) -> Option<&Path> {
    None
  }
  fn inherits_version(&self) -> bool {
    false
  }
  fn set_version(&self, edits: &mut Edits, next: &Version) -> Result<()>;
  fn update_dependencies(&self, edits: &mut Edits, versions: &Versions) -> Result<()>;
}

#[enum_dispatch(Manifest)]
#[derive(Clone, Debug)]
pub enum Manifests {
  Npm(NpmManifest),
  Cargo(CargoManifest),
  Jsr(JsrManifest),
}

impl Manifests {
  pub fn read(path: &Path, read: &Reader<'_>) -> Result<Option<Self>> {
    let Some(raw) = read(path)? else {
      return Ok(None);
    };
    let manifest = match path.file_name().and_then(|s| s.to_str()) {
      Some("package.json") => NpmManifest::parse(path, &raw)?.map(Into::into),
      Some("Cargo.toml") => CargoManifest::parse(path, &raw, read)?.map(Into::into),
      Some("deno.json") => JsrManifest::parse(path, &raw)?.map(Into::into),
      _ => anyhow::bail!("unsupported manifest {}", path.display()),
    };
    Ok(manifest)
  }
}

#[derive(Clone, Debug)]
pub struct NpmManifest {
  data: ManifestData,
}
#[derive(Clone, Debug)]
pub struct CargoManifest {
  data: ManifestData,
  workspace: Option<PathBuf>,
  inherited_version: bool,
}
#[derive(Clone, Debug)]
pub struct JsrManifest {
  data: ManifestData,
}

impl ManifestData {
  fn parse(path: &Path, package: &Value, inherited: Option<&str>) -> Result<Option<Self>> {
    if inherited.is_none() && package.get("version").is_none() {
      return Ok(None);
    }
    let version = inherited
      .or_else(|| package["version"].as_str())
      .with_context(|| format!("invalid version in {}", path.display()))?;
    let name = package["name"]
      .as_str()
      .with_context(|| format!("missing package name in {}", path.display()))?
      .to_owned();
    Ok(Some(Self {
      path: path.to_owned(),
      name,
      version: Version::parse(version)?,
      publish: package["private"].as_bool() != Some(true)
        && package["publish"].as_bool() != Some(false)
        && package["publish"].as_array().is_none_or(|a| !a.is_empty()),
      dependencies: vec![],
    }))
  }
}

impl NpmManifest {
  fn parse(path: &Path, raw: &str) -> Result<Option<Self>> {
    let doc = json(raw, false).with_context(|| format!("invalid {}", path.display()))?;
    let Some(mut data) = ManifestData::parse(path, &doc, None)? else {
      return Ok(None);
    };
    for table in ["dependencies", "optionalDependencies", "peerDependencies"] {
      if let Some(deps) = doc[table].as_object() {
        for (key, value) in deps {
          let spec = value
            .as_str()
            .context("dependency specifier must be a string")?;
          let alias = spec.strip_prefix("npm:").map(spec_name).or_else(|| {
            spec
              .strip_prefix("workspace:")
              .filter(|s| s.contains('@'))
              .map(spec_name)
          });
          data.dependencies.push(Dependency {
            name: alias.unwrap_or(key).to_owned(),
            registry: Registries::npm(),
          });
        }
      }
    }
    Ok(Some(Self { data }))
  }
}
impl Manifest for NpmManifest {
  fn data(&self) -> &ManifestData {
    &self.data
  }
  fn registry(&self) -> Registries {
    Registries::npm()
  }
  fn set_version(&self, edits: &mut Edits, next: &Version) -> Result<()> {
    set_json_version(self.path(), edits, next)
  }
  fn update_dependencies(&self, edits: &mut Edits, versions: &Versions) -> Result<()> {
    update_json_dependencies(
      self.path(),
      edits,
      versions,
      &[
        "dependencies",
        "optionalDependencies",
        "peerDependencies",
        "devDependencies",
      ],
    )
  }
}

impl CargoManifest {
  fn parse(path: &Path, raw: &str, read: &Reader<'_>) -> Result<Option<Self>> {
    let doc = json(raw, true).with_context(|| format!("invalid {}", path.display()))?;
    if doc.get("package").is_none() {
      return Ok(None);
    }
    let mut workspace = None;
    let mut workspace_data = Value::Null;
    for ancestor in path.parent().unwrap_or(Path::new("")).ancestors() {
      let candidate = ancestor.join("Cargo.toml");
      if let Some(content) = read(&candidate)? {
        let parsed = json(&content, true)?;
        if parsed.get("workspace").is_some() {
          workspace = Some(candidate);
          workspace_data = parsed;
          break;
        }
      }
    }
    let inherited_version = doc["package"]["version"]["workspace"].as_bool() == Some(true);
    let inherited = if inherited_version {
      Some(
        workspace_data["workspace"]["package"]["version"]
          .as_str()
          .context("workspace.package.version is missing")?,
      )
    } else {
      None
    };
    let Some(mut data) = ManifestData::parse(path, &doc["package"], inherited)? else {
      return Ok(None);
    };
    cargo_deps(&doc, &workspace_data, &mut data.dependencies)?;
    if let Some(targets) = doc["target"].as_object() {
      for target in targets.values() {
        cargo_deps(target, &workspace_data, &mut data.dependencies)?;
      }
    }
    Ok(Some(Self {
      data,
      workspace,
      inherited_version,
    }))
  }
}
impl Manifest for CargoManifest {
  fn data(&self) -> &ManifestData {
    &self.data
  }
  fn registry(&self) -> Registries {
    Registries::cargo()
  }
  fn workspace(&self) -> Option<&Path> {
    self.workspace.as_deref()
  }
  fn inherits_version(&self) -> bool {
    self.inherited_version
  }
  fn set_version(&self, edits: &mut Edits, next: &Version) -> Result<()> {
    let path = if self.inherited_version {
      self.workspace().context("missing workspace")?
    } else {
      self.path()
    };
    let mut doc = edits
      .read(path)?
      .context("manifest disappeared")?
      .parse::<toml_edit::DocumentMut>()?;
    let field = if self.inherited_version {
      &mut doc["workspace"]["package"]["version"]
    } else {
      &mut doc["package"]["version"]
    };
    replace_toml_string(field, &next.to_string());
    edits.write(path, doc.to_string());
    Ok(())
  }
  fn update_dependencies(&self, edits: &mut Edits, versions: &Versions) -> Result<()> {
    update_cargo_dependencies(self.path(), edits, versions)?;
    if let Some(workspace) = self.workspace()
      && workspace != self.path()
    {
      update_cargo_dependencies(workspace, edits, versions)?;
    }
    Ok(())
  }
}

impl JsrManifest {
  fn parse(path: &Path, raw: &str) -> Result<Option<Self>> {
    let doc = json(raw, false).with_context(|| format!("invalid {}", path.display()))?;
    let Some(mut data) = ManifestData::parse(path, &doc, None)? else {
      return Ok(None);
    };
    if let Some(imports) = doc["imports"].as_object() {
      for (key, spec) in imports {
        let Some(spec) = spec.as_str() else {
          continue;
        };
        let (registry, name) = if let Some(s) = spec.strip_prefix("npm:") {
          (Registries::npm(), spec_name(s))
        } else if let Some(s) = spec.strip_prefix("jsr:") {
          (Registries::jsr(), spec_name(s))
        } else {
          (Registries::jsr(), key.as_str())
        };
        data.dependencies.push(Dependency {
          name: name.to_owned(),
          registry,
        });
      }
    }
    Ok(Some(Self { data }))
  }
}
impl Manifest for JsrManifest {
  fn data(&self) -> &ManifestData {
    &self.data
  }
  fn registry(&self) -> Registries {
    Registries::jsr()
  }
  fn set_version(&self, edits: &mut Edits, next: &Version) -> Result<()> {
    set_json_version(self.path(), edits, next)
  }
  fn update_dependencies(&self, edits: &mut Edits, versions: &Versions) -> Result<()> {
    update_json_dependencies(self.path(), edits, versions, &["imports"])
  }
}

pub fn json(raw: &str, cargo: bool) -> Result<Value> {
  if cargo {
    Ok(serde_json::to_value(toml::from_str::<toml::Value>(raw)?)?)
  } else {
    Ok(serde_json::from_str(raw)?)
  }
}

fn set_json_version(path: &Path, edits: &mut Edits, next: &Version) -> Result<()> {
  let mut doc: Value = serde_json::from_str(&edits.read(path)?.context("manifest disappeared")?)?;
  doc["version"] = Value::String(next.to_string());
  edits.write(path, format!("{}\n", serde_json::to_string_pretty(&doc)?));
  Ok(())
}

fn update_cargo_dependencies(path: &Path, edits: &mut Edits, versions: &Versions) -> Result<()> {
  let raw = edits.read(path)?.context("manifest disappeared")?;
  let mut doc = raw.parse::<toml_edit::DocumentMut>()?;
  update_cargo_table(doc.as_table_mut(), versions)?;
  if let Some(workspace) = doc
    .get_mut("workspace")
    .and_then(toml_edit::Item::as_table_mut)
  {
    update_cargo_table(workspace, versions)?;
  }
  if let Some(targets) = doc
    .get_mut("target")
    .and_then(toml_edit::Item::as_table_mut)
  {
    for (_, target) in targets.iter_mut() {
      if let Some(table) = target.as_table_mut() {
        update_cargo_table(table, versions)?;
      }
    }
  }
  let updated = doc.to_string();
  if updated != raw {
    edits.write(path, updated);
  }
  Ok(())
}

fn update_json_dependencies(
  path: &Path,
  edits: &mut Edits,
  versions: &Versions,
  tables: &[&str],
) -> Result<()> {
  let raw = edits.read(path)?.context("manifest disappeared")?;
  let mut doc: Value = serde_json::from_str(&raw)?;
  let mut changed = false;
  for table in tables {
    if let Some(deps) = doc[*table].as_object_mut() {
      for (name, spec) in deps {
        let Some(old) = spec.as_str() else {
          continue;
        };
        let registry = if *table == "imports" && !old.starts_with("npm:") {
          Registries::jsr()
        } else {
          Registries::npm()
        };
        let prefix = ["workspace:", "npm:", "jsr:"]
          .into_iter()
          .find(|p| old.starts_with(p))
          .unwrap_or("");
        let rest = old.strip_prefix(prefix).unwrap_or(old);
        let alias = !prefix.is_empty() && rest.contains('@');
        let actual_name = if alias {
          spec_name(rest)
        } else {
          name.as_str()
        };
        if let Some(next) = versions.get(&(registry, actual_name.to_owned())) {
          let range = if alias {
            &rest[actual_name.len() + 1..]
          } else {
            rest
          };
          let updated = if prefix == "workspace:" && matches!(range, "*" | "^" | "~" | "") {
            old.to_owned()
          } else {
            let range = updated_range(range, next)?;
            if alias {
              format!("{prefix}{actual_name}@{range}")
            } else {
              format!("{prefix}{range}")
            }
          };
          if updated != old {
            *spec = Value::String(updated);
            changed = true;
          }
        }
      }
    }
  }
  if changed {
    edits.write(path, format!("{}\n", serde_json::to_string_pretty(&doc)?));
  }
  Ok(())
}
fn cargo_deps(data: &Value, workspace: &Value, out: &mut Vec<Dependency>) -> Result<()> {
  for table in ["dependencies", "build-dependencies"] {
    if let Some(deps) = data[table].as_object() {
      for (key, entry) in deps {
        let entry = if entry["workspace"].as_bool() == Some(true) {
          workspace["workspace"]["dependencies"]
            .get(key)
            .with_context(|| format!("workspace dependency {key} is missing"))?
        } else {
          entry
        };
        out.push(Dependency {
          name: entry["package"].as_str().unwrap_or(key).into(),
          registry: Registries::cargo(),
        });
      }
    }
  }
  Ok(())
}

fn spec_name(spec: &str) -> &str {
  let start = usize::from(spec.starts_with('@'));
  spec[start..]
    .find('@')
    .map_or(spec, |idx| &spec[..start + idx])
}

fn replace_toml_string(item: &mut toml_edit::Item, next: &str) {
  let decor = item.as_value().map(|v| v.decor().clone());
  *item = toml_edit::value(next);
  if let (Some(decor), Some(value)) = (decor, item.as_value_mut()) {
    *value.decor_mut() = decor;
  }
}

fn update_cargo_table(table: &mut toml_edit::Table, versions: &Versions) -> Result<()> {
  for key in ["dependencies", "build-dependencies", "dev-dependencies"] {
    let Some(deps) = table
      .get_mut(key)
      .and_then(toml_edit::Item::as_table_like_mut)
    else {
      continue;
    };
    for (name, entry) in deps.iter_mut() {
      let actual = entry
        .get("package")
        .and_then(toml_edit::Item::as_str)
        .unwrap_or(name.get());
      let Some(next) = versions.get(&(Registries::cargo(), actual.to_owned())) else {
        continue;
      };
      if let Some(old) = entry.as_str() {
        let updated = updated_range(old, next)?;
        replace_toml_string(entry, &updated);
      } else if entry.get("workspace").and_then(toml_edit::Item::as_bool) != Some(true) {
        if let Some(old) = entry.get("version").and_then(toml_edit::Item::as_str) {
          let updated = updated_range(old, next)?;
          replace_toml_string(&mut entry["version"], &updated);
        } else if entry.get("path").is_some() {
          entry["version"] = toml_edit::value(next.to_string());
        }
      }
    }
  }
  Ok(())
}

fn updated_range(range: &str, version: &Version) -> Result<String> {
  if range == "*" {
    return Ok(range.into());
  }
  let (prefix, digits) = match range.chars().next() {
    Some('^' | '~' | '=') => (&range[..1], range[1..].trim()),
    _ => ("", range.trim()),
  };
  ensure!(
    Version::parse(digits).is_ok(),
    "cannot automatically update internal dependency range '{range}'; use an exact, ^, ~, =, or workspace range"
  );
  Ok(format!("{prefix}{version}"))
}
