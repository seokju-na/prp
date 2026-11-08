use crate::action::ActionRunner;
use crate::commands::Outcome;
use crate::workspace::Workspace;
use anyhow::{Result, ensure};
use clap::Args;
use std::io::IsTerminal;

#[derive(Args, Default)]
pub struct ChangeArgs {
  /// A detailed title of the change.
  #[arg(long)]
  pub title: Option<String>,
  /// A longer description of the change.
  #[arg(long)]
  pub body: Option<String>,
  /// Comma-separated packages the change belongs to.
  #[arg(long, value_delimiter = ',')]
  pub packages: Vec<String>,
  #[arg(long)]
  pub breaking: bool,
  #[arg(long)]
  pub dry_run: bool,
}

pub async fn change(workspace: &Workspace, args: ChangeArgs) -> Result<Outcome> {
  let interactive = std::io::stdin().is_terminal();
  let title = match args.title {
    Some(title) => title,
    None => {
      ensure!(interactive, "--title is required without a terminal");
      dialoguer::Input::<String>::new()
        .with_prompt("Change title")
        .interact_text()?
    }
  };
  ensure!(
    !title.trim().is_empty() && !title.contains(['\n', '\r']),
    "Title must be one non-empty line"
  );

  let body = match args.body {
    Some(body) => body,
    None if interactive => dialoguer::Editor::new().edit("")?.unwrap_or_default(),
    None => String::new(),
  };

  let action = workspace.add_change(title, &body, &args.packages, args.breaking)?;
  ActionRunner::new(&workspace.root, args.dry_run)
    .run(&[action])
    .await?;
  Ok(Outcome::Done)
}
