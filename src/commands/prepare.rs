mod editor;
mod view;

use crate::action::{Action, ActionRunner};
use crate::changelog;
use crate::commands::Outcome;
use crate::fs_util;
use crate::git::Git;
use crate::github::{GitHub, PullRequest};
use crate::plan::{Decisions, Plan, Planner};
use crate::registry::Registries;
use crate::workspace::Workspace;
use anyhow::{Context, Result, ensure};
use clap::Args;
use editor::{Effect, PlanEditor};
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use std::io::IsTerminal;

#[derive(Args, Default)]
pub struct PrepareArgs {
  /// Release what the commits ask for, without asking.
  #[arg(long)]
  pub auto: bool,
  /// Write the release files without committing them.
  #[arg(long)]
  pub skip_commit: bool,
  /// Commit to a local release branch without pushing or opening a pull request.
  #[arg(long)]
  pub skip_pr: bool,
  #[arg(long)]
  pub draft_pr: bool,
  /// Run with uncommitted changes in the working tree.
  #[arg(long)]
  pub allow_dirty: bool,
  /// Show what would happen without changing anything.
  #[arg(long)]
  pub dry_run: bool,
  #[arg(long, env = "GITHUB_TOKEN", hide_env_values = true)]
  pub github_token: Option<String>,
}

pub async fn prepare(workspace: &Workspace, args: PrepareArgs) -> Result<Outcome> {
  ensure!(
    args.auto || (std::io::stdin().is_terminal() && std::io::stdout().is_terminal()),
    "prepare asks what to release in a terminal; use --auto to release what the commits ask for"
  );
  let git = &workspace.git;
  ensure!(
    !git.shallow()?,
    "prepare requires the full Git history; fetch with --unshallow and --tags"
  );
  let branch = git.branch()?;
  ensure_release_branch(workspace, &branch)?;
  ensure!(
    args.dry_run || args.allow_dirty || git.clean()?,
    "Working tree is dirty; commit your changes or use --allow-dirty"
  );
  let opens_pull_request = !args.dry_run && !args.skip_commit && !args.skip_pr;
  let github = opens_pull_request
    .then(|| {
      let token = args.github_token.as_deref();
      let config = &workspace.config;
      GitHub::new(&workspace.root, &config.github, &config.http, token)
    })
    .transpose()?;
  if opens_pull_request {
    sync_with_origin(git, &branch)?;
  }

  let planner = Planner::new(workspace)?;
  let plan = planner.plan(&Decisions::default())?;
  if plan.is_empty() {
    println!("No changes to prepare.");
    return Ok(Outcome::NoChanges);
  }

  let registries = Registries::new(&workspace.config.registry, &workspace.config.http)?;
  let publishing = Publishing {
    workspace,
    args: &args,
    head: git.resolve("HEAD")?,
    base: &branch,
  };
  let actions = |plan: &Plan| publishing.actions(plan, &registries);
  let plan = match args.auto {
    true => plan,
    false => {
      let title = format!("prp prepare · {branch}");
      let review = |plan: &Plan| review_lines(workspace, plan, &actions(plan)?);
      match edit(PlanEditor::new(&planner, plan, &registries), &title, review)? {
        Some(plan) => plan,
        None => {
          println!("Cancelled; nothing changed.");
          return Ok(Outcome::NoChanges);
        }
      }
    }
  };
  print!("{plan}");
  ActionRunner::new(&workspace.root, args.dry_run)
    .with_github(github.as_ref())
    .run(&actions(&plan)?)
    .await?;
  Ok(Outcome::Done)
}

/// Runs `editor` until the plan is applied (`Some`) or the user quits (`None`).
fn edit<'a>(
  editor: PlanEditor<'a>,
  title: &str,
  review: impl Fn(&Plan) -> Result<Vec<String>>,
) -> Result<Option<Plan<'a>>> {
  let mut terminal = ratatui::try_init()?;
  let result = run_editor(&mut terminal, editor, title, review);
  ratatui::restore();
  result
}

fn run_editor<'a>(
  terminal: &mut DefaultTerminal,
  mut editor: PlanEditor<'a>,
  title: &str,
  review: impl Fn(&Plan) -> Result<Vec<String>>,
) -> Result<Option<Plan<'a>>> {
  loop {
    terminal.draw(|frame| view::render(frame, &editor, title))?;
    let Event::Key(key) = event::read()? else {
      continue;
    };
    if key.kind != KeyEventKind::Press {
      continue;
    }
    match editor.handle(key) {
      Effect::None => {}
      Effect::Review => {
        let lines = review(editor.plan())?;
        editor.review(lines);
      }
      Effect::Apply => return Ok(Some(editor.into_plan())),
      Effect::Quit => return Ok(None),
    }
  }
}

