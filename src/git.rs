use anyhow::{Context, Result, ensure};
use std::{
  io::Write,
  path::{Path, PathBuf},
  process::{Command, Stdio},
};

#[derive(Clone)]
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
  pub fn discover() -> Result<Self> {
    let root = run(Path::new("."), &["rev-parse", "--show-toplevel"], None, &[])?;
    Ok(Self {
      root: PathBuf::from(root.trim()),
    })
  }
  pub fn run(&self, args: &[&str]) -> Result<String> {
    run(&self.root, args, None, &[])
  }
  pub fn input(&self, args: &[&str], input: &str, env: &[(&str, &str)]) -> Result<String> {
    run(&self.root, args, Some(input), env)
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
        .context("prepare requires a checked-out branch")?
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
  pub fn read_at(&self, reference: &str, path: &Path) -> Result<Option<String>> {
    let tree_path = format!("{reference}:{}", path.to_string_lossy());
    let output = Command::new("git")
      .current_dir(&self.root)
      .args(["cat-file", "-e", &tree_path])
      .output()?;
    if !output.status.success() {
      return Ok(None);
    }
    Ok(Some(self.run(&["show", &tree_path])?))
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
  pub fn latest_tag(&self, format: &str, name: &str, to: &str) -> Result<Option<String>> {
    let pattern = format.replace("{package_name}", name);
    let (prefix, suffix) = pattern
      .split_once("{version}")
      .context("missing version in tag format")?;
    let mut tags = self
      .run(&["tag", "--merged", to])?
      .lines()
      .filter_map(|tag| {
        let version = tag.strip_prefix(prefix)?.strip_suffix(suffix)?;
        Some((semver::Version::parse(version).ok()?, tag.to_owned()))
      })
      .collect::<Vec<_>>();
    tags.sort_by(|a, b| a.0.cmp_precedence(&b.0));
    Ok(tags.pop().map(|(_, tag)| tag))
  }
}

fn run(root: &Path, args: &[&str], input: Option<&str>, env: &[(&str, &str)]) -> Result<String> {
  let mut cmd = Command::new("git");
  cmd
    .current_dir(root)
    .args(args)
    .env("GIT_TERMINAL_PROMPT", "0")
    .env("GIT_LITERAL_PATHSPECS", "1");
  for (key, value) in env {
    cmd.env(key, value);
  }
  cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
  if input.is_some() {
    cmd.stdin(Stdio::piped());
  }
  let mut child = cmd.spawn().context("cannot execute git")?;
  if let Some(input) = input {
    child
      .stdin
      .take()
      .context("missing git stdin")?
      .write_all(input.as_bytes())?;
  }
  let output = child.wait_with_output()?;
  ensure!(
    output.status.success(),
    "git {} failed: {}",
    args.first().unwrap_or(&""),
    String::from_utf8_lossy(&output.stderr).trim()
  );
  String::from_utf8(output.stdout).context("Git returned non-UTF-8 data")
}
