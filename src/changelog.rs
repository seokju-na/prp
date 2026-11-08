use crate::action::Action;
use crate::changes::PackageChanges;
use crate::config::ResolvedChangelogConfig;
use crate::edit::Patch;
use crate::fs_util;
use crate::git::Commit;
use anyhow::{Context, Result, ensure};
use semver::Version;
use std::collections::BTreeMap;
use std::path::PathBuf;

const TITLE: &str = "# Changelog\n";

/// A section going into a changelog file.
#[derive(Debug)]
pub struct ChangelogEntry {
  pub path: PathBuf,
  pub heading: String,
  pub section: String,
}

/// Changelog files being edited, as they were and as they will be.
#[derive(Default)]
pub struct ChangelogEdits {
  files: BTreeMap<PathBuf, (String, String)>,
}

impl ChangelogEdits {
  pub fn insert(&mut self, entry: &ChangelogEntry) -> Result<()> {
    let path = &entry.path;
    if !self.files.contains_key(path) {
      let raw = fs_util::read_to_string_optional(path)?.unwrap_or_default();
      self.files.insert(path.clone(), (raw.clone(), raw));
    }
    let (_, updated) = self
      .files
      .get_mut(path)
      .context("Changelog is loaded above")?;
    *updated = insert_section(updated, &entry.heading, &entry.section)
      .with_context(|| format!("Failed to update {}", path.display()))?;
    Ok(())
  }

  pub fn into_actions(self) -> Result<Vec<Action>> {
    self
      .files
      .into_iter()
      .map(|(path, (original, updated))| {
        let patch = Patch::new(&original, &updated)?;
        Ok(Action::FileWrite { path, patch })
      })
      .collect()
  }
}

pub fn package_heading(version: &Version) -> String {
  format!("## v{version}")
}

pub fn root_heading(name: &str, version: &Version) -> String {
  format!("## {name} v{version}")
}

/// Renders the body of a release: notes, then the list of changes. `extra`
/// holds entries that have no commit of their own, such as dependency updates.
pub fn render_section(
  config: &ResolvedChangelogConfig,
  changes: &PackageChanges,
  extra: &[String],
  registries: Option<&str>,
) -> String {
  let mut blocks = vec![];
  if let Some(registries) = registries {
    blocks.push(format!("This release includes packages: {registries}"));
  }

  let notes = changes
    .notes
    .iter()
    .map(|note| {
      let prefix = if note.breaking {
        config.breaking_prefix.as_str()
      } else {
        ""
      };
      match note.body.is_empty() {
        true => format!("### {prefix}{}", note.title),
        false => format!("### {prefix}{}\n\n{}", note.title, note.body),
      }
    })
    .collect::<Vec<_>>();
  let has_notes = !notes.is_empty();
  blocks.extend(notes);

  let mut lines = vec![];
  if config.show_commits {
    lines.extend(changes.commits.iter().map(|c| render_commit(config, c)));
  }
  lines.extend(extra.iter().map(|entry| format!("- {entry}")));
  if !lines.is_empty() {
    if has_notes {
      blocks.push("---".to_string());
    }
    blocks.push("Changes".to_string());
    blocks.push(lines.join("\n"));
  }
  blocks.join("\n\n")
}

fn render_commit(config: &ResolvedChangelogConfig, commit: &Commit) -> String {
  let sha = &commit.sha[..7.min(commit.sha.len())];
  let values = [
    ("summary", commit.summary.as_str()),
    ("body", commit.body.as_str()),
    ("author_name", commit.author_name.as_str()),
    ("author_email", commit.author_email.as_str()),
    ("committer_name", commit.committer_name.as_str()),
    ("committer_email", commit.committer_email.as_str()),
    ("sha", sha),
  ];
  config
    .commit_templates
    .iter()
    .map(|template| {
      values.iter().fold(template.clone(), |text, (key, value)| {
        text.replace(&format!("{{{key}}}"), value)
      })
    })
    // Templates are fallbacks: GitHub metadata may not be available.
    .find(|text| !text.contains("{github_"))
    .unwrap_or_else(|| format!("- {} ({sha})", commit.summary))
}

/// Adds a release section at the top of a changelog, creating one if needed.
fn insert_section(existing: &str, heading: &str, section: &str) -> Result<String> {
  let existing = match existing.trim().is_empty() {
    true => TITLE,
    false => existing,
  };
  ensure!(
    !existing.lines().any(|line| line.trim_end() == heading),
    "Changelog already has an entry \"{heading}\""
  );

  let split = match existing.strip_prefix("# ") {
    Some(_) => existing.find('\n').map_or(existing.len(), |end| end + 1),
    None => 0,
  };
  let (title, rest) = existing.split_at(split);
  let mut out = String::new();
  if !title.is_empty() {
    out.push_str(title.trim_end());
    out.push_str("\n\n");
  }
  out.push_str(heading);
  out.push('\n');
  if !section.is_empty() {
    out.push('\n');
    out.push_str(section);
    out.push('\n');
  }
  if !rest.trim().is_empty() {
    out.push('\n');
    out.push_str(rest.trim_start());
  }
  Ok(out)
}

/// The body of the section under `heading`.
pub fn extract_section(changelog: &str, heading: &str) -> Option<String> {
  let mut lines = changelog.lines();
  lines.find(|line| line.trim_end() == heading)?;
  let section = lines
    .take_while(|line| !line.starts_with("## "))
    .collect::<Vec<_>>();
  Some(section.join("\n").trim().to_string())
}
