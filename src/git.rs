use anyhow::{Context, Result, ensure};
use std::{
  path::{Path, PathBuf},
  process::Command,
};

#[derive(Clone, Debug)]
pub struct Git {
  pub root: PathBuf,
}

#[derive(Clone, Debug)]
pub struct Commit {
  pub sha: String,
  pub summary: String,
  pub body: String,
  pub author_name: String,
  pub author_email: String,
  pub committer_name: String,
  pub committer_email: String,
}

impl Git {
  pub fn new(root: &Path) -> Self {
    Self {
      root: root.to_path_buf(),
    }
  }
  pub fn run(&self, args: &[&str]) -> Result<String> {
    run(&self.root, args)
  }

  pub fn resolve(&self, reference: &str) -> Result<String> {
    Ok(
      self
        .run(&[
          "rev-parse",
          "--verify",
          "--end-of-options",
          &format!("{reference}^{{commit}}"),
        ])?
        .trim()
        .into(),
    )
  }
  pub fn branch(&self) -> Result<String> {
    Ok(
      self
        .run(&["symbolic-ref", "--quiet", "--short", "HEAD"])
        .context("A checked-out branch is required")?
        .trim()
        .into(),
    )
  }
  pub fn clean(&self) -> Result<bool> {
    Ok(self.run(&["status", "--porcelain"])?.is_empty())
  }

  pub fn shallow(&self) -> Result<bool> {
    Ok(self.run(&["rev-parse", "--is-shallow-repository"])?.trim() == "true")
  }

  pub fn parents(&self, sha: &str) -> Result<Vec<String>> {
    Ok(
      self
        .run(&["rev-list", "--parents", "-n", "1", sha])?
        .split_whitespace()
        .skip(1)
        .map(str::to_owned)
        .collect(),
    )
  }

  pub fn tag_exists(&self, tag: &str) -> bool {
    self
      .run(&[
        "show-ref",
        "--verify",
        "--quiet",
        &format!("refs/tags/{tag}"),
      ])
      .is_ok()
  }

  pub fn has_remote(&self, remote: &str) -> bool {
    self.run(&["remote", "get-url", remote]).is_ok()
  }

  pub fn commits(&self, from: Option<&str>, to: &str) -> Result<Vec<Commit>> {
    let range = from.map_or_else(|| to.to_owned(), |from| format!("{from}..{to}"));
    self
      .run(&["rev-list", "--topo-order", &range, "--"])?
      .lines()
      .map(|sha| self.commit(sha))
      .collect()
  }

  pub fn commit(&self, sha: &str) -> Result<Commit> {
    let output = self.run(&[
      "show",
      "-s",
      "--format=%H%x00%s%x00%b%x00%an%x00%ae%x00%cn%x00%ce",
      sha,
      "--",
    ])?;
    let fields: Vec<_> = output.trim_end_matches('\n').split('\0').collect();
    ensure!(fields.len() == 7, "cannot parse commit {sha}");
    Ok(Commit {
      sha: fields[0].into(),
      summary: fields[1].into(),
      body: fields[2].into(),
      author_name: fields[3].into(),
      author_email: fields[4].into(),
      committer_name: fields[5].into(),
      committer_email: fields[6].into(),
    })
  }

  pub fn changed_paths(&self, sha: &str) -> Result<Vec<PathBuf>> {
    let parents = self.parents(sha)?;
    let raw = if let Some(parent) = parents.first() {
      self.run(&[
        "diff",
        "--relative",
        "--name-only",
        "--no-renames",
        "-z",
        parent,
        sha,
        "--",
      ])?
    } else {
      self.run(&[
        "diff-tree",
        "--relative",
        "--root",
        "--no-commit-id",
        "--name-only",
        "--no-renames",
        "-r",
        "-z",
        sha,
        "--",
      ])?
    };
    Ok(
      raw
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect(),
    )
  }
}

fn run(root: &Path, args: &[&str]) -> Result<String> {
  let output = Command::new("git")
    .current_dir(root)
    .args(args)
    .env("GIT_TERMINAL_PROMPT", "0")
    .env("GIT_LITERAL_PATHSPECS", "1")
    .output()
    .context("Failed to execute git")?;
  ensure!(
    output.status.success(),
    "git {} failed: {}",
    args.first().unwrap_or(&""),
    String::from_utf8_lossy(&output.stderr).trim()
  );
  String::from_utf8(output.stdout).context("Git returned non-UTF-8 data")
}
