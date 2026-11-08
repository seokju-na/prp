use crate::action::{Action, ActionRunner};
use crate::commands::Outcome;
use crate::config::{Config, GitConfig, PackagesConfig, ResolvedConfig};
use crate::edit::Patch;
use crate::git::Git;
use crate::workspace::Workspace;
use crate::{cargo, deno, fs_util, npm};
use anyhow::{Context, Result, ensure};
use clap::Args;
use std::path::Path;

#[derive(Args, Default)]
pub struct InitArgs {
  /// Replace an existing config file.
  #[arg(long)]
  pub force: bool,
  /// Show the config without writing it.
  #[arg(long)]
  pub dry_run: bool,
}

pub async fn init(root: &Path, config_path: &Path, args: InitArgs) -> Result<Outcome> {
  let existing = fs_util::read_to_string_optional(config_path)?;
  ensure!(
    existing.is_none() || args.force,
    "{} already exists; use --force to replace it",
    config_path.display()
  );

  let patterns = PackagePatterns::detect(root)?;
  let config = toml::to_string_pretty(&patterns.config(&default_branch(&Git::new(root))))
    .context("Failed to write the config")?;
  show_packages(root, &config, &patterns)?;

  let action = Action::FileWrite {
    path: config_path.to_path_buf(),
    patch: Patch::new(existing.as_deref().unwrap_or_default(), &config)?,
  };
  ActionRunner::new(root, args.dry_run).run(&[action]).await?;
  Ok(Outcome::Done)
}

/// Where the repository declares its packages.
#[derive(Debug, Default, PartialEq)]
struct PackagePatterns {
  includes: Vec<String>,
  excludes: Vec<String>,
  /// The files the patterns come from.
  sources: Vec<&'static str>,
}

impl PackagePatterns {
  fn detect(root: &Path) -> Result<Self> {
    let declared = [
      ("package.json", npm::workspace_patterns(root)?),
      ("pnpm-workspace.yaml", npm::pnpm_workspace_patterns(root)?),
      ("Cargo.toml", cargo::workspace_patterns(root)?),
      ("deno.json", deno::workspace_patterns(root)?),
    ];
    let mut patterns = Self::default();
    for (source, found) in declared {
      let Some(found) = found else {
        continue;
      };
      patterns.sources.push(source);
      for pattern in found {
        let (list, pattern) = match pattern.strip_prefix('!') {
          Some(excluded) => (&mut patterns.excludes, excluded),
          None => (&mut patterns.includes, pattern.as_str()),
        };
        let pattern = normalize(pattern);
        if !list.contains(&pattern) {
          list.push(pattern);
        }
      }
    }
    Ok(patterns)
  }

  fn config(&self, default_branch: &str) -> Config {
    Config {
      git: Some(GitConfig {
        default_branch: Some(default_branch.to_string()),
        ..Default::default()
      }),
      packages: PackagesConfig {
        includes: self.includes.clone(),
        excludes: (!self.excludes.is_empty()).then(|| self.excludes.clone()),
        ..Default::default()
      },
      ..Default::default()
    }
  }
}

/// `./packages/*/` and `packages/*` are the same glob to prp.
fn normalize(pattern: &str) -> String {
  let pattern = pattern.trim();
  let pattern = pattern.strip_prefix("./").unwrap_or(pattern);
  match pattern.trim_end_matches('/') {
    "" => ".".to_string(),
    pattern => pattern.to_string(),
  }
}

/// The branch origin points at, or the conventional one that exists.
fn default_branch(git: &Git) -> String {
  if let Ok(head) = git.run(&[
    "symbolic-ref",
    "--quiet",
    "--short",
    "refs/remotes/origin/HEAD",
  ]) && let Some(branch) = head.trim().strip_prefix("origin/")
  {
    return branch.to_string();
  }
  ["main", "master"]
    .into_iter()
    .find(|branch| git.resolve(&format!("refs/heads/{branch}")).is_ok())
    .unwrap_or("main")
    .to_string()
}

/// What prp makes of the repository with `config`.
fn show_packages(root: &Path, config: &str, patterns: &PackagePatterns) -> Result<()> {
  match patterns.sources.is_empty() {
    true => println!("No workspace declaration found; every manifest with a version is a package."),
    false => println!("Detected packages from {}.", patterns.sources.join(", ")),
  }
  let config = ResolvedConfig::parse(config.as_bytes()).context("Generated an invalid config")?;
  let workspace = match Workspace::load(root, config) {
    Ok(workspace) => workspace,
    Err(error) => {
      eprintln!("warning: {error:#}; edit [packages] to point at your packages");
      return Ok(());
    }
  };

  let packages = &workspace.packages;
  let width = packages
    .iter()
    .map(|package| package.id().len())
    .max()
    .unwrap_or_default();
  let noun = if packages.len() == 1 {
    "package"
  } else {
    "packages"
  };
  println!("Found {} {noun}:", packages.len());
  for package in packages {
    let manifests = package
      .manifests()
      .iter()
      .map(|manifest| manifest.name())
      .collect::<Vec<_>>();
    println!("  {:width$}  {}", package.id(), manifests.join(", "));
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  fn patterns(includes: &[&str], excludes: &[&str]) -> PackagePatterns {
    PackagePatterns {
      includes: includes.iter().map(|x| x.to_string()).collect(),
      excludes: excludes.iter().map(|x| x.to_string()).collect(),
      sources: vec!["package.json"],
    }
  }

  fn render(patterns: &PackagePatterns, default_branch: &str) -> String {
    toml::to_string_pretty(&patterns.config(default_branch)).unwrap()
  }

  #[test]
  fn normalizes_workspace_globs() {
    assert_eq!(normalize("./packages/*/"), "packages/*");
    assert_eq!(normalize(" crates/core "), "crates/core");
    assert_eq!(normalize("./"), ".");
  }

  #[test]
  fn writes_only_what_was_detected() {
    let config = render(&patterns(&["packages/*"], &[]), "main");
    assert_eq!(
      config,
      "[git]\ndefault_branch = \"main\"\n\n[packages]\nincludes = [\"packages/*\"]\n"
    );
  }

  #[test]
  fn renders_a_config_prp_can_read() {
    let patterns = patterns(&["packages/*", "crates/\"q\""], &["packages/old"]);
    let resolved = ResolvedConfig::parse(render(&patterns, "trunk").as_bytes()).unwrap();
    assert_eq!(resolved.git.default_branch, "trunk");
    assert_eq!(resolved.packages.includes, ["packages/*", "crates/\"q\""]);
    assert_eq!(resolved.packages.excludes, ["packages/old"]);
  }

  #[test]
  fn breaks_long_lists_into_lines() {
    let long = (0..8)
      .map(|i| format!("packages/group-{i}/*"))
      .collect::<Vec<_>>();
    let includes = long.iter().map(String::as_str).collect::<Vec<_>>();
    let config = render(&patterns(&includes, &[]), "main");
    assert!(
      config.contains("includes = [\n    \"packages/group-0/*\",\n"),
      "{config}"
    );
    let resolved = ResolvedConfig::parse(config.as_bytes()).unwrap();
    assert_eq!(resolved.packages.includes, long);
  }
}
