use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path};

/// Where the config is, relative to the working directory.
pub const CONFIG_PATH: &str = ".prp/config.toml";

#[derive(Debug, Clone)]
pub struct ResolvedConfig {
  pub root: Option<String>,
  pub git: ResolvedGitConfig,
  pub github: ResolvedGitHubConfig,
  pub changelog: ResolvedChangelogConfig,
  pub packages: ResolvedPackagesConfig,
  pub http: HttpConfig,
  pub registry: RegistryConfig,
}

impl ResolvedConfig {
  pub fn load(file: &Path) -> Result<Self> {
    let raw = std::fs::read(file).map_err(|e| anyhow!("Fail to read config file: {}", e))?;
    Self::parse(&raw)
  }

  pub fn parse(raw: &[u8]) -> Result<Self> {
    let toml = toml::from_slice(raw).context("Fail to parse config file: Invalid TOML")?;
    let config = serde_json::from_value::<Config>(toml)
      .map_err(|e| anyhow!("Fail to parse config file: {}", e))?;

    let resolved = Self {
      root: config.root,
      git: ResolvedGitConfig::from(config.git),
      github: ResolvedGitHubConfig::from(config.github),
      changelog: ResolvedChangelogConfig::from(config.changelog),
      packages: ResolvedPackagesConfig::from(config.packages),
      http: config.http,
      registry: config.registry,
    };
    Ok(resolved)
  }
}

#[derive(Debug, Clone)]
pub struct ResolvedGitConfig {
  pub default_branch: String,
  pub release_branches: Vec<String>,
  pub release_tag_format: String,
}

impl From<Option<GitConfig>> for ResolvedGitConfig {
  fn from(value: Option<GitConfig>) -> Self {
    let val = value.unwrap_or_default();
    Self {
      default_branch: val.default_branch.unwrap_or("main".to_string()),
      release_branches: val.release_branches.unwrap_or(vec![]),
      release_tag_format: val
        .release_tag_format
        .unwrap_or("{package_name}-v{version}".to_string()),
    }
  }
}

#[derive(Debug, Clone)]
pub struct ResolvedGitHubConfig {
  pub api_url: String,
  pub owner: Option<String>,
  pub repo: Option<String>,
  pub skip_release: bool,
  pub release_pr_branch_format: String,
  pub release_pr_title_format: String,
  pub release_pr_labels: Vec<String>,
}

impl From<Option<GitHubConfig>> for ResolvedGitHubConfig {
  fn from(value: Option<GitHubConfig>) -> Self {
    let val = value.unwrap_or_default();
    Self {
      api_url: val.api_url.unwrap_or("https://api.github.com".to_string()),
      owner: val.owner,
      repo: val.repo,
      skip_release: val.skip_releases.unwrap_or(false),
      release_pr_branch_format: val
        .release_pr_branch_format
        .unwrap_or("prp/{branch}_{commit}".to_string()),
      release_pr_title_format: val
        .release_pr_title_format
        .unwrap_or("release: {num}(s) packages".to_string()),
      release_pr_labels: val.release_pr_labels.unwrap_or(vec![]),
    }
  }
}

#[derive(Debug, Clone)]
pub struct ResolvedChangelogConfig {
  pub root: bool,
  pub root_filename: String,
  pub filename: String,
  pub commit_templates: Vec<String>,
  pub breaking_prefix: String,
  pub show_commits: bool,
  pub show_registries: bool,
}

impl From<Option<ChangelogConfig>> for ResolvedChangelogConfig {
  fn from(value: Option<ChangelogConfig>) -> Self {
    let val = value.unwrap_or_default();
    let mut commit_templates = val.commit_templates.unwrap_or(vec![]);
    if commit_templates.is_empty() {
      commit_templates.push("- {summary} by {author_name} ({sha})".into());
    }

    Self {
      root: val.root.unwrap_or(true),
      root_filename: val.root_filename.unwrap_or("CHANGELOG.md".to_string()),
      filename: val.filename.unwrap_or("CHANGELOG.md".to_string()),
      commit_templates,
      breaking_prefix: val.breaking_prefix.unwrap_or("**[BREAKING]** ".into()),
      show_commits: val.show_commits.unwrap_or(true),
      show_registries: val.show_registries.unwrap_or(true),
    }
  }
}

#[derive(Debug, Clone)]
pub struct ResolvedPackagesConfig {
  pub includes: Vec<String>,
  pub excludes: Vec<String>,
  pub fixed: Vec<Vec<String>>,
  pub linked: Vec<Vec<String>>,
  pub changes_ignore: Vec<String>,
  pub overrides: BTreeMap<String, ResolvedPackageConfig>,
}

