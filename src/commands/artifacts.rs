use crate::{config::Config, git::Git, package::Package};
use anyhow::{Result, ensure};
use clap::Args;
use std::collections::BTreeSet;

#[derive(Args)]
pub struct Artifacts {
  /// Collect/restore only these package directories or names (repeat for multiple packages).
  #[arg(long)]
  pub package: Vec<String>,
  #[arg(long)]
  pub restore: bool,
  #[arg(long)]
  pub dry_run: bool,
}

pub fn run(git: &Git, config: &Config, packages: Vec<Package>, args: Artifacts) -> Result<()> {
  let packages = if args.package.is_empty() {
    packages
  } else {
    let mut selected = BTreeSet::new();
    for selector in &args.package {
      let matches = packages
        .iter()
        .enumerate()
        .filter(|(_, p)| p.matches(selector))
        .collect::<Vec<_>>();
      ensure!(
        matches.len() == 1,
        "package '{selector}' is missing or ambiguous; use its directory path"
      );
      selected.insert(matches[0].0);
    }
    packages
      .into_iter()
      .enumerate()
      .filter(|(i, _)| selected.contains(i))
      .map(|(_, p)| p)
      .collect()
  };
  if args.restore {
    crate::artifacts::restore(&git.root, config, &packages, args.dry_run, false)
  } else {
    crate::artifacts::collect(&git.root, config, &packages, args.dry_run)
  }
}
