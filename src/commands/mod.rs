pub mod artifacts;
pub mod change;
pub mod init;
pub mod prepare;
pub mod release;

use crate::{
  cli::{Cli, Command},
  config::Config,
  git::Git,
};
use anyhow::Result;

pub fn run(cli: Cli) -> Result<()> {
  let git = Git::discover()?;
  match cli.command {
    Command::Init => init::run(&git),
    Command::Change(args) => change::run(&git, args),
    command => {
      let config = Config::load(&git.root)?;
      let packages = crate::package::load(&git.root, &config)?;
      match command {
        Command::Prepare(args) => prepare::run(&git, &config, &packages, args),
        Command::Release(args) => release::run(&git, &config, &packages, args),
        Command::Artifacts(args) => artifacts::run(&git, &config, packages, args),
        _ => unreachable!(),
      }
    }
  }
}
