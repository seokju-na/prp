use crate::config::{HttpConfig, NpmRegistryConfig};
use crate::dependency::{self, Dependencies, Dependency, DependencyVersion};
use crate::edit::Edit;
use crate::http::HttpClient;
use crate::manifest::{ManifestKind, ReleasedVersions};
use crate::registry::{Registry, RegistryPublishResult};
use crate::{fs_util, registry, serde_util};
use anyhow::{Context, Result, anyhow};
use reqwest::header::ACCEPT;
use reqwest::{Method, Request, StatusCode, Url};
use semver::Version;
use serde_json::Value;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use tokio::process::Command;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum NpmPackageManager {
  Npm,
  YarnClassic,
  Yarn,
  Pnpm,
}

impl NpmPackageManager {
  pub fn publish_command(&self) -> String {
    match self {
      Self::Npm => "npm publish",
      Self::YarnClassic => "yarn publish --non-interactive",
      Self::Yarn => "yarn npm publish",
      Self::Pnpm => "pnpm publish --no-git-checks",
    }
    .to_string()
  }
}

static PACKAGE_MANAGERS: LazyLock<Mutex<HashMap<PathBuf, Option<NpmPackageManager>>>> =
  LazyLock::new(Default::default);

pub fn find_npm_package_manager(cwd: &Path) -> Result<Option<NpmPackageManager>> {
  let mut visited = Vec::new();
  let mut found = None;
  for dir in cwd.ancestors() {
    if let Some(cached) = PACKAGE_MANAGERS.lock().unwrap().get(dir).copied() {
      found = cached;
      break;
    }
    visited.push(dir.to_path_buf());
    if let Some(manager) = detect_package_manager(dir)? {
      found = Some(manager);
      break;
    }
  }
  let mut cache = PACKAGE_MANAGERS.lock().unwrap();
  for dir in visited {
    cache.insert(dir, found);
  }
  Ok(found)
}

fn detect_package_manager(dir: &Path) -> Result<Option<NpmPackageManager>> {
  let package_json = dir.join("package.json");
  if let Some(raw) = fs_util::read_to_string_optional(&package_json)? {
    let data: serde_json::Value = serde_json::from_str(&raw)
      .with_context(|| format!("Failed to parse {}", package_json.display()))?;
    if let Some(field) = data.get("packageManager") {
      let manager = field
        .as_str()
        .context("Field \"packageManager\" is not a string")
        .and_then(parse_package_manager_field)
        .with_context(|| format!("Invalid {}", package_json.display()))?;
      return Ok(Some(manager));
    }
  }

  let mut managers = Vec::new();
  for (file, manager) in [
    ("package-lock.json", NpmPackageManager::Npm),
    ("npm-shrinkwrap.json", NpmPackageManager::Npm),
    ("pnpm-lock.yaml", NpmPackageManager::Pnpm),
  ] {
    let path = dir.join(file);
    if std::fs::exists(&path).with_context(|| format!("Failed to read {}", path.display()))? {
      managers.push(manager);
    }
  }
  if let Some(raw) = fs_util::read_to_string_optional(&dir.join("yarn.lock"))? {
    // Yarn 1 writes this header; Yarn 2+ writes a YAML lock file with __metadata.
    managers.push(if raw.contains("# yarn lockfile v1") {
      NpmPackageManager::YarnClassic
    } else {
      NpmPackageManager::Yarn
    });
  }
  managers.dedup();
  match managers[..] {
    [] => Ok(None),
    [manager] => Ok(Some(manager)),
    _ => anyhow::bail!("Multiple lock files in {}", dir.display()),
  }
}

fn parse_package_manager_field(field: &str) -> Result<NpmPackageManager> {
  let (name, version) = field
    .split_once('@')
    .with_context(|| format!("Field \"packageManager\" must be <name>@<version>: {field}"))?;
  let version = Version::parse(version)
    .with_context(|| format!("Field \"packageManager\" has an invalid version: {field}"))?;
  match name {
    "npm" => Ok(NpmPackageManager::Npm),
    "pnpm" => Ok(NpmPackageManager::Pnpm),
    "yarn" if version.major < 2 => Ok(NpmPackageManager::YarnClassic),
    "yarn" => Ok(NpmPackageManager::Yarn),
    _ => anyhow::bail!("Unsupported package manager: {name}"),
  }
}

