use crate::{config::CONFIG_PATH, git::Git};
use anyhow::{Context, Result};
use std::{fs, io::Write};

pub fn run(git: &Git) -> Result<()> {
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