impl From<PackagesConfig> for ResolvedPackagesConfig {
  fn from(val: PackagesConfig) -> Self {
    Self {
      includes: val.includes,
      excludes: val.excludes.unwrap_or(vec![]),
      fixed: val.fixed.unwrap_or(vec![]),
      linked: val.linked.unwrap_or(vec![]),
      changes_ignore: val.changes_ignore.unwrap_or(vec!["**/*.md".to_string()]),
      overrides: val
        .overrides
        .into_iter()
        .map(|(name, config)| (name, ResolvedPackageConfig::from(config)))
        .collect(),
    }
  }
}

#[derive(Debug, Clone, Default)]
pub struct ResolvedPackageConfig {
  pub name: Option<String>,
  pub release_tag_format: Option<String>,
  pub manifests: Vec<String>,
  pub before_publish_scripts: Vec<ScriptConfig>,
  pub assets: Vec<String>,
  pub changes_ignore: Vec<String>,
  pub skip_github_release: bool,
  pub skip_publish: bool,
}

impl From<PackageConfig> for ResolvedPackageConfig {
  fn from(value: PackageConfig) -> Self {
    Self {
      name: value.name,
      release_tag_format: value.release_tag_format,
      manifests: value.manifests.unwrap_or(vec![]),
      before_publish_scripts: value.before_publish_scripts.unwrap_or(vec![]),
      assets: value.assets.unwrap_or(vec![]),
      changes_ignore: value.changes_ignore.unwrap_or(vec![]),
      skip_github_release: value.skip_github_release.unwrap_or(false),
      skip_publish: value.skip_publish.unwrap_or(false),
    }
  }
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
  pub root: Option<String>,
  pub git: Option<GitConfig>,
  pub github: Option<GitHubConfig>,
  pub changelog: Option<ChangelogConfig>,
  pub packages: PackagesConfig,
  #[serde(default, skip_serializing_if = "is_default")]
  pub http: HttpConfig,
  #[serde(default, skip_serializing_if = "is_default")]
  pub registry: RegistryConfig,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GitConfig {
  pub default_branch: Option<String>,
  pub release_branches: Option<Vec<String>>,
  pub release_tag_format: Option<String>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GitHubConfig {
  pub api_url: Option<String>,
  pub owner: Option<String>,
  pub repo: Option<String>,
  pub skip_releases: Option<bool>,
  pub release_pr_branch_format: Option<String>,
  pub release_pr_title_format: Option<String>,
  pub release_pr_labels: Option<Vec<String>>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ChangelogConfig {
  pub root: Option<bool>,
  pub root_filename: Option<String>,
  pub filename: Option<String>,
  pub commit_templates: Option<Vec<String>>,
  pub breaking_prefix: Option<String>,
  pub show_commits: Option<bool>,
  pub show_registries: Option<bool>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct PackagesConfig {
  pub includes: Vec<String>,
  pub excludes: Option<Vec<String>>,
  pub fixed: Option<Vec<Vec<String>>>,
  pub linked: Option<Vec<Vec<String>>>,
  pub changes_ignore: Option<Vec<String>>,
  #[serde(flatten)]
  pub overrides: BTreeMap<String, PackageConfig>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PackageConfig {
  pub name: Option<String>,
  pub release_tag_format: Option<String>,
  pub manifests: Option<Vec<String>>,
  pub before_publish_scripts: Option<Vec<ScriptConfig>>,
  pub assets: Option<Vec<String>>,
  pub changes_ignore: Option<Vec<String>>,
  pub skip_github_release: Option<bool>,
  pub skip_publish: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScriptConfig {
  pub command: String,
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub args: Vec<String>,
}

#[derive(Debug, Default, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HttpConfig {
  pub user_agent: Option<String>,
  pub timeout_secs: Option<u64>,
}

#[derive(Debug, Default, Clone, PartialEq, Deserialize, Serialize)]
pub struct RegistryConfig {
  pub npm: Option<NpmRegistryConfig>,
  pub cargo: Option<CargoRegistryConfig>,
  pub jsr: Option<JsrRegistryConfig>,
}

#[derive(Debug, Default, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NpmRegistryConfig {
  pub registry: Option<String>,
  pub publish_registry: Option<String>,
  pub publish_command: Option<String>,
}

#[derive(Debug, Default, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CargoRegistryConfig {
  pub index: Option<String>,
}

#[derive(Debug, Default, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct JsrRegistryConfig {
  pub api_url: Option<String>,
  pub publish_command: Option<String>,
}

fn is_default<T: Default + PartialEq>(value: &T) -> bool {
  *value == T::default()
}
