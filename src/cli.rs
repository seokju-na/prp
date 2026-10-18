use crate::commands::{artifacts::Artifacts, change::Change, prepare::Prepare, release::Release};
use clap::{Parser, Subcommand};

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
  /// Publish committed manifest versions missing from their registries, then ensure tags/releases.
  Release(Release),
  /// Collect configured build artifacts, or restore them to package directories.
  Artifacts(Artifacts),
}
