use crate::config::{HttpConfig, JsrRegistryConfig};
use crate::dependency::{self, Dependencies, Dependency, DependencyVersion};
use crate::edit::Edit;
use crate::http::HttpClient;
use crate::manifest::{ManifestKind, ReleasedVersions};
use crate::registry::{self, Registry, RegistryPublishResult};
use crate::{fs_util, serde_util};
use anyhow::{Context, Result, anyhow};
use reqwest::{Method, Request, StatusCode, Url};
use semver::Version;
use serde_json::Value;
use std::ffi::OsStr;
use std::ops::Range;
use std::path::Path;
use tokio::process::Command;

pub const FILE_NAMES: [&str; 2] = ["deno.json", "deno.jsonc"];

pub fn parse_deno_json(raw: &str) -> Result<Value> {
  jsonc_parser::parse_to_serde_value(raw, &Default::default()).context("Failed to parse JSON")
}

/// A registry package named by an `imports` value, such as `jsr:@std/path@^1/posix`.
struct Specifier<'a> {
  kind: ManifestKind,
  name: &'a str,
  /// Where the version requirement is in the value.
  requirement: Option<Range<usize>>,
}

fn parse_specifier(value: &str) -> Option<Specifier<'_>> {
  let (kind, rest) = match value.split_once(':')? {
    ("jsr", rest) => (ManifestKind::DenoJson, rest),
    ("npm", rest) => (ManifestKind::PackageJson, rest),
    _ => return None,
  };
  let rest = rest.strip_prefix('/').unwrap_or(rest);
  let offset = value.len() - rest.len();
  let scope_end = match rest.starts_with('@') {
    true => rest.find('/')? + 1,
    false => 0,
  };
  let name_end = rest[scope_end..]
    .find(['@', '/'])
    .map_or(rest.len(), |end| scope_end + end);
  let requirement = rest[name_end..].strip_prefix('@').map(|after| {
    let start = offset + name_end + 1;
    start..start + after.find('/').unwrap_or(after.len())
  });
  Some(Specifier {
    kind,
    name: &rest[..name_end],
    requirement,
  })
}

fn imports(deno_json: &Value) -> impl Iterator<Item = (&String, &str)> {
  deno_json
    .get("imports")
    .and_then(Value::as_object)
    .into_iter()
    .flatten()
    .filter_map(|(alias, value)| Some((alias, value.as_str()?)))
}

pub fn get_deno_dependencies(deno_json: &Value) -> Dependencies {
  imports(deno_json)
    .filter_map(|(_, value)| {
      let specifier = parse_specifier(value)?;
      let version = specifier
        .requirement
        .map_or(DependencyVersion::Other, |range| {
          DependencyVersion::from_npm(&value[range])
        });
      Some(Dependency {
        kind: specifier.kind,
        name: specifier.name.to_string(),
        version,
      })
    })
    .collect()
}

/// The edits pointing the `imports` of a parsed `deno.json` at `versions`.
pub fn dependency_edits(deno_json: &Value, versions: &ReleasedVersions) -> Vec<Edit> {
  imports(deno_json)
    .filter_map(|(alias, value)| {
      let specifier = parse_specifier(value)?;
      let next = versions.get(&(specifier.kind, specifier.name.to_string()))?;
      let range = specifier.requirement?;
      let requirement = dependency::next_requirement(&value[range.clone()], next)?;
      let updated = format!(
        "{}{requirement}{}",
        &value[..range.start],
        &value[range.end..]
      );
      Some((vec!["imports".to_string(), alias.clone()], updated))
    })
    .collect()
}

/// The `workspace` members of `deno.json` in `root`.
pub fn workspace_patterns(root: &Path) -> Result<Option<Vec<String>>> {
  for file_name in FILE_NAMES {
    let path = root.join(file_name);
    let Some(raw) = fs_util::read_to_string_optional(&path)? else {
      continue;
    };
    let data = parse_deno_json(&raw).with_context(|| format!("Invalid {}", path.display()))?;
    let Some(workspace) = data.get("workspace") else {
      return Ok(None);
    };
    return serde_util::string_list(workspace)
      .map(Some)
      .with_context(|| format!("Field \"workspace\" of {} must list paths", path.display()));
  }
  Ok(None)
}

const JSR_API_URL: &str = "https://api.jsr.io";
const PUBLISH_COMMAND: &str = "deno publish --allow-dirty";

#[derive(Debug, Clone)]
pub struct JsrRegistry {
  config: JsrRegistryConfig,
  http: HttpClient,
}

impl JsrRegistry {
  pub fn new(config: &JsrRegistryConfig, http_config: &HttpConfig) -> Result<Self> {
    Ok(Self {
      config: config.clone(),
      http: HttpClient::new(http_config)?,
    })
  }

