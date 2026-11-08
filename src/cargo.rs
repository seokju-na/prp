use crate::config::{CargoRegistryConfig, HttpConfig};
use crate::dependency::{self, Dependencies, Dependency, DependencyVersion};
use crate::edit::Edit;
use crate::http::HttpClient;
use crate::manifest::{ManifestKind, ReleasedVersions};
use crate::registry::{self, Registry, RegistryPublishResult};
use anyhow::{Context, Result, ensure};
use reqwest::{Method, Request, StatusCode, Url};
use semver::Version;
use serde::Deserialize;
use serde_json::Value;
use std::ffi::OsStr;
use std::path::Path;
use tokio::process::Command;

const CRATES_IO_INDEX_URL: &str = "https://index.crates.io";

pub fn read_cargo_toml(path: &Path) -> Result<Value> {
  let raw = std::fs::read(path).with_context(|| format!("Failed to read {}", path.display()))?;
  let data = parse_cargo_toml(&raw)?;

  Ok(data)
}

pub fn parse_cargo_toml(raw: &[u8]) -> Result<Value> {
  let toml = toml::from_slice::<toml::Table>(raw).context("Failed to parse TOML")?;
  let data = serde_json::to_value(toml).context("Failed to serialize TOML")?;

  Ok(data)
}

pub fn get_cargo_dependencies(cargo_toml: &Value) -> Dependencies {
  ["dependencies", "dev-dependencies", "build-dependencies"]
    .iter()
    .filter_map(|table| cargo_toml.get(table).and_then(Value::as_object))
    .flatten()
    .map(|(key, entry)| Dependency {
      kind: ManifestKind::CargoToml,
      // `alias = { package = "real-name" }` depends on `real-name`.
      name: entry
        .get("package")
        .and_then(Value::as_str)
        .unwrap_or(key)
        .to_string(),
      version: DependencyVersion::from_cargo(entry),
    })
    .collect()
}

/// Dependency tables whose versions follow a release.
const DEPENDENCY_TABLES: [&[&str]; 4] = [
  &["dependencies"],
  &["dev-dependencies"],
  &["build-dependencies"],
  &["workspace", "dependencies"],
];

/// The edits pointing the dependencies of a parsed `Cargo.toml` at `versions`.
/// Entries without a version of their own (`{ workspace = true }`) are skipped.
pub fn dependency_edits(cargo: &Value, versions: &ReleasedVersions) -> Vec<Edit> {
  let mut edits = vec![];
  for table_path in DEPENDENCY_TABLES {
    let table = table_path
      .iter()
      .try_fold(cargo, |value, key| value.get(key));
    let Some(table) = table.and_then(Value::as_object) else {
      continue;
    };
    for (key, entry) in table {
      let name = entry.get("package").and_then(Value::as_str).unwrap_or(key);
      let Some(next) = versions.get(&(ManifestKind::CargoToml, name.to_string())) else {
        continue;
      };
      let mut path = table_path.iter().map(|x| x.to_string()).collect::<Vec<_>>();
      path.push(key.clone());
      let current = match entry {
        Value::String(current) => current.as_str(),
        _ => {
          path.push("version".into());
          match entry.get("version").and_then(Value::as_str) {
            Some(current) => current,
            None => continue,
          }
        }
      };
      // A prerelease is pinned exactly, so a dependent never resolves across channels.
      let requirement = match next.pre.is_empty() {
        true => dependency::next_requirement(current, next),
        false => Some(format!("={next}")),
      };
      if let Some(requirement) = requirement {
        edits.push((path, requirement));
      }
    }
  }
  edits
}

/// The `[workspace]` members of `Cargo.toml` in `root`, its `exclude` entries
/// marked with `!`.
pub fn workspace_patterns(root: &Path) -> Result<Option<Vec<String>>> {
  let path = root.join("Cargo.toml");
  if !std::fs::exists(&path).with_context(|| format!("Failed to read {}", path.display()))? {
    return Ok(None);
  }
  let cargo = read_cargo_toml(&path)?;
  let Some(workspace) = cargo.get("workspace") else {
    return Ok(None);
  };
  let list = |field: &str| -> Result<Vec<String>> {
    let Some(values) = workspace.get(field) else {
      return Ok(vec![]);
    };
    values
      .as_array()
      .and_then(|values| {
        values
          .iter()
          .map(|value| value.as_str().map(str::to_string))
          .collect()
      })
      .with_context(|| {
        format!(
          "Field \"workspace.{field}\" of {} must list paths",
          path.display()
        )
      })
  };
  let mut patterns = list("members")?;
  patterns.extend(list("exclude")?.into_iter().map(|path| format!("!{path}")));
  Ok(Some(patterns))
}

