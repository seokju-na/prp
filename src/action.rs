use crate::edit::{self, Patch};
use crate::github::{GitHub, PullRequest, Release};
use crate::manifest::ManifestKind;
use crate::registry::{Registries, RegistryPublishResult};
use anyhow::{Context, Result, ensure};
use console::Style;
use semver::Version;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A mutation. Commands plan these instead of acting, so `--dry-run` can show
/// them without side effects.
#[derive(Debug, Clone)]
pub enum Action {
  FileWrite {
    path: PathBuf,
    patch: Patch,
  },
  FileRemove {
    path: PathBuf,
  },
  RunProcess {
    cmd: String,
    args: Vec<String>,
    cwd: PathBuf,
  },
  Publish {
    kind: ManifestKind,
    name: String,
    version: Version,
    dir: PathBuf,
    args: Vec<String>,
  },
  UpsertPullRequest(PullRequest),
  EnsureRelease(Release),
}

impl Action {
  /// The file this action changes.
  pub fn file_path(&self) -> Option<&Path> {
    match self {
      Self::FileWrite { path, .. } | Self::FileRemove { path } => Some(path),
      _ => None,
    }
  }

  /// One line saying what the action does, with paths relative to `root`.
  pub fn describe(&self, root: &Path) -> String {
    let (verb, subject) = self.summary(root);
    format!("{verb} {subject}")
  }

  fn summary(&self, root: &Path) -> (&'static str, String) {
    match self {
      Self::FileWrite { path, .. } => ("update", relative(root, path)),
      Self::FileRemove { path } => ("remove", relative(root, path)),
      Self::RunProcess { cmd, args, .. } => {
        let words = shell_words::join(std::iter::once(cmd).chain(args));
        ("run", words)
      }
      Self::Publish {
        name,
        version,
        args,
        ..
      } => (
        "publish",
        format!("{name}@{version} {}", shell_words::join(args)),
      ),
      Self::UpsertPullRequest(pr) => (
        "open pull request",
        format!("{} -> {}: {}", pr.head, pr.base, pr.title),
      ),
      Self::EnsureRelease(release) => ("create GitHub release", release.tag.clone()),
    }
  }

  fn verb_style(&self) -> Style {
    let style = Style::new().bold();
    match self {
      Self::FileWrite { .. } => style.yellow(),
      Self::FileRemove { .. } => style.red(),
      Self::RunProcess { .. } => style.magenta(),
      Self::Publish { .. } => style.green(),
      Self::UpsertPullRequest(_) | Self::EnsureRelease(_) => style.blue(),
    }
  }

  pub fn process(cmd: &str, args: &[&str], cwd: &Path) -> Self {
    Self::RunProcess {
      cmd: cmd.to_string(),
      args: args.iter().map(|arg| arg.to_string()).collect(),
      cwd: cwd.to_path_buf(),
    }
  }
}

pub struct ActionRunner<'a> {
  root: &'a Path,
  dry_run: bool,
  registries: Option<&'a Registries>,
  github: Option<&'a GitHub>,
}

impl<'a> ActionRunner<'a> {
  pub fn new(root: &'a Path, dry_run: bool) -> Self {
    Self {
      root,
      dry_run,
      registries: None,
      github: None,
    }
  }

  pub fn with_registries(mut self, registries: &'a Registries) -> Self {
    self.registries = Some(registries);
    self
  }

  /// Only needed outside dry runs, which never reach GitHub.
  pub fn with_github(mut self, github: Option<&'a GitHub>) -> Self {
    self.github = github;
    self
  }

  /// Runs `actions` in order, stopping at the first failure.
  pub async fn run(&self, actions: &[Action]) -> Result<()> {
    for action in actions {
      self.show(action);
      if !self.dry_run {
        self.apply(action).await?;
      }
    }
    Ok(())
  }

  fn show(&self, action: &Action) {
    print!("{}", self.render(action, console::colors_enabled()));
  }

