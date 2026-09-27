use crate::{
  config::{PackagesConfig, RegistryConfig},
  http_util::{self, HttpClient},
};
use anyhow::{Context, Result, ensure};
use enum_dispatch::enum_dispatch;
use reqwest::StatusCode;
use semver::Version;
use std::{io::Write, path::Path, process::Command};

#[enum_dispatch]
pub trait Registry {
  fn label(&self) -> &'static str;
  fn url(&self, name: &str, version: &Version) -> String;
  fn exists(
    &self,
    http: &HttpClient,
    endpoints: &RegistryConfig,
    name: &str,
    version: &Version,
  ) -> Result<bool>;
  fn command(&self, config: &PackagesConfig, version: &Version) -> Result<Vec<String>>;
  fn is_duplicate_rejection(&self, output: &str) -> bool;
}

#[enum_dispatch(Registry)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Registries {
  Npm(NpmRegistry),
  Cargo(CargoRegistry),
  Jsr(JsrRegistry),
}
impl Registries {
  pub fn npm() -> Self {
    NpmRegistry.into()
  }
  pub fn cargo() -> Self {
    CargoRegistry.into()
  }
  pub fn jsr() -> Self {
    JsrRegistry.into()
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct NpmRegistry;
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct CargoRegistry;
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct JsrRegistry;

impl Registry for NpmRegistry {
  fn label(&self) -> &'static str {
    "npm"
  }
  fn url(&self, name: &str, version: &Version) -> String {
    format!("https://www.npmjs.com/package/{name}/v/{version}")
  }
  fn exists(
    &self,
    http: &HttpClient,
    endpoints: &RegistryConfig,
    name: &str,
    version: &Version,
  ) -> Result<bool> {
    let response = http.get(http_util::url(
      &endpoints.npm_url,
      &[name, &version.to_string()],
    )?)?;
    if response.status == StatusCode::NOT_FOUND {
      return Ok(false);
    }
    response.ensure_success()?;
    Ok(true)
  }
  fn command(&self, config: &PackagesConfig, version: &Version) -> Result<Vec<String>> {
    let mut words = command(&config.npm_publish_command, &config.npm_publish_args)?;
    if !version.pre.is_empty()
      && !words
        .iter()
        .any(|w| w == "--tag" || w.starts_with("--tag="))
    {
      let id = version.pre.as_str().split('.').next().unwrap_or("next");
      words.push(format!(
        "--tag={}",
        if id.parse::<u64>().is_ok() {
          "next"
        } else {
          id
        }
      ));
    }
    Ok(words)
  }
  fn is_duplicate_rejection(&self, output: &str) -> bool {
    let output = output.to_ascii_lowercase();
    output.contains("cannot publish over") || output.contains("epublishconflict")
  }
}

impl Registry for CargoRegistry {
  fn label(&self) -> &'static str {
    "cargo"
  }
  fn url(&self, name: &str, version: &Version) -> String {
    format!("https://crates.io/crates/{name}/{version}")
  }
  fn exists(
    &self,
    http: &HttpClient,
    endpoints: &RegistryConfig,
    name: &str,
    version: &Version,
  ) -> Result<bool> {
    ensure!(
      !name.is_empty()
        && name
          .bytes()
          .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_')),
      "invalid crate name {name}"
    );
    let name = name.to_ascii_lowercase();
    let path = match name.len() {
      1 => format!("1/{name}"),
      2 => format!("2/{name}"),
      3 => format!("3/{}/{name}", &name[..1]),
      _ => format!("{}/{}/{name}", &name[..2], &name[2..4]),
    };
    let response = http.get(http_util::url(
      &endpoints.cargo_index_url,
      &path.split('/').collect::<Vec<_>>(),
    )?)?;
    if response.status == StatusCode::NOT_FOUND {
      return Ok(false);
    }
    response.ensure_success()?;
    let text = std::str::from_utf8(&response.body).context("invalid crate index encoding")?;
    #[derive(serde::Deserialize)]
    struct Entry {
      vers: String,
    }
    let mut found = false;
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
      let entry: Entry = serde_json::from_str(line).context("invalid crate index entry")?;
      // Yanked versions also occupy their version number.
      found |= entry.vers == version.to_string();
    }
    ensure!(!text.trim().is_empty(), "empty crate index response");
    Ok(found)
  }
  fn command(&self, config: &PackagesConfig, _: &Version) -> Result<Vec<String>> {
    command(&config.cargo_publish_command, &config.cargo_publish_args)
  }
  fn is_duplicate_rejection(&self, output: &str) -> bool {
    output.to_ascii_lowercase().contains("is already uploaded")
  }
}

