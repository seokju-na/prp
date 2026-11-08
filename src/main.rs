mod action;
mod bump;
mod cargo;
mod changelog;
mod changes;
mod commands;
mod config;
mod deno;
mod dependency;
mod detection;
mod edit;
mod fs_util;
mod git;
mod github;
mod http;
mod manifest;
mod npm;
mod package;
mod plan;
mod registry;
mod release_record;
mod serde_util;
mod workspace;

use crate::commands::Outcome;
use crate::commands::change::{ChangeArgs, change};
use crate::commands::init::{InitArgs, init};
use crate::commands::prepare::{PrepareArgs, prepare};
use crate::commands::release::{ReleaseArgs, release};
use crate::commands::status::status;
use crate::config::{CONFIG_PATH, ResolvedConfig};
use crate::workspace::Workspace;
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf, absolute};

#[derive(Parser)]
#[command(version, about = "Please Release Please")]
struct Cli {
  /// Run as if started in this directory.
  #[arg(long, global = true)]
  pub cwd: Option<PathBuf>,
  /// The config file, relative to `--cwd`. Defaults to `.prp/config.toml`.
  #[arg(long, global = true)]
  pub config: Option<PathBuf>,
  #[command(subcommand)]
  pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
  /// Write a config file for the repository.
  Init(InitArgs),
  /// Write down a notable change of the current commit.
  Change(ChangeArgs),
  /// Bump versions, update changelogs and open a release pull request.
  Prepare(PrepareArgs),
  /// Publish packages, push tags and create GitHub releases.
  Release(ReleaseArgs),
  /// List the packages, their groups and their latest published versions.
  Status,
}

/// The working directory and the config file the command runs with.
fn resolve_paths(cli: &Cli) -> Result<(PathBuf, PathBuf)> {
  let cwd = match &cli.cwd {
    Some(cwd) => absolute(cwd).with_context(|| format!("Invalid path: {}", cwd.display()))?,
    None => std::env::current_dir().context("Could not get current working directory")?,
  };
  let config_path = cwd.join(cli.config.as_deref().unwrap_or(Path::new(CONFIG_PATH)));
  Ok((cwd, config_path))
}

fn load_workspace(cwd: &Path, config_path: &Path) -> Result<Workspace> {
  let config = ResolvedConfig::load(config_path)?;
  let root = match &config.root {
    Some(root) => cwd.join(root),
    None => cwd.to_path_buf(),
  };
  Workspace::load(&root, config)
}

fn run_cli(cli: Cli) -> Result<Outcome> {
  let (cwd, config_path) = resolve_paths(&cli)?;
  let workspace = || load_workspace(&cwd, &config_path);
  let runtime = tokio::runtime::Builder::new_multi_thread()
    .enable_all()
    .build()
    .context("Failed to start the async runtime")?;
  runtime.block_on(async {
    match cli.command {
      // There is no workspace to load before its config exists.
      Command::Init(args) => init(&cwd, &config_path, args).await,
      Command::Change(args) => change(&workspace()?, args).await,
      Command::Prepare(args) => prepare(&workspace()?, args).await,
      Command::Release(args) => release(&workspace()?, args).await,
      Command::Status => status(&workspace()?).await,
    }
  })
}

fn main() -> std::process::ExitCode {
  match run_cli(Cli::parse()) {
    Ok(outcome) => outcome.into(),
    Err(error) => {
      eprintln!("{error:#}");
      std::process::ExitCode::FAILURE
    }
  }
}
