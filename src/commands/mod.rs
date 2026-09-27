pub mod prepare_release;
pub mod release;

use crate::{
  cli::{Cli, Command},
  config::{CONFIG_PATH, Config},
  git::Git,
};
use anyhow::{Context, Result};
use std::{fs, io::Write};

pub fn run(cli: Cli) -> Result<()> {
  let git = Git::discover()?;
  match cli.command {
    Command::Init => {
      let path = git.root.join(CONFIG_PATH);
      fs::create_dir_all(path.parent().unwrap())?;
      let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .context("config already exists or cannot be created")?;
      file.write_all(b"[git]\ndefault_branch = \"main\"\nrelease_tag_format = \"{package_name}-v{version}\"\n\n[packages]\n# Empty include discovers npm/pnpm, Cargo, and Deno workspace members.\ninclude = []\nexclude = []\nchanges_ignore = [\"**/*.md\", \"**/*.markdown\"]\n\n[changelog]\nroot = true\nfilename = \"CHANGELOG.md\"\nshow_registries = true\n")?;
      println!("Created {CONFIG_PATH}");
      Ok(())
    }
    Command::Change(args) => crate::changelog::create(&git, args),
    command => {
      let config = Config::load(&git.root)?;
      let packages = crate::package::load(&git.root, &config)?;
      match command {
        Command::Prepare(args) => prepare_release::run(&git, &config, &packages, args),
        Command::Release(args) => release::run(&git, &config, &packages, args),
        Command::Artifacts(args) => {
          if args.restore {
            crate::artifacts::restore(&git.root, &config, &packages, args.dry_run, false)
          } else {
            crate::artifacts::collect(&git.root, &config, &packages, args.dry_run)
          }
        }
        _ => unreachable!(),
      }
    }
  }
}