impl Registry for JsrRegistry {
  fn label(&self) -> &'static str {
    "jsr"
  }
  fn url(&self, name: &str, version: &Version) -> String {
    format!("https://jsr.io/{name}@{version}")
  }
  fn exists(
    &self,
    http: &HttpClient,
    endpoints: &RegistryConfig,
    name: &str,
    version: &Version,
  ) -> Result<bool> {
    let (scope, package) = name
      .strip_prefix('@')
      .and_then(|n| n.split_once('/'))
      .context("JSR package name must be @scope/name")?;
    ensure!(
      !scope.is_empty() && !package.is_empty() && !package.contains('/'),
      "invalid JSR package name {name}"
    );
    let response = http.get(http_util::url(
      &endpoints.jsr_api_url,
      &[
        "scopes",
        scope,
        "packages",
        package,
        "versions",
        &version.to_string(),
      ],
    )?)?;
    if response.status == StatusCode::NOT_FOUND {
      return Ok(false);
    }
    response.ensure_success()?;
    Ok(true)
  }
  fn command(&self, config: &PackagesConfig, _: &Version) -> Result<Vec<String>> {
    command(&config.jsr_publish_command, &config.jsr_publish_args)
  }
  fn is_duplicate_rejection(&self, _: &str) -> bool {
    false
  }
}

fn command(cmd: &str, extra: &[String]) -> Result<Vec<String>> {
  let mut words = shell_words::split(cmd).context("invalid publish command quoting")?;
  ensure!(!words.is_empty(), "publish command cannot be empty");
  words.extend(extra.iter().cloned());
  Ok(words)
}

pub fn execute(cwd: &Path, words: &[String], dry_run: bool) -> Result<()> {
  ensure!(!words.is_empty(), "empty command");
  println!("{}: {}", cwd.display(), shell_words::join(words));
  if dry_run {
    return Ok(());
  }
  let status = Command::new(&words[0])
    .args(&words[1..])
    .current_dir(cwd)
    .status()
    .with_context(|| format!("cannot execute {}", words[0]))?;
  ensure!(
    status.success(),
    "{} failed in {} ({status})",
    words[0],
    cwd.display()
  );
  Ok(())
}

pub fn publish(
  registry: &impl Registry,
  cwd: &Path,
  words: &[String],
  dry_run: bool,
) -> Result<()> {
  ensure!(!words.is_empty(), "empty publish command");
  println!("{}: {}", cwd.display(), shell_words::join(words));
  if dry_run {
    return Ok(());
  }
  let output = Command::new(&words[0])
    .args(&words[1..])
    .current_dir(cwd)
    .output()
    .with_context(|| format!("cannot execute {}", words[0]))?;
  std::io::stdout().write_all(&output.stdout)?;
  std::io::stderr().write_all(&output.stderr)?;
  if !output.status.success() {
    let combined = format!(
      "{}\n{}",
      String::from_utf8_lossy(&output.stdout),
      String::from_utf8_lossy(&output.stderr)
    );
    ensure!(
      registry.is_duplicate_rejection(&combined),
      "{} failed in {} ({})",
      words[0],
      cwd.display(),
      output.status
    );
    println!(
      "{}: registry reports this version is already published",
      registry.label()
    );
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::config::HttpConfig;

  #[test]
  fn cargo_index_handles_short_names_and_rejects_malformed_entries() {
    let mut server = mockito::Server::new();
    let endpoints = RegistryConfig {
      cargo_index_url: server.url(),
      ..Default::default()
    };
    let http = HttpClient::new(&HttpConfig::default()).unwrap();
    let version = Version::parse("1.0.0").unwrap();
    for (name, path) in [
      ("a", "/1/a"),
      ("ab", "/2/ab"),
      ("ABC", "/3/a/abc"),
      ("core", "/co/re/core"),
    ] {
      let mock = server
        .mock("GET", path)
        .with_body("{\"vers\":\"1.0.0\",\"yanked\":true}\n")
        .expect(2)
        .create();
      assert!(
        CargoRegistry
          .exists(&http, &endpoints, name, &version)
          .unwrap()
      );
      assert!(
        !CargoRegistry
          .exists(&http, &endpoints, name, &Version::parse("1.0.1").unwrap())
          .unwrap()
      );
      mock.assert();
    }
    let mock = server
      .mock("GET", "/ba/d_/bad_data")
      .with_body("invalid json")
      .create();
    assert!(
      CargoRegistry
        .exists(&http, &endpoints, "bad_data", &version)
        .is_err()
    );
    mock.assert();
  }

  #[cfg(unix)]
  #[test]
  fn publish_accepts_only_recognized_duplicate_rejections() {
    let dir = tempfile::tempdir().unwrap();
    for (registry, message, accepted) in [
      (Registries::npm(), "npm ERR! EPUBLISHCONFLICT", true),
      (
        Registries::npm(),
        "You cannot publish over the previously published versions",
        true,
      ),
      (
        Registries::cargo(),
        "crate version 1.0.0 is already uploaded",
        true,
      ),
      (Registries::npm(), "npm ERR! E403 permission denied", false),
      (Registries::cargo(), "authentication failed", false),
      (Registries::jsr(), "authentication failed", false),
    ] {
      let words = [
        "sh",
        "-c",
        "printf '%s' \"$1\" >&2; exit 1",
        "test",
        message,
      ]
      .map(String::from);
      assert_eq!(
        publish(&registry, dir.path(), &words, false).is_ok(),
        accepted
      );
    }
  }
}
