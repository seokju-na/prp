use crate::registry::Registry;
use crate::{
  config::{Config, PackageConfig, relative_path},
  manifest::{MANIFESTS, Manifest, Manifests, json},
};
use anyhow::{Context, Result, ensure};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use std::{
  collections::{BTreeMap, BTreeSet},
  fs,
  path::{Path, PathBuf},
};
use walkdir::WalkDir;

#[derive(Clone, Debug)]
pub struct Package {
  pub path: PathBuf,
  pub name: String,
  pub display_name: String,
  pub manifests: Vec<Manifests>,
  pub config: PackageConfig,
}

impl Package {
  pub fn version(&self) -> &semver::Version {
    self.manifests[0].version()
  }
  pub fn id(&self) -> String {
    if self.path.as_os_str().is_empty() {
      ".".into()
    } else {
      self.path.to_string_lossy().replace('\\', "/")
    }
  }
  pub fn matches(&self, input: &str) -> bool {
    self.id() == input || self.name == input || self.manifests.iter().any(|m| m.name() == input)
  }
  pub fn tag(&self, config: &Config, version: &semver::Version) -> String {
    self
      .tag_format(config)
      .replace("{package_name}", &self.name)
      .replace("{version}", &version.to_string())
  }
  pub fn tag_format<'a>(&'a self, config: &'a Config) -> &'a str {
    self
      .config
      .release_tag_format
      .as_deref()
      .unwrap_or(&config.git.release_tag_format)
  }
}

pub fn patterns(patterns: &[String]) -> Result<GlobSet> {
  let mut builder = GlobSetBuilder::new();
  for p in patterns {
    builder.add(
      GlobBuilder::new(p.strip_prefix("./").unwrap_or(p))
        .literal_separator(true)
        .build()
        .with_context(|| format!("invalid glob {p}"))?,
    );
  }
  Ok(builder.build()?)
}

pub fn files(root: &Path) -> Result<Vec<PathBuf>> {
  if !root.exists() {
    return Ok(vec![]);
  }
  let mut found = Vec::new();
  for entry in WalkDir::new(root)
    .follow_links(false)
    .into_iter()
    .filter_entry(|e| {
      e.depth() == 0
        || !matches!(
          e.file_name().to_str(),
          Some(".git" | "node_modules" | "target" | "dist" | ".prp")
        )
    })
  {
    let entry = entry?;
    if entry.file_type().is_file() {
      found.push(entry.path().strip_prefix(root)?.to_owned());
    }
  }
  found.sort();
  Ok(found)
}

