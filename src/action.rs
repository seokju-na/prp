use crate::git::Git;
use anyhow::{Context, Result, ensure};
use std::{
  collections::BTreeMap,
  fs,
  io::Write,
  path::{Path, PathBuf},
};

/// In-memory edits only. No release-plan file is written.
pub struct Edits {
  pub root: PathBuf,
  pub files: BTreeMap<PathBuf, Option<String>>,
}
impl Edits {
  pub fn new(root: &Path) -> Self {
    Self {
      root: root.to_owned(),
      files: BTreeMap::new(),
    }
  }
  pub fn read(&self, path: &Path) -> Result<Option<String>> {
    if let Some(value) = self.files.get(path) {
      return Ok(value.clone());
    }
    match fs::read_to_string(self.root.join(path)) {
      Ok(s) => Ok(Some(s)),
      Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
      Err(e) => Err(e.into()),
    }
  }
  pub fn write(&mut self, path: &Path, content: String) {
    self.files.insert(path.to_owned(), Some(content));
  }
  pub fn remove(&mut self, path: &Path) {
    self.files.insert(path.to_owned(), None);
  }
  pub fn show(&self) {
    for (path, content) in &self.files {
      println!(
        "{} {}",
        if content.is_some() { "write" } else { "remove" },
        path.display()
      );
    }
  }
  pub fn check_clean_targets(&self, git: &Git) -> Result<()> {
    for path in self.files.keys() {
      let current = fs::read_to_string(self.root.join(path)).ok();
      let committed = git.read_at("HEAD", path)?;
      ensure!(
        current == committed,
        "release would overwrite local changes in {}; commit or restore that file first",
        path.display()
      );
    }
    Ok(())
  }
  pub fn apply(&self) -> Result<()> {
    let mut originals = BTreeMap::new();
    for path in self.files.keys() {
      let full = self.root.join(path);
      let old = match fs::read(&full) {
        Ok(bytes) => Some(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.into()),
      };
      originals.insert(path.clone(), old);
    }
    for (path, content) in &self.files {
      if let Err(error) = replace(&self.root.join(path), content.as_deref().map(str::as_bytes)) {
        for (path, original) in originals {
          if let Err(restore) = replace(&self.root.join(&path), original.as_deref()) {
            eprintln!("cannot restore {}: {restore:#}", path.display());
          }
        }
        return Err(error);
      }
    }
    Ok(())
  }
  /// Build a commit through a temporary index, preserving the user's index and checkout.
  pub fn commit(&self, git: &Git, branch: &str, title: &str) -> Result<String> {
    git.run(&["check-ref-format", "--branch", branch])?;
    ensure!(
      git.branch()? != branch,
      "release branch must differ from the current branch"
    );
    let head = git.resolve("HEAD")?;
    let reference = format!("refs/heads/{branch}");
    ensure!(
      git.run(&["show-ref", "--verify", &reference]).is_err(),
      "release branch '{branch}' already exists; inspect it before preparing another release"
    );
    let temp = tempfile::tempdir()?;
    let index = temp.path().join("index");
    let index_str = index.to_str().context("non-UTF-8 temporary path")?;
    let env = [("GIT_INDEX_FILE", index_str)];
    git.input(&["read-tree", &head], "", &env)?;
    for (path, content) in &self.files {
      let path = path.to_str().context("non-UTF-8 file path")?;
      if let Some(content) = content {
        let blob = git.input(&["hash-object", "-w", "--stdin"], content, &[])?;
        let existing = git.run(&["ls-tree", &head, "--", path])?;
        let mode = existing.split_whitespace().next().unwrap_or("100644");
        ensure!(
          matches!(mode, "100644" | "100755"),
          "cannot edit non-regular file {path}"
        );
        git.input(
          &[
            "update-index",
            "--add",
            "--cacheinfo",
            mode,
            blob.trim(),
            path,
          ],
          "",
          &env,
        )?;
      } else {
        git.input(&["update-index", "--force-remove", "--", path], "", &env)?;
      }
    }
    let tree = git.input(&["write-tree"], "", &env)?;
    let commit = git.input(
      &["commit-tree", tree.trim(), "-p", &head],
      &format!("{title}\n"),
      &[],
    )?;
    git.run(&["update-ref", &reference, commit.trim(), ""])?;
    Ok(commit.trim().into())
  }
}

fn replace(path: &Path, content: Option<&[u8]>) -> Result<()> {
  if let Some(content) = content {
    let parent = path.parent().context("missing parent directory")?;
    fs::create_dir_all(parent)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.write_all(content)?;
    if let Ok(metadata) = fs::metadata(path) {
      temp.as_file().set_permissions(metadata.permissions())?;
    }
    temp
      .persist(path)
      .with_context(|| format!("cannot write {}", path.display()))?;
  } else if path.exists() {
    fs::remove_file(path)?;
  }
  Ok(())
}