fn review_lines(workspace: &Workspace, plan: &Plan, actions: &[Action]) -> Result<Vec<String>> {
  let mut lines = plan
    .to_string()
    .lines()
    .map(String::from)
    .collect::<Vec<_>>();
  lines.push(String::new());
  lines.extend(
    actions
      .iter()
      .map(|action| action.describe(&workspace.root)),
  );
  Ok(lines)
}

/// With `git.release_branches` configured, only those and the default branch
/// can release. Otherwise the current branch is the release branch.
fn ensure_release_branch(workspace: &Workspace, branch: &str) -> Result<()> {
  let config = &workspace.config.git;
  if config.release_branches.is_empty() || branch == config.default_branch {
    return Ok(());
  }
  ensure!(
    fs_util::build_glob_set(&config.release_branches)?.is_match(branch),
    "{branch} is not a configured release branch"
  );
  Ok(())
}

/// The release is based on HEAD, so it has to be what the pull request targets.
fn sync_with_origin(git: &Git, branch: &str) -> Result<()> {
  ensure!(
    git.has_remote("origin"),
    "A remote \"origin\" is required to open a release pull request"
  );
  let refspec = format!("+refs/heads/{branch}:refs/remotes/origin/{branch}");
  git
    .run(&["fetch", "--quiet", "--tags", "origin", &refspec])
    .context("Failed to fetch the release branch and tags")?;
  ensure!(
    git.resolve(&format!("refs/remotes/origin/{branch}"))? == git.resolve("HEAD")?,
    "HEAD is not the tip of origin/{branch}; pull before preparing a release"
  );
  Ok(())
}

/// Getting the release files onto a branch and in front of reviewers.
struct Publishing<'a> {
  workspace: &'a Workspace,
  args: &'a PrepareArgs,
  head: String,
  base: &'a str,
}

impl Publishing<'_> {
  fn title(&self, plan: &Plan) -> String {
    let config = &self.workspace.config.github;
    config
      .release_pr_title_format
      .replace("{num}", &plan.packages.len().to_string())
  }

  fn branch(&self) -> String {
    let config = &self.workspace.config.github;
    config
      .release_pr_branch_format
      .replace("{branch}", self.base)
      .replace("{commit}", &self.head[..7])
  }

  fn body(&self, plan: &Plan) -> String {
    let mut body = String::from("Generated by `prp prepare`.\n");
    for planned in &plan.packages {
      let name = planned.package.name();
      let heading = changelog::root_heading(name, planned.next_version());
      body.push_str(&format!("\n{heading}\n\n"));
      let commits = planned.changes.commits.iter();
      let summaries = commits.map(|commit| commit.summary.clone());
      for line in summaries.chain(plan.dependency_updates(planned)) {
        body.push_str(&format!("- {line}\n"));
      }
    }
    body
  }

  fn actions(&self, plan: &Plan, registries: &Registries) -> Result<Vec<Action>> {
    let workspace = self.workspace;
    let mut files = workspace.version_actions(plan)?;
    files.extend(workspace.changelog_actions(plan, registries)?);
    files.extend(workspace.consume_changes(plan));
    files.push(workspace.record_action(plan, self.base)?);
    Ok(self.wrap(files, plan))
  }

  /// Surrounds `files` with the git and GitHub steps the arguments ask for.
  fn wrap(&self, files: Vec<Action>, plan: &Plan) -> Vec<Action> {
    if self.args.skip_commit {
      return files;
    }
    let root = self.workspace.root.as_path();
    let branch = self.branch();
    let title = self.title(plan);
    let git = |args: &[&str]| Action::process("git", args, root);

    let touched = files
      .iter()
      .filter_map(Action::file_path)
      .map(|path| path.strip_prefix(root).unwrap_or(path).to_string_lossy())
      .collect::<Vec<_>>();
    let mut add = vec!["add", "--all", "--"];
    add.extend(touched.iter().map(AsRef::as_ref));

    let mut actions = vec![git(&["switch", "--quiet", "-C", &branch])];
    actions.extend(files.iter().cloned());
    actions.push(git(&add));
    actions.push(git(&["commit", "--quiet", "--message", &title]));
    if !self.args.skip_pr {
      // The branch only ever holds this one release commit, so a refresh replaces it.
      actions.push(git(&["push", "--quiet", "--force", "origin", &branch]));
      actions.push(Action::UpsertPullRequest(PullRequest {
        base: self.base.to_string(),
        head: branch,
        body: self.body(plan),
        title,
        draft: self.args.draft_pr,
        labels: self.workspace.config.github.release_pr_labels.clone(),
      }));
    }
    actions.push(git(&["switch", "--quiet", self.base]));
    actions
  }
}
