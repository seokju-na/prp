use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use std::{
  collections::BTreeMap,
  fs,
  path::{Component, Path},
};

pub const CONFIG_PATH: &str = ".prp/config.toml";

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
  pub git: GitConfig,
  pub github: GithubConfig,
  pub changelog: ChangelogConfig,
  pub artifacts: ArtifactsConfig,
  pub packages: PackagesConfig,
  pub snapshot: SnapshotConfig,
  pub http: HttpConfig,
  pub registries: RegistryConfig,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GitConfig {
  pub default_branch: String,
  pub release_branches: Vec<String>,
  pub release_tag_format: String,
}
impl Default for GitConfig {
  fn default() -> Self {
    Self {
      default_branch: "main".into(),
      release_branches: vec![],
      release_tag_format: "{package_name}-v{version}".into(),
    }
  }
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GithubConfig {
  pub api_url: String,
  pub owner: Option<String>,
  pub repo: Option<String>,
  pub skip_releases: bool,
  pub release_pr_branch_format: String,
  pub release_pr_title_format: String,
  pub release_pr_labels: Vec<String>,
}
impl Default for GithubConfig {
  fn default() -> Self {
    Self {
      api_url: "https://api.github.com".into(),
      owner: None,
      repo: None,
      skip_releases: false,
      release_pr_branch_format: "prp/{branch}_{commit}".into(),
      release_pr_title_format: "release: ".into(),
      release_pr_labels: vec![],
    }
  }
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ChangelogConfig {
  pub root: bool,
  pub filename: String,
  pub commit_templates: Vec<String>,
  pub breaking_prefix: String,
  pub show_commits: bool,
  pub show_registries: bool,
}
impl Default for ChangelogConfig {
  fn default() -> Self {
    Self {
      root: true,
      filename: "CHANGELOG.md".into(),
      commit_templates: vec!["- {summary} by {author_name} ({sha})".into()],
      breaking_prefix: "**[BREAKING]** ".into(),
      show_commits: true,
      show_registries: true,
    }
  }
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ArtifactsConfig {
  pub dir: String,
}
impl Default for ArtifactsConfig {
  fn default() -> Self {
    Self {
      dir: ".prp/artifacts".into(),
    }
  }
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct PackagesConfig {
  pub include: Vec<String>,
  pub exclude: Vec<String>,
  pub fixed: Vec<Vec<String>>,
  pub linked: Vec<Vec<String>>,
  pub changes_ignore: Vec<String>,
  pub npm_publish_command: String,
  pub npm_publish_args: Vec<String>,
  pub cargo_publish_command: String,
  pub cargo_publish_args: Vec<String>,
  #[serde(alias = "jsx_publish_command")]
  pub jsr_publish_command: String,
  pub jsr_publish_args: Vec<String>,
  #[serde(flatten)]
  pub overrides: BTreeMap<String, PackageConfig>,
}
impl Default for PackagesConfig {
  fn default() -> Self {
    Self {
      include: vec![],
      exclude: vec![],
      fixed: vec![],
      linked: vec![],
      changes_ignore: vec!["**/*.md".into(), "**/*.markdown".into()],
      npm_publish_command: "npm publish".into(),
      npm_publish_args: vec![],
      cargo_publish_command: "cargo publish".into(),
      cargo_publish_args: vec![],
      jsr_publish_command: "deno publish".into(),
      jsr_publish_args: vec![],
      overrides: BTreeMap::new(),
    }
  }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PackageConfig {
  pub display_name: Option<String>,
  pub manifests: Option<Vec<String>>,
  pub before_publish_scripts: Vec<Script>,
  pub artifacts: Vec<Artifact>,
  pub assets: Vec<String>,
  #[serde(alias = "changes_ignore")]
  pub change_ignore: Option<Vec<String>>,
  pub skip_github_release: bool,
  pub skip_publish: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Script {
  pub command: String,
  #[serde(default)]
  pub args: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
  pub source: Patterns,
  pub dest: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Patterns {
  One(String),
  Many(Vec<String>),
}
impl Patterns {
  pub fn values(&self) -> &[String] {
    match self {
      Self::One(s) => std::slice::from_ref(s),
      Self::Many(v) => v,
    }
  }
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SnapshotConfig {
  pub prerelease_format: String,
}
impl Default for SnapshotConfig {
  fn default() -> Self {
    Self {
      prerelease_format: "snapshot.{sha}".into(),
    }
  }
}

impl Config {
  pub fn load(root: &Path) -> Result<Self> {
    let raw = fs::read_to_string(root.join(CONFIG_PATH))
      .context("cannot read .prp/config.toml; run `prp init`")?;
    let config: Self = toml::from_str(&raw).context("invalid .prp/config.toml")?;
    ensure!(
      config.http.timeout_secs > 0
        && config.http.retries <= 10
        && config.http.max_retry_delay_secs > 0,
      "http.timeout_secs and max_retry_delay_secs must be positive; retries must be at most 10"
    );
    for endpoint in [
      &config.github.api_url,
      &config.registries.npm_url,
      &config.registries.cargo_index_url,
      &config.registries.jsr_api_url,
    ] {
      crate::http_util::endpoint(endpoint)?;
    }
    ensure!(
      config.packages.fixed.is_empty() && config.packages.linked.is_empty(),
      "fixed/linked version groups are deferred beyond 0.1.0; remove these settings"
    );
    ensure!(
      config.github.owner.is_some() == config.github.repo.is_some(),
      "github.owner and github.repo must be configured together"
    );
    ensure!(
      config.git.release_tag_format.contains("{package_name}")
        && config.git.release_tag_format.contains("{version}"),
      "release_tag_format must contain {{package_name}} and {{version}}"
    );
    relative_path(&config.changelog.filename)?;
    relative_path(&config.artifacts.dir)?;
    for (path, pkg) in &config.packages.overrides {
      relative_path(path)?;
      for artifact in &pkg.artifacts {
        for pattern in artifact.source.values() {
          relative_path(pattern)?;
        }
        if let Some(dest) = &artifact.dest {
          relative_path(dest)?;
        }
      }
      for pattern in pkg.assets.iter().chain(pkg.manifests.iter().flatten()) {
        relative_path(pattern)?;
      }
    }
    Ok(config)
  }
}

pub fn relative_path(path: &str) -> Result<()> {
  ensure!(
    !path.is_empty()
      && !Path::new(path).is_absolute()
      && !path.contains('\\')
      && !Path::new(path)
        .components()
        .any(|c| matches!(c, Component::ParentDir | Component::Prefix(_))),
    "expected a repository-relative path without '..': {path}"
  );
  Ok(())
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HttpConfig {
  pub timeout_secs: u64,
  pub retries: u32,
  pub retry_base_ms: u64,
  pub max_retry_delay_secs: u64,
}
impl Default for HttpConfig {
  fn default() -> Self {
    Self {
      timeout_secs: 10,
      retries: 2,
      retry_base_ms: 500,
      max_retry_delay_secs: 30,
    }
  }
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RegistryConfig {
  pub npm_url: String,
  pub cargo_index_url: String,
  pub jsr_api_url: String,
}
impl Default for RegistryConfig {
  fn default() -> Self {
    Self {
      npm_url: "https://registry.npmjs.org".into(),
      cargo_index_url: "https://index.crates.io".into(),
      jsr_api_url: "https://api.jsr.io".into(),
    }
  }
}