  /// `/scopes/<scope>/packages/<name>`, then `segments`.
  fn package_url(&self, name: &str, segments: &[&str]) -> Result<Url> {
    let (scope, package) = name
      .strip_prefix('@')
      .and_then(|name| name.split_once('/'))
      .with_context(|| format!("JSR package names are @scope/name: {name}"))?;
    let api_url = self.config.api_url.as_deref().unwrap_or(JSR_API_URL);
    let mut url = Url::parse(api_url).with_context(|| format!("Invalid JSR API url: {api_url}"))?;
    url
      .path_segments_mut()
      .map_err(|_| anyhow!("Invalid JSR API url: {api_url}"))?
      .pop_if_empty()
      .extend(["scopes", scope, "packages", package])
      .extend(segments);
    Ok(url)
  }
}

impl Registry for JsrRegistry {
  fn name(&self) -> &'static str {
    "jsr"
  }

  fn url(&self, name: &str, version: &Version) -> String {
    format!("https://jsr.io/{name}@{version}")
  }

  async fn exists(&self, name: &str, version: &Version) -> Result<bool> {
    let url = self.package_url(name, &["versions", &version.to_string()])?;
    let response = self
      .http
      .send(Request::new(Method::GET, url))
      .await
      .with_context(|| format!("Failed to look up JSR package {name}@{version}"))?;
    if response.status() == StatusCode::NOT_FOUND {
      return Ok(false);
    }
    // Yanked versions are found too, and they still occupy their number.
    response
      .error_for_status()
      .with_context(|| format!("Failed to look up JSR package {name}@{version}"))?;
    Ok(true)
  }

  async fn latest(&self, name: &str) -> Result<Option<Version>> {
    let url = self.package_url(name, &[])?;
    let response = self
      .http
      .send(Request::new(Method::GET, url))
      .await
      .with_context(|| format!("Failed to look up JSR package {name}"))?;
    if response.status() == StatusCode::NOT_FOUND {
      return Ok(None);
    }
    let document: Value = response
      .error_for_status()
      .with_context(|| format!("Failed to look up JSR package {name}"))?
      .json()
      .await
      .with_context(|| format!("Invalid registry response for JSR package {name}"))?;
    let Some(latest) = document.get("latestVersion").and_then(Value::as_str) else {
      return Ok(None);
    };
    let latest = Version::parse(latest)
      .with_context(|| format!("JSR package {name} has an invalid latest version: {latest}"))?;
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
    let publish_command = self
      .config
      .publish_command
      .as_deref()
      .unwrap_or(PUBLISH_COMMAND);
    let words = shell_words::split(publish_command)
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
    registry::run_publish_command(command, |output| {
      output.contains("already published") || output.contains("already exists")
    })
    .await
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use serde_json::json;

  #[test]
  fn reads_registry_packages_from_imports() {
    let deno_json = json!({
      "imports": {
        "@std/path": "jsr:@std/path@^1.0.0/posix",
        "@wvb/node": "npm:@wvb/node@0.3.1",
        "chalk": "npm:chalk",
        "remote": "https://deno.land/x/remote/mod.ts",
      }
    });
    let found = get_deno_dependencies(&deno_json)
      .into_iter()
      .map(|dependency| (dependency.kind, dependency.name))
      .collect::<Vec<_>>();
    assert_eq!(
      found,
      [
        (ManifestKind::DenoJson, "@std/path".to_string()),
        (ManifestKind::PackageJson, "@wvb/node".to_string()),
        (ManifestKind::PackageJson, "chalk".to_string()),
      ]
    );
  }

  #[test]
  fn edits_keep_the_operator_and_subpath() {
    let deno_json = json!({
      "imports": {
        "@std/path": "jsr:/@std/path@^1.0.0/posix",
        "@wvb/node": "npm:@wvb/node@0.3.1",
        "chalk": "npm:chalk",
      }
    });
    let versions = ReleasedVersions::from([
      (
        (ManifestKind::DenoJson, "@std/path".into()),
        Version::new(1, 2, 0),
      ),
      (
        (ManifestKind::PackageJson, "@wvb/node".into()),
        Version::new(0, 3, 2),
      ),
      (
        (ManifestKind::PackageJson, "chalk".into()),
        Version::new(5, 0, 0),
      ),
    ]);
    let mut edits = dependency_edits(&deno_json, &versions);
    edits.sort();
    assert_eq!(
      edits,
      [
        (
          vec!["imports".to_string(), "@std/path".to_string()],
          "jsr:/@std/path@^1.2.0/posix".to_string()
        ),
        (
          vec!["imports".to_string(), "@wvb/node".to_string()],
          "npm:@wvb/node@0.3.2".to_string()
        ),
      ]
    );
  }
}
