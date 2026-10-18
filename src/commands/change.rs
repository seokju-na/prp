use crate::{changelog::Frontmatter, git::Git};
use anyhow::{Result, ensure};
use clap::Args;
use std::{
  fs,
  io::{IsTerminal, Write},
};

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

pub fn run(git: &Git, args: Change) -> Result<()> {
  let title = match args.title {
    Some(title) => title,
    None => {
      ensure!(
        std::io::stdin().is_terminal(),
        "--title is required without an interactive terminal"
      );
      dialoguer::Input::<String>::new()
        .with_prompt("Change title")
        .interact_text()?
    }
  };
  ensure!(
    !title.trim().is_empty() && !title.contains(['\n', '\r']),
    "title must be one non-empty line"
  );
  let body = match args.body {
    Some(body) => body,
    None if std::io::stdin().is_terminal() => {
      dialoguer::Editor::new().edit("")?.unwrap_or_default()
    }
    None => String::new(),
  };
  let commit = args
    .commit
    .as_deref()
    .map(|sha| git.resolve(sha))
    .transpose()?;
  let meta = Frontmatter {
    title,
    commit,
    breaking: args.breaking,
  };
  let content = format!(
    "---\n{}---\n\n{}\n",
    serde_yaml_ng::to_string(&meta)?,
    body.trim()
  );
  let dir = git.root.join(".prp/changes");
  fs::create_dir_all(&dir)?;
  let stamp = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)?
    .as_nanos();
  let path = dir.join(format!("{stamp}.md"));
  let mut file = fs::OpenOptions::new()
    .write(true)
    .create_new(true)
    .open(&path)?;
  file.write_all(content.as_bytes())?;
  println!("Created {}", path.strip_prefix(&git.root)?.display());
  Ok(())
}
