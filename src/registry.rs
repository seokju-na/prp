use crate::cargo::CargoRegistry;
use crate::config::{HttpConfig, RegistryConfig};
use crate::deno::JsrRegistry;
use crate::manifest::ManifestKind;
use crate::npm::NpmRegistry;
use anyhow::{Context, Result};
use semver::Version;
use std::ffi::OsStr;
use std::path::Path;
use std::process::{ExitStatus, Stdio};
use tokio::process::Command;

#[derive(Debug, Clone)]
pub enum RegistryPublishResult {
  Success,
  Duplicated,
  Error { status: ExitStatus, message: String },
}

pub trait Registry {
  fn name(&self) -> &'static str;
  fn url(&self, name: &str, version: &Version) -> String;
  async fn exists(&self, name: &str, version: &Version) -> Result<bool>;
  /// The version the registry hands out by default, or `None` when it has no
  /// release of `name` yet.
  async fn latest(&self, name: &str) -> Result<Option<Version>>;
  async fn publish<I, K, V>(
    &self,
    manifest_dir: &Path,
    args: &[String],
    envs: I,
  ) -> Result<RegistryPublishResult>
  where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<OsStr>,
    V: AsRef<OsStr>;
}

/// The registries a release goes to. `Registry` has generic methods, so
/// dispatching on the manifest kind happens here.
#[derive(Debug, Clone)]
pub struct Registries {
  npm: NpmRegistry,
  cargo: CargoRegistry,
  jsr: JsrRegistry,
}

impl Registries {
  pub fn new(config: &RegistryConfig, http: &HttpConfig) -> Result<Self> {
    Ok(Self {
      npm: NpmRegistry::new(&config.npm.clone().unwrap_or_default(), http)?,
      cargo: CargoRegistry::new(&config.cargo.clone().unwrap_or_default(), http)?,
      jsr: JsrRegistry::new(&config.jsr.clone().unwrap_or_default(), http)?,
    })
  }

  pub fn name(&self, kind: ManifestKind) -> &'static str {
    match kind {
      ManifestKind::PackageJson => self.npm.name(),
      ManifestKind::CargoToml => self.cargo.name(),
      ManifestKind::DenoJson => self.jsr.name(),
    }
  }

  pub fn url(&self, kind: ManifestKind, name: &str, version: &Version) -> String {
    match kind {
      ManifestKind::PackageJson => self.npm.url(name, version),
      ManifestKind::CargoToml => self.cargo.url(name, version),
      ManifestKind::DenoJson => self.jsr.url(name, version),
    }
  }

  pub async fn exists(&self, kind: ManifestKind, name: &str, version: &Version) -> Result<bool> {
    match kind {
      ManifestKind::PackageJson => self.npm.exists(name, version).await,
      ManifestKind::CargoToml => self.cargo.exists(name, version).await,
      ManifestKind::DenoJson => self.jsr.exists(name, version).await,
    }
  }

  pub async fn latest(&self, kind: ManifestKind, name: &str) -> Result<Option<Version>> {
    match kind {
      ManifestKind::PackageJson => self.npm.latest(name).await,
      ManifestKind::CargoToml => self.cargo.latest(name).await,
      ManifestKind::DenoJson => self.jsr.latest(name).await,
    }
  }

  pub async fn publish(
    &self,
    kind: ManifestKind,
    manifest_dir: &Path,
    args: &[String],
  ) -> Result<RegistryPublishResult> {
    let envs = std::iter::empty::<(&str, &str)>();
    match kind {
      ManifestKind::PackageJson => self.npm.publish(manifest_dir, args, envs).await,
      ManifestKind::CargoToml => self.cargo.publish(manifest_dir, args, envs).await,
      ManifestKind::DenoJson => self.jsr.publish(manifest_dir, args, envs).await,
    }
  }
}

/// Runs a publish command. `is_duplicated` receives the lowercased output of a
/// failed run and tells whether the registry already has this version.
pub async fn run_publish_command(
  mut command: Command,
  is_duplicated: impl Fn(&str) -> bool,
) -> Result<RegistryPublishResult> {
  let program = command.as_std().get_program().to_string_lossy().to_string();
  let output = command
    .stdin(Stdio::null())
    .output()
    .await
    .with_context(|| format!("Failed to execute {program}"))?;
  if output.status.success() {
    return Ok(RegistryPublishResult::Success);
  }

  let message = format!(
    "{}\n{}",
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr),
  );
  if is_duplicated(&message.to_ascii_lowercase()) {
    return Ok(RegistryPublishResult::Duplicated);
  }
  Ok(RegistryPublishResult::Error {
    status: output.status,
    message,
  })
}
