use crate::{
  action::Edits,
  config::ChangelogConfig,
  git::{Commit, Git},
  github::Github,
  manifest::Manifest,
  package::Package,
  registry::Registry,
};
use anyhow::{Context, Result, ensure};
use semver::Version;
use serde::{Deserialize, Serialize};
use std::{
  collections::BTreeSet,
  fs,
  path::{Path, PathBuf},
};

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Frontmatter {
  pub title: String,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub commit: Option<String>,
  #[serde(default)]
  pub breaking: bool,
}

pub struct Note {
  pub path: PathBuf,
  pub meta: Frontmatter,
  pub body: String,
  pub commit: String,
}

pub fn load(git: &Git) -> Result<Vec<Note>> {
  let dir = git.root.join(".prp/changes");
  if !dir.exists() {
    return Ok(vec![]);
  }
  let mut paths = fs::read_dir(dir)?
    .map(|e| e.map(|e| e.path()))
    .collect::<std::io::Result<Vec<_>>>()?;
  paths.sort();
  let mut notes = Vec::new();
  for full in paths {
    if full.extension().is_none_or(|e| e != "md") || !full.is_file() {
      continue;
    }
    let path = full.strip_prefix(&git.root)?.to_owned();
    let raw = fs::read_to_string(&full)?.replace("\r\n", "\n");
    let (front, body) = raw
      .strip_prefix("---\n")
      .and_then(|s| s.split_once("\n---\n"))
      .with_context(|| format!("{} must have YAML frontmatter", path.display()))?;
    let meta: Frontmatter = serde_yaml_ng::from_str(front)
      .with_context(|| format!("invalid frontmatter in {}", path.display()))?;
    ensure!(
      !meta.title.trim().is_empty() && !meta.title.contains(['\n', '\r']),
      "invalid title in {}",
      path.display()
    );
    let commit = if let Some(reference) = &meta.commit {
      git.resolve(reference)?
    } else {
      let history = git.run(&[
        "log",
        "--diff-filter=A",
        "--format=%H",
        "--",
        path.to_str().context("non-UTF-8 note path")?,
      ])?;
      history
        .lines()
        .next()
        .with_context(|| {
          format!(
            "{} is uncommitted; commit the note or provide its commit field",
            path.display()
          )
        })?
        .into()
    };
    notes.push(Note {
      path,
      meta,
      body: body.trim().into(),
      commit,
    });
  }
  Ok(notes)
}

pub fn render(
  config: &ChangelogConfig,
  commits: &[Commit],
  notes: &[Note],
  dependency_notes: &[String],
  github: Option<&Github>,
) -> (String, BTreeSet<PathBuf>) {
  let mut body = String::new();
  let mut consumed = BTreeSet::new();
  for note in notes {
    if commits.iter().any(|c| c.sha == note.commit) {
      let prefix = if note.meta.breaking {
        &config.breaking_prefix
      } else {
        ""
      };
      body.push_str(&format!(
        "### {prefix}{}\n\n{}\n\n",
        note.meta.title, note.body
      ));
      consumed.insert(note.path.clone());
    }
  }
  if !dependency_notes.is_empty() {
    body.push_str("### Dependencies\n\n");
    for note in dependency_notes {
      body.push_str(&format!("- {note}\n"));
    }
    body.push('\n');
  }
  if config.show_commits && !commits.is_empty() {
    body.push_str("### Commits\n\n");
    for commit in commits {
      let pr = if config
        .commit_templates
        .iter()
        .any(|s| s.contains("{github_"))
      {
        github.and_then(|gh| gh.commit_pr(&commit.sha).ok().flatten())
      } else {
        None
      };
      let mut values = vec![
        ("summary", commit.summary.as_str()),
        ("body", commit.body.as_str()),
        ("author_name", commit.author_name.as_str()),
        ("author_email", commit.author_email.as_str()),
        ("committer_name", commit.committer_name.as_str()),
        ("committer_email", commit.committer_email.as_str()),
        ("sha", &commit.sha[..7]),
      ];
      if let Some((author, number)) = &pr {
        values.push(("github_pr_author_login", author));
        values.push(("github_pr_number", number));
      }
      let rendered = config
        .commit_templates
        .iter()
        .find_map(|template| {
          let mut text = template.clone();
          for (key, value) in &values {
            text = text.replace(&format!("{{{key}}}"), value);
          }
          if text.contains("{github_") {
            None
          } else {
            Some(text)
          }
        })
        .unwrap_or_else(|| format!("- {} ({})", commit.summary, &commit.sha[..7]));
      body.push_str(&rendered);
      body.push('\n');
    }
    body.push('\n');
  }
  (body.trim_end().to_owned(), consumed)
}