/// Dependency tables whose versions follow a release.
const DEPENDENCY_TABLES: [&str; 4] = [
  "dependencies",
  "devDependencies",
  "peerDependencies",
  "optionalDependencies",
];

pub fn get_npm_dependencies(pkg_json: &Value) -> Dependencies {
  DEPENDENCY_TABLES
    .iter()
    .filter_map(|table| serde_util::parse_map(pkg_json, table).ok().flatten())
    .flatten()
    .map(|(name, spec)| Dependency {
      kind: ManifestKind::PackageJson,
      version: DependencyVersion::from_npm(&spec),
      name,
    })
    .collect()
}

/// The edits pointing the dependencies of a parsed `package.json` at `versions`.
pub fn dependency_edits(pkg_json: &Value, versions: &ReleasedVersions) -> Vec<Edit> {
  let mut edits = vec![];
  for table in DEPENDENCY_TABLES {
    let Some(entries) = pkg_json.get(table).and_then(Value::as_object) else {
      continue;
    };
    for (name, current) in entries {
      if let Some(next) = versions.get(&(ManifestKind::PackageJson, name.clone()))
        && let Some(requirement) = current
          .as_str()
          .and_then(|current| dependency::next_requirement(current, next))
      {
        edits.push((vec![table.to_string(), name.clone()], requirement));
      }
    }
  }
  edits
}

/// The `workspaces` of `package.json` in `root`, `!` marking an exclusion.
pub fn workspace_patterns(root: &Path) -> Result<Option<Vec<String>>> {
  let path = root.join("package.json");
  let Some(raw) = fs_util::read_to_string_optional(&path)? else {
    return Ok(None);
  };
  let data: Value =
    serde_json::from_str(&raw).with_context(|| format!("Failed to parse {}", path.display()))?;
  // Yarn 1 also takes `{ "packages": [...] }`.
  let workspaces = match data.get("workspaces") {
    Some(Value::Object(object)) => object.get("packages"),
    workspaces => workspaces,
  };
  let Some(workspaces) = workspaces else {
    return Ok(None);
  };
  serde_util::string_list(workspaces)
    .map(Some)
    .with_context(|| format!("Field \"workspaces\" of {} must list globs", path.display()))
}

/// The `packages` of `pnpm-workspace.yaml` in `root`, `!` marking an exclusion.
pub fn pnpm_workspace_patterns(root: &Path) -> Result<Option<Vec<String>>> {
  let path = root.join("pnpm-workspace.yaml");
  let Some(raw) = fs_util::read_to_string_optional(&path)? else {
    return Ok(None);
  };
  let data: Value =
    serde_yaml_ng::from_str(&raw).with_context(|| format!("Failed to parse {}", path.display()))?;
  let Some(packages) = data.get("packages") else {
    return Ok(None);
  };
  serde_util::string_list(packages)
    .map(Some)
    .with_context(|| format!("Field \"packages\" of {} must list globs", path.display()))
}

const NPM_REGISTRY_URL: &str = "https://registry.npmjs.org";

#[derive(Debug, Clone)]
pub struct NpmRegistry {
  config: NpmRegistryConfig,
  http: HttpClient,
}

impl NpmRegistry {
  pub fn new(config: &NpmRegistryConfig, http_config: &HttpConfig) -> Result<Self> {
    let http = HttpClient::new(http_config)?;
    Ok(Self {
      config: config.clone(),
      http,
    })
  }
}

