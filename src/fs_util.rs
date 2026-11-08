use anyhow::{Context, Result};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

pub fn read_to_string_optional(path: &Path) -> Result<Option<String>> {
  match std::fs::read_to_string(path) {
    Ok(raw) => Ok(Some(raw)),
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
    Err(e) => Err(e).with_context(|| format!("Failed to read file: {}", path.display())),
  }
}

#[derive(Debug, PartialEq, Eq, Hash)]
struct FoundFilesKey {
  root: PathBuf,
  includes: Vec<String>,
  excludes: Vec<String>,
}

static FOUND_FILES: LazyLock<Mutex<HashMap<FoundFilesKey, HashSet<PathBuf>>>> =
  LazyLock::new(Default::default);

pub fn find_files_recursively(
  root: &Path,
  file_names: &[&str],
  includes: Option<&Vec<String>>,
  excludes: Option<&Vec<String>>,
) -> Result<Vec<PathBuf>> {
  let key = FoundFilesKey {
    root: root.to_path_buf(),
    includes: includes.cloned().unwrap_or_default(),
    excludes: excludes.cloned().unwrap_or_default(),
  };

  let mut cache = FOUND_FILES.lock().unwrap();
  let files = match cache.entry(key) {
    Entry::Occupied(entry) => entry.into_mut(),
    Entry::Vacant(entry) => {
      let files = walk_files(entry.key())?;
      entry.insert(files)
    }
  };

  let mut found = files
    .iter()
    .filter(|path| {
      path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| file_names.contains(&name))
    })
    .cloned()
    .collect::<Vec<_>>();
  found.sort();
  Ok(found)
}

fn walk_files(key: &FoundFilesKey) -> Result<HashSet<PathBuf>> {
  let includes = (!key.includes.is_empty())
    .then(|| build_glob_set(&key.includes))
    .transpose()?;
  let excludes = build_glob_set(&key.excludes)?;
  let root = key.root.clone();

  let mut files = HashSet::new();
  let walker = ignore::WalkBuilder::new(&key.root)
    .git_ignore(true)
    .require_git(false)
    .git_global(false)
    .git_exclude(false)
    .filter_entry(move |entry| {
      if entry.depth() == 0 {
        return true;
      }
      let relative = entry.path().strip_prefix(&root).unwrap_or(entry.path());
      if entry.file_type().is_some_and(|t| t.is_dir()) {
        return !matches!(entry.file_name().to_str(), Some("node_modules" | "target"))
          && !excludes.is_match(relative);
      }
      includes
        .as_ref()
        .is_none_or(|set| is_glob_match(set, relative))
        && !is_glob_match(&excludes, relative)
    })
    .build();
  for entry in walker {
    let entry = entry.with_context(|| format!("Failed to walk {}", key.root.display()))?;
    if entry.file_type().is_some_and(|t| t.is_file()) {
      files.insert(entry.into_path());
    }
  }
  Ok(files)
}

pub fn build_glob_set(patterns: &[String]) -> Result<GlobSet> {
  let mut builder = GlobSetBuilder::new();
  for pattern in patterns {
    builder.add(
      GlobBuilder::new(pattern.strip_prefix("./").unwrap_or(pattern))
        .literal_separator(true)
        .build()
        .with_context(|| format!("Invalid glob: {pattern}"))?,
    );
  }
  builder.build().context("Failed to build glob set")
}

/// Matches the file itself or its directory, so `packages/*` selects
/// `packages/a/package.json`. The root directory is matched as `.`.
fn is_glob_match(set: &GlobSet, relative: &Path) -> bool {
  let dir = relative
    .parent()
    .filter(|dir| !dir.as_os_str().is_empty())
    .unwrap_or(Path::new("."));
  set.is_match(relative) || set.is_match(dir)
}

/// Files under `dir` matching `pattern`, including ignored build output such as
/// release assets.
pub fn find_glob_files(dir: &Path, pattern: &str) -> Result<Vec<PathBuf>> {
  let set = build_glob_set(&[pattern.to_string()])?;
  let walker = walkdir::WalkDir::new(dir)
    .into_iter()
    .filter_entry(|entry| !matches!(entry.file_name().to_str(), Some(".git" | "node_modules")));
  let mut found = vec![];
  for entry in walker {
    let entry = entry.with_context(|| format!("Failed to walk {}", dir.display()))?;
    let relative = entry.path().strip_prefix(dir).unwrap_or(entry.path());
    if entry.file_type().is_file() && set.is_match(relative) {
      found.push(entry.into_path());
    }
  }
  found.sort();
  Ok(found)
}