pub fn append(
  edits: &mut Edits,
  config: &ChangelogConfig,
  package: &Package,
  next: &Version,
  body: &str,
) -> Result<()> {
  let date = chrono::Utc::now().format("%Y-%m-%d");
  let local = package.path.join(&config.filename);
  let root = Path::new(&config.filename);
  if local != root || !config.root {
    let heading = format!("## v{next} ({date})");
    prepend(
      edits,
      &local,
      &heading,
      body,
      (next.major > package.version().major).then_some("## "),
    )?;
  }
  if config.root {
    let prefix = format!("## `{}` ", package.display_name);
    let heading = format!("{prefix}v{next} ({date})");
    prepend(
      edits,
      root,
      &heading,
      body,
      (next.major > package.version().major).then_some(prefix.as_str()),
    )?;
  }
  Ok(())
}

fn prepend(
  edits: &mut Edits,
  path: &Path,
  heading: &str,
  body: &str,
  remove_prefix: Option<&str>,
) -> Result<()> {
  let old = edits.read(path)?.unwrap_or_else(|| "# Changelog\n".into());
  ensure!(
    !old.lines().any(|line| line == heading),
    "changelog entry already exists in {}",
    path.display()
  );
  let mut kept = String::new();
  let mut removing = false;
  for line in old.lines() {
    if line.starts_with("## ") {
      removing = remove_prefix.is_some_and(|prefix| line.starts_with(prefix));
    }
    if !removing {
      kept.push_str(line);
      kept.push('\n');
    }
  }
  let position = kept.find("\n## ").map(|n| n + 1).unwrap_or(kept.len());
  let (preamble, tail) = kept.split_at(position);
  edits.write(
    path,
    format!("{}\n\n{heading}\n\n{body}\n\n{}", preamble.trim_end(), tail)
      .trim_end()
      .to_owned()
      + "\n",
  );
  Ok(())
}

pub fn release_body(root: &Path, config: &ChangelogConfig, package: &Package) -> Result<String> {
  let local = package.path.join(&config.filename);
  let raw = fs::read_to_string(root.join(&local))
    .or_else(|_| fs::read_to_string(root.join(&config.filename)))
    .unwrap_or_default();
  let local_prefix = format!("## v{} (", package.version());
  let root_prefix = format!("## `{}` v{} (", package.display_name, package.version());
  let mut inside = false;
  let mut out = String::new();
  for line in raw.lines() {
    if line.starts_with("## ") {
      if inside {
        break;
      }
      inside = line.starts_with(&local_prefix) || line.starts_with(&root_prefix);
      continue;
    }
    if inside {
      out.push_str(line);
      out.push('\n');
    }
  }
  Ok(out.trim().into())
}

pub fn with_registries(
  config: &ChangelogConfig,
  package: &Package,
  version: &Version,
  body: String,
) -> String {
  if !config.show_registries || package.config.skip_publish {
    return body;
  }
  let links = package
    .manifests
    .iter()
    .filter(|m| m.can_publish())
    .map(|m| {
      let registry = m.registry();
      format!(
        "[`{}` ({})]({})",
        m.name(),
        registry.label(),
        registry.url(m.name(), version)
      )
    })
    .collect::<Vec<_>>();
  if links.is_empty() {
    return body;
  }
  format!(
    "This release includes packages: {}\n\n{body}",
    links.join(", ")
  )
  .trim_end()
  .to_owned()
}