  /// What `show` prints for `action`, in color when `colors`.
  fn render(&self, action: &Action, colors: bool) -> String {
    let paint = |style: Style, text: &str| style.force_styling(colors).apply_to(text).to_string();
    let prefix = match self.dry_run {
      true => paint(Style::new().dim(), "[dry-run] "),
      false => String::new(),
    };
    let (verb, subject) = action.summary(self.root);
    let mut out = format!("{prefix}{} {subject}\n", paint(action.verb_style(), verb));
    match action {
      Action::FileWrite { patch, .. } if self.dry_run => out.push_str(&patch.render(colors)),
      Action::UpsertPullRequest(pr) if self.dry_run => {
        out.push_str(&paint(Style::new().dim(), &pr.body));
        out.push('\n');
      }
      Action::EnsureRelease(release) => {
        for asset in &release.assets {
          let verb = paint(action.verb_style(), "upload");
          out.push_str(&format!("{prefix}{verb} {}\n", relative(self.root, asset)));
        }
      }
      _ => {}
    }
    out
  }

  async fn apply(&self, action: &Action) -> Result<()> {
    match action {
      Action::FileWrite { path, patch } => apply_patch(path, patch),
      Action::FileRemove { path } => {
        fs::remove_file(path).with_context(|| format!("Failed to remove {}", path.display()))
      }
      Action::RunProcess { cmd, args, cwd } => run_process(cmd, args, cwd),
      Action::Publish {
        kind,
        name,
        version,
        dir,
        args,
      } => {
        let registries = self.registries.context("Registries are not configured")?;
        match registries.publish(*kind, dir, args).await? {
          RegistryPublishResult::Success => Ok(()),
          RegistryPublishResult::Duplicated => {
            println!("{name}@{version} is already published");
            Ok(())
          }
          RegistryPublishResult::Error { status, message } => {
            anyhow::bail!(
              "Failed to publish {name}@{version} ({status}): {}",
              message.trim()
            )
          }
        }
      }
      Action::UpsertPullRequest(pr) => {
        let url = self.github()?.upsert_pull_request(pr).await?;
        println!("Pull request: {url}");
        Ok(())
      }
      Action::EnsureRelease(release) => {
        let url = self.github()?.ensure_release(release).await?;
        println!("GitHub release: {url}");
        Ok(())
      }
    }
  }

  fn github(&self) -> Result<&GitHub> {
    self.github.context("GitHub is not configured")
  }
}

fn relative(root: &Path, path: &Path) -> String {
  path
    .strip_prefix(root)
    .unwrap_or(path)
    .display()
    .to_string()
}

fn apply_patch(path: &Path, patch: &Patch) -> Result<()> {
  let original = match fs::read_to_string(path) {
    Ok(raw) => raw,
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
    Err(error) => return Err(error).with_context(|| format!("Failed to read {}", path.display())),
  };
  let updated = edit::apply_patch(&original, patch)
    .with_context(|| format!("Failed to update {}", path.display()))?;
  if let Some(parent) = path.parent() {
    fs::create_dir_all(parent).with_context(|| format!("Failed to create {}", parent.display()))?;
  }
  fs::write(path, updated).with_context(|| format!("Failed to write {}", path.display()))
}

fn run_process(cmd: &str, args: &[String], cwd: &Path) -> Result<()> {
  let output = Command::new(cmd)
    .args(args)
    .current_dir(cwd)
    .output()
    .with_context(|| format!("Failed to execute {cmd}"))?;
  ensure!(
    output.status.success(),
    "{cmd} failed ({}): {}",
    output.status,
    String::from_utf8_lossy(&output.stderr).trim()
  );
  print!("{}", String::from_utf8_lossy(&output.stdout));
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  fn update() -> Action {
    Action::FileWrite {
      path: PathBuf::from("/repo/package.json"),
      patch: Patch::new("\"1.0.0\"\n", "\"1.1.0\"\n").unwrap(),
    }
  }

  #[test]
  fn a_dry_run_shows_the_action_and_its_diff() {
    let runner = ActionRunner::new(Path::new("/repo"), true);
    let plain = runner.render(&update(), false);
    assert!(
      plain.starts_with("[dry-run] update package.json\n"),
      "{plain}"
    );
    assert!(plain.contains("-\"1.0.0\"\n+\"1.1.0\""), "{plain}");
    assert!(!plain.contains('\x1b'), "{plain}");
  }

  #[test]
  fn colors_mark_the_verb_and_the_diff() {
    let runner = ActionRunner::new(Path::new("/repo"), true);
    let colored = runner.render(&update(), true);
    assert!(
      colored.contains("\x1b[33m\x1b[1mupdate\x1b[0m"),
      "{colored:?}"
    );
    assert!(colored.contains("\x1b[32m+\"1.1.0\""), "{colored:?}");
    assert!(colored.contains("\x1b[31m-\"1.0.0\""), "{colored:?}");
  }
}