/// The real crate name behind an `alias.workspace = true` dependency.
pub fn workspace_dependency_name<'a>(root_cargo: Option<&'a Value>, alias: &'a str) -> &'a str {
  root_cargo
    .and_then(|cargo| {
      cargo
        .get("workspace")?
        .get("dependencies")?
        .get(alias)?
        .get("package")
    })
    .and_then(Value::as_str)
    .unwrap_or(alias)
}

#[derive(Debug, Clone)]
pub struct CargoRegistry {
  config: CargoRegistryConfig,
  http: HttpClient,
}

impl CargoRegistry {
  pub fn new(config: &CargoRegistryConfig, http_config: &HttpConfig) -> Result<Self> {
    Ok(Self {
      config: config.clone(),
      http: HttpClient::new(http_config)?,
    })
  }
}

impl CargoRegistry {
  /// Every version in the index, none for a crate it does not have.
  async fn index_entries(&self, name: &str) -> Result<Vec<IndexEntry>> {
    let index_url = self.config.index.as_deref().unwrap_or(CRATES_IO_INDEX_URL);
    let url = format!(
      "{}/{}",
      index_url.trim_end_matches('/'),
      crate_index_path(name)?
    );
    let url = Url::parse(&url).with_context(|| format!("Invalid crate index url: {url}"))?;
    let response = self.http.send(Request::new(Method::GET, url)).await?;
    if response.status() == StatusCode::NOT_FOUND {
      return Ok(vec![]);
    }
    let index = response
      .error_for_status()?
      .text()
      .await
      .with_context(|| format!("Failed to read crate index of {name}"))?;
    index
      .lines()
      .filter(|line| !line.trim().is_empty())
      .map(|line| {
        let entry: RawIndexEntry = serde_json::from_str(line)
          .with_context(|| format!("Invalid crate index entry of {name}"))?;
        let version = Version::parse(&entry.vers)
          .with_context(|| format!("Invalid version of crate {name}: {}", entry.vers))?;
        Ok(IndexEntry {
          version,
          yanked: entry.yanked,
        })
      })
      .collect()
  }
}

#[derive(Deserialize)]
struct RawIndexEntry {
  vers: String,
  #[serde(default)]
  yanked: bool,
}

struct IndexEntry {
  version: Version,
  yanked: bool,
}

impl Registry for CargoRegistry {
  fn name(&self) -> &'static str {
    match self.config.index {
      None => "crates.io",
      Some(_) => "cargo",
    }
  }

  fn url(&self, name: &str, version: &Version) -> String {
    format!("https://crates.io/crates/{name}/{version}")
  }

  async fn exists(&self, name: &str, version: &Version) -> Result<bool> {
    let entries = self
      .index_entries(name)
      .await
      .with_context(|| format!("Failed to look up crate {name}@{version}"))?;
    // Yanked versions are listed too, and they still occupy their number.
    Ok(entries.iter().any(|entry| &entry.version == version))
  }

  async fn latest(&self, name: &str) -> Result<Option<Version>> {
    let entries = self
      .index_entries(name)
      .await
      .with_context(|| format!("Failed to look up crate {name}"))?;
    // What `cargo add` picks: the highest release, or a prerelease if that is
    // all there is. Yanked versions are not handed out.
    let available = entries.into_iter().filter(|entry| !entry.yanked);
    let (releases, prereleases): (Vec<_>, Vec<_>) =
      available.partition(|entry| entry.version.pre.is_empty());
    let highest = |entries: Vec<IndexEntry>| {
      entries
        .into_iter()
        .map(|entry| entry.version)
        .max_by(|a, b| a.cmp_precedence(b))
    };
    Ok(highest(releases).or_else(|| highest(prereleases)))
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
    let mut command = Command::new("cargo");
    // Scripts running before publish leave build output behind.
    command
      .args(["publish", "--allow-dirty"])
      .args(args)
      .envs(envs)
      .current_dir(manifest_dir);
    registry::run_publish_command(command, |output| {
      output.contains("is already uploaded") || output.contains("already exists on")
    })
    .await
  }
}

fn crate_index_path(name: &str) -> Result<String> {
  ensure!(
    !name.is_empty()
      && name
        .bytes()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_')),
    "Invalid crate name: {name}"
  );
  let name = name.to_ascii_lowercase();
  let path = match name.len() {
    1 => format!("1/{name}"),
    2 => format!("2/{name}"),
    3 => format!("3/{}/{name}", &name[..1]),
    _ => format!("{}/{}/{name}", &name[..2], &name[2..4]),
  };
  Ok(path)
}