pub fn load(root: &Path, config: &Config) -> Result<Vec<Package>> {
  let all = files(root)?;
  let manifests: Vec<_> = all
    .into_iter()
    .filter(|p| {
      p.file_name()
        .and_then(|s| s.to_str())
        .is_some_and(|n| MANIFESTS.contains(&n))
    })
    .collect();
  let mut include = config.packages.include.clone();
  let mut exclude = config.packages.exclude.clone();
  if include.is_empty() {
    discover_workspaces(root, &mut include, &mut exclude)?;
  }
  if include.is_empty() {
    include = vec!["packages/*".into(), "crates/*".into(), ".".into()];
  }
  for pattern in &include {
    relative_path(pattern)?;
  }
  let includes = patterns(&include)?;
  let excludes = patterns(&exclude)?;
  let mut roots = BTreeSet::new();
  for manifest in &manifests {
    let dir = manifest.parent().unwrap_or(Path::new(""));
    let id = if dir.as_os_str().is_empty() {
      Path::new(".")
    } else {
      dir
    };
    if (includes.is_match(id)
      || config
        .packages
        .overrides
        .contains_key(&id.to_string_lossy().to_string()))
      && !excludes.is_match(id)
    {
      roots.insert(dir.to_owned());
    }
  }
  for explicit in config.packages.overrides.keys() {
    let p = if explicit == "." {
      PathBuf::new()
    } else {
      PathBuf::from(explicit)
    };
    ensure!(
      excludes.is_match(explicit) || roots.contains(&p),
      "configured package '{explicit}' has no supported manifest"
    );
  }
  let read = |path: &Path| -> Result<Option<String>> {
    match fs::read_to_string(root.join(path)) {
      Ok(s) => Ok(Some(s)),
      Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
      Err(e) => Err(e.into()),
    }
  };
  let mut packages = Vec::new();
  let mut owners = BTreeMap::new();
  for dir in &roots {
    let id = if dir.as_os_str().is_empty() {
      ".".into()
    } else {
      dir.to_string_lossy().to_string()
    };
    let settings = config
      .packages
      .overrides
      .get(&id)
      .cloned()
      .unwrap_or_default();
    let explicit_patterns = settings
      .manifests
      .as_ref()
      .map(|p| patterns(p))
      .transpose()?;
    let mut owned = Vec::new();
    for file in &manifests {
      if file.ancestors().skip(1).any(|p| excludes.is_match(p)) {
        continue;
      }
      let Ok(relative) = file.strip_prefix(dir) else {
        continue;
      };
      let nearest = roots
        .iter()
        .filter(|p| file.starts_with(p))
        .max_by_key(|p| p.components().count());
      if nearest != Some(dir) {
        continue;
      }
      if explicit_patterns
        .as_ref()
        .is_some_and(|p| !p.is_match(relative))
      {
        continue;
      }
      if let Some(manifest) = Manifests::read(file, &read)? {
        if let Some(previous) = owners.insert(
          (manifest.registry(), manifest.name().to_owned()),
          file.clone(),
        ) {
          anyhow::bail!(
            "duplicate {} package {} in {} and {}",
            manifest.registry().label(),
            manifest.name(),
            previous.display(),
            file.display()
          );
        }
        owned.push(manifest);
      }
    }
    if owned.is_empty() {
      ensure!(
        !config.packages.overrides.contains_key(&id),
        "configured package '{id}' has no versioned manifests"
      );
      continue;
    }
    owned.sort_by_key(|m| (m.path().components().count(), m.path().to_owned()));
    let name = if owned.len() == 1 {
      owned[0].name().to_owned()
    } else {
      dir
        .file_name()
        .or_else(|| root.file_name())
        .context("cannot determine package name")?
        .to_string_lossy()
        .to_string()
    };
    let display_name = settings
      .display_name
      .clone()
      .unwrap_or_else(|| name.clone());
    packages.push(Package {
      path: dir.clone(),
      name,
      display_name,
      manifests: owned,
      config: settings,
    });
  }
  let mut names = BTreeSet::new();
  for pkg in &packages {
    ensure!(
      names.insert(&pkg.name),
      "duplicate logical package name {}; use distinct directory names",
      pkg.name
    );
  }
  ensure!(
    !packages.is_empty(),
    "no versioned packages found; configure [packages].include"
  );
  Ok(packages)
}

fn discover_workspaces(
  root: &Path,
  include: &mut Vec<String>,
  exclude: &mut Vec<String>,
) -> Result<()> {
  if root.join("package.json").exists() {
    let doc = json(&fs::read_to_string(root.join("package.json"))?, false)?;
    let values = doc["workspaces"]
      .as_array()
      .or_else(|| doc["workspaces"]["packages"].as_array());
    if let Some(values) = values {
      include.extend(values.iter().filter_map(|v| v.as_str().map(str::to_owned)));
    }
  }
  if root.join("pnpm-workspace.yaml").exists() {
    let doc: serde_yaml_ng::Value =
      serde_yaml_ng::from_str(&fs::read_to_string(root.join("pnpm-workspace.yaml"))?)?;
    if let Some(values) = doc["packages"].as_sequence() {
      for value in values {
        if let Some(pattern) = value.as_str() {
          if let Some(p) = pattern.strip_prefix('!') {
            exclude.push(p.into());
          } else {
            include.push(pattern.into());
          }
        }
      }
    }
  }
  if root.join("Cargo.toml").exists() {
    let doc = json(&fs::read_to_string(root.join("Cargo.toml"))?, true)?;
    if let Some(members) = doc["workspace"]["members"].as_array() {
      include.extend(members.iter().filter_map(|v| v.as_str().map(str::to_owned)));
    }
    if let Some(excluded) = doc["workspace"]["exclude"].as_array() {
      exclude.extend(
        excluded
          .iter()
          .filter_map(|v| v.as_str().map(str::to_owned)),
      );
    }
    if doc["package"]["version"].is_string() {
      include.push(".".into());
    }
  }
  if root.join("deno.json").exists() {
    let doc = json(&fs::read_to_string(root.join("deno.json"))?, false)?;
    if let Some(members) = doc["workspace"].as_array() {
      include.extend(
        members
          .iter()
          .filter_map(|v| v.as_str().map(|s| s.trim_start_matches("./").to_owned())),
      );
    }
  }
  Ok(())
}