impl NpmRegistry {
  /// `segments` under the registry url. A scoped name stays one segment, as
  /// `@scope%2Fname`.
  fn package_url(&self, segments: &[&str]) -> Result<Url> {
    let registry_url = self.config.registry.as_deref().unwrap_or(NPM_REGISTRY_URL);
    let mut url = Url::parse(registry_url)
      .with_context(|| format!("Invalid npm registry url: {registry_url}"))?;
    url
      .path_segments_mut()
      .map_err(|_| anyhow!("Invalid npm registry url: {registry_url}"))?
      .pop_if_empty()
      .extend(segments);
    Ok(url)
  }
}

impl Registry for NpmRegistry {
  fn name(&self) -> &'static str {
    "npm"
  }

  fn url(&self, name: &str, version: &Version) -> String {
    format!("https://www.npmjs.com/package/{name}/v/{version}")
  }

  async fn exists(&self, name: &str, version: &Version) -> Result<bool> {
    let url = self.package_url(&[name, &version.to_string()])?;
    let request = Request::new(Method::GET, url);
    let response = self
      .http
      .send(request)
      .await
      .with_context(|| format!("Failed to look up npm package {name}@{version}"))?;

    if response.status() == StatusCode::NOT_FOUND {
      return Ok(false);
    }

    response
      .error_for_status()
      .with_context(|| format!("Failed to look up npm package {name}@{version}"))?;

    Ok(true)
  }

  async fn latest(&self, name: &str) -> Result<Option<Version>> {
    let url = self.package_url(&[name])?;
    let response = self
      .http
      .request(Method::GET, url)
      // The abbreviated document still has the dist-tags, without every readme.
      .header(ACCEPT, "application/vnd.npm.install-v1+json")
      .send()
      .await
      .with_context(|| format!("Failed to look up npm package {name}"))?;
    if response.status() == StatusCode::NOT_FOUND {
      return Ok(None);
    }
    let document: Value = response
      .error_for_status()
      .with_context(|| format!("Failed to look up npm package {name}"))?
      .json()
      .await
      .with_context(|| format!("Invalid registry response for npm package {name}"))?;
    let latest = document
      .pointer("/dist-tags/latest")
      .and_then(Value::as_str)
      .with_context(|| format!("npm package {name} has no \"latest\" dist-tag"))?;
    let latest = Version::parse(latest)
      .with_context(|| format!("npm package {name} has an invalid latest version: {latest}"))?;
    Ok(Some(latest))
  }

  async fn publish<I, K, V>(
    &self,
    manifest_dir: &Path,
    args: &[String],
    envs: I,
  ) -> Result<RegistryPublishResult>
  where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<OsStr>,
    V: AsRef<OsStr>,
  {
    let manager = find_npm_package_manager(manifest_dir)?.unwrap_or(NpmPackageManager::Npm);
    let publish_command = match &self.config.publish_command {
      Some(command) => command.to_owned(),
      None => manager.publish_command(),
    };
    let words = shell_words::split(&publish_command)
      .with_context(|| format!("Invalid publish command: {publish_command}"))?;
    let (program, command_args) = words
      .split_first()
      .with_context(|| format!("Invalid publish command: {publish_command}"))?;

    let mut command = Command::new(program);
    command
      .args(command_args)
      .args(args)
      .envs(envs)
      .current_dir(manifest_dir);
    if let Some(url) = &self.config.publish_registry {
      match manager {
        // Yarn 2+ has no `--registry` flag.
        NpmPackageManager::Yarn => {
          command.env("YARN_NPM_PUBLISH_REGISTRY", url);
        }
        _ => {
          command.args(["--registry", url]);
        }
      }
    }

    registry::run_publish_command(command, |output| {
      output.contains("cannot publish over") || output.contains("epublishconflict")
    })
    .await
  }
}

/// The dist-tag a version must go out under: a prerelease published without one
/// would become `latest`.
pub fn dist_tag(version: &Version) -> Option<&str> {
  if version.pre.is_empty() {
    return None;
  }
  let id = version.pre.as_str().split('.').next().unwrap_or("next");
  match id.parse::<u64>() {
    Ok(_) => Some("next"),
    Err(_) => Some(id),
  }
}
