use clap::{Args, Parser, Subcommand};

#[derive(Parser)]
#[command(
  version,
  about = "Review changes, version packages, and publish a monorepo"
)]
pub struct Cli {
  #[command(subcommand)]
  pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
  /// Create .prp/config.toml with workspace discovery enabled.
  Init,
  /// Write an optional notable-change note.
  Change(Change),
  /// Review commits and prepare versions, changelogs, and a release PR.
  #[command(alias = "prepare-release")]
  Prepare(Prepare),
  /// Publish packages whose version increased in HEAD compared with HEAD^.
  Release(Release),
  /// Collect configured build artifacts, or restore them to package directories.
  Artifacts(Artifacts),
}

#[derive(Args)]
pub struct Change {
  #[arg(long)]
  pub title: Option<String>,
  #[arg(long)]
  pub body: Option<String>,
  #[arg(long)]
  pub commit: Option<String>,
  #[arg(long)]
  pub breaking: bool,
}

#[derive(Args, Default)]
pub struct Prepare {
  /// Package directory or manifest name. Omit to review all candidates interactively.
  pub package: Option<String>,
  /// Bump type (patch, minor, major, premajor, preminor, prepatch, prerelease, release) or exact version.
  #[arg(requires = "package")]
  pub bump: Option<String>,
  /// Changelog commit IDs, comma-separated; use "none" for no commit entries.
  #[arg(long, requires = "package")]
  pub changes: Option<String>,
  #[arg(long)]
  pub from: Option<String>,
  #[arg(long, default_value = "HEAD")]
  pub to: String,
  /// Must resolve to checked-out HEAD; the PR base uses the current branch.
  #[arg(long = "in")]
  pub reference: Option<String>,
  #[arg(long)]
  pub allow_dirty: bool,
  /// Write changes locally without committing, pushing, or opening a PR.
  #[arg(long)]
  pub skip_commit: bool,
  /// Create a local release branch without pushing or opening a PR.
  #[arg(long)]
  pub skip_pr: bool,
  /// Persist skip_publish = true in the selected packages' configuration.
  #[arg(long)]
  pub skip_publish: bool,
  #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
  pub draft: bool,
  #[arg(long)]
  pub dry_run: bool,
  #[arg(long, default_value = "rc")]
  pub preid: String,
  #[arg(long, env = "GITHUB_TOKEN", hide_env_values = true)]
  pub github_token: Option<String>,
}

#[derive(Args)]
pub struct Release {
  #[arg(long)]
  pub dry_run: bool,
  #[arg(long)]
  pub skip_github_release: bool,
  /// Keep tags local (also requires --skip-github-release).
  #[arg(long, requires = "skip_github_release")]
  pub skip_push: bool,
  #[arg(long)]
  pub allow_missing_artifacts: bool,
  #[arg(long, env = "GITHUB_TOKEN", hide_env_values = true)]
  pub github_token: Option<String>,
}

#[derive(Args)]
pub struct Artifacts {
  #[arg(long)]
  pub restore: bool,
  #[arg(long)]
  pub dry_run: bool,
}
