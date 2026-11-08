use crate::commands::Outcome;
use crate::manifest::{Manifest, ManifestKind};
use crate::package::Package;
use crate::registry::Registries;
use crate::workspace::{PackageGroups, Workspace};
use anyhow::Result;
use console::{Style, measure_text_width};
use semver::Version;
use std::cmp::Ordering;
use std::collections::HashMap;
use tokio::task::JoinSet;

pub async fn status(workspace: &Workspace) -> Result<Outcome> {
  let fixed = workspace.fixed_groups()?;
  let linked = workspace.linked_groups()?;
  let registries = Registries::new(&workspace.config.registry, &workspace.config.http)?;
  let mut latest = latest_versions(workspace, &registries).await;

  let packages = workspace
    .packages
    .iter()
    .map(|package| PackageStatus {
      name: package.name().to_string(),
      scope: scope(package, &fixed, &linked),
      manifests: package
        .manifests()
        .iter()
        .map(|manifest| {
          let lookup = latest.remove(&key(manifest));
          ManifestStatus::new(package, manifest, &registries, lookup)
        })
        .collect(),
    })
    .collect();
  let report = Report {
    title: workspace_title(workspace, &fixed, &linked),
    packages,
  };
  print!("{}", report.render(console::colors_enabled()));
  Ok(Outcome::Done)
}

type Key = (ManifestKind, String);
type Lookup = Result<Option<Version>>;

fn key(manifest: &Manifest) -> Key {
  (manifest.kind(), manifest.name().to_string())
}

/// The latest version of every published manifest, looked up concurrently.
async fn latest_versions(workspace: &Workspace, registries: &Registries) -> HashMap<Key, Lookup> {
  let mut lookups = JoinSet::new();
  for package in &workspace.packages {
    for manifest in package.publishable_manifests() {
      let registries = registries.clone();
      let key = key(manifest);
      lookups.spawn(async move {
        let latest = registries.latest(key.0, &key.1).await;
        (key, latest)
      });
    }
  }
  let mut found = HashMap::new();
  while let Some(lookup) = lookups.join_next().await {
    // A lookup that panicked has no key to report under; its manifest shows
    // up as not loaded.
    if let Ok((key, latest)) = lookup {
      found.insert(key, latest);
    }
  }
  found
}

fn scope(package: &Package, fixed: &PackageGroups, linked: &PackageGroups) -> Option<String> {
  let groups = [("fixed", fixed), ("linked", linked)];
  let labels = groups
    .iter()
    .flat_map(|(kind, groups)| {
      groups
        .iter()
        .enumerate()
        .filter(|(_, members)| members.contains(package))
        .map(move |(index, _)| format!("{kind} {}", index + 1))
    })
    .collect::<Vec<_>>();
  (!labels.is_empty()).then(|| labels.join(", "))
}

fn workspace_title(workspace: &Workspace, fixed: &PackageGroups, linked: &PackageGroups) -> String {
  let name = workspace.root.file_name().map_or_else(
    || workspace.root.display().to_string(),
    |name| name.to_string_lossy().to_string(),
  );
  let mut parts = vec![name, count(workspace.packages.len(), "package")];
  for (len, noun) in [(fixed.len(), "fixed group"), (linked.len(), "linked group")] {
    if len > 0 {
      parts.push(count(len, noun));
    }
  }
  parts.join(" · ")
}

fn count(len: usize, noun: &str) -> String {
  match len {
    1 => format!("1 {noun}"),
    _ => format!("{len} {noun}s"),
  }
}

/// Where a manifest stands against its registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Standing {
  UpToDate,
  Unreleased,
  BehindRegistry,
  NeverPublished,
  CouldNotLoad,
  Private,
}

impl Standing {
  const ALL: [Self; 6] = [
    Self::UpToDate,
    Self::Unreleased,
    Self::BehindRegistry,
    Self::NeverPublished,
    Self::CouldNotLoad,
    Self::Private,
  ];

  fn label(self) -> &'static str {
    match self {
      Self::UpToDate => "up to date",
      Self::Unreleased => "unreleased",
      Self::BehindRegistry => "behind registry",
      Self::NeverPublished => "never published",
      Self::CouldNotLoad => "could not load",
      Self::Private => "private",
    }
  }

  fn symbol(self) -> &'static str {
    match self {
      Self::UpToDate => "✓",
      Self::Unreleased => "↑",
      Self::BehindRegistry => "↓",
      Self::NeverPublished => "+",
      Self::CouldNotLoad => "✗",
      Self::Private => "·",
    }
  }

  fn style(self) -> Style {
    let style = Style::new();
    match self {
      Self::UpToDate => style.green(),
      Self::Unreleased => style.yellow(),
      Self::BehindRegistry => style.magenta(),
      Self::NeverPublished => style.cyan(),
      Self::CouldNotLoad => style.red(),
      Self::Private => style.dim(),
    }
  }
}

struct ManifestStatus {
  name: String,
  registry: &'static str,
  version: Version,
  latest: Option<Version>,
  standing: Standing,
  /// Why the lookup failed.
  error: Option<String>,
}

impl ManifestStatus {
  fn new(
    package: &Package,
    manifest: &Manifest,
    registries: &Registries,
    lookup: Option<Lookup>,
  ) -> Self {
    let version = manifest.version();
    let (latest, standing, error) = match lookup {
      _ if !package.publishes(manifest) => (None, Standing::Private, None),
      None => (
        None,
        Standing::CouldNotLoad,
        Some("lookup did not finish".into()),
      ),
      Some(Err(error)) => (None, Standing::CouldNotLoad, Some(format!("{error:#}"))),
      Some(Ok(None)) => (None, Standing::NeverPublished, None),
      Some(Ok(Some(latest))) => {
        let standing = match version.cmp_precedence(&latest) {
          Ordering::Equal => Standing::UpToDate,
          Ordering::Greater => Standing::Unreleased,
          Ordering::Less => Standing::BehindRegistry,
        };
        (Some(latest), standing, None)
      }
    };
    Self {
      name: manifest.name().to_string(),
      registry: registries.name(manifest.kind()),
      version: version.clone(),
      latest,
      standing,
      error,
    }
  }
}

struct PackageStatus {
  name: String,
  scope: Option<String>,
  manifests: Vec<ManifestStatus>,
}

struct Report {
  title: String,
  packages: Vec<PackageStatus>,
}

impl Report {
  fn render(&self, colors: bool) -> String {
    let paint = |text: &str, style: Style| style.force_styling(colors).apply_to(text).to_string();
    let dim = Style::new().dim();

    let mut table = Table::new(
      [
        "PACKAGE", "SCOPE", "MANIFEST", "REGISTRY", "VERSION", "LATEST", "STATUS",
      ],
      Style::new().bold(),
    );
    let mut failures = vec![];
    for status in &self.packages {
      for (index, manifest) in status.manifests.iter().enumerate() {
        let (package, scope) = match index {
          0 => (
            Cell::new(&status.name, Style::new().bold()),
            match &status.scope {
              Some(scope) => Cell::new(scope, Style::new().cyan()),
              None => Cell::new("-", dim.clone()),
            },
          ),
          _ => (Cell::plain(""), Cell::plain("")),
        };
        let latest = match (&manifest.latest, manifest.standing) {
          (Some(latest), _) => Cell::plain(&latest.to_string()),
          (None, Standing::CouldNotLoad) => Cell::new("?", dim.clone()),
          (None, _) => Cell::new("-", dim.clone()),
        };
        let standing = manifest.standing;
        let mut label = format!("{} {}", standing.symbol(), standing.label());
        if let Some(error) = &manifest.error {
          failures.push((&manifest.name, error));
          label.push_str(&format!(" [{}]", failures.len()));
        }
        table.push([
          package,
          scope,
          Cell::plain(&manifest.name),
          Cell::new(manifest.registry, dim.clone()),
          Cell::plain(&manifest.version.to_string()),
          latest,
          Cell::new(&label, standing.style()),
        ]);
      }
    }

    let mut out = format!("{}\n\n", paint(&self.title, Style::new().bold()));
    out.push_str(&table.render(colors));

    if !failures.is_empty() {
      let red = Style::new().red();
      out.push_str(&format!(
        "\n{}\n",
        paint("Could not load", red.clone().bold())
      ));
      for (index, (name, error)) in failures.iter().enumerate() {
        let reference = paint(&format!("[{}]", index + 1), red.clone());
        out.push_str(&format!("  {reference} {name}: {error}\n"));
      }
    }

    let summary = Standing::ALL
      .iter()
      .filter_map(|&standing| {
        let count = self
          .packages
          .iter()
          .flat_map(|status| &status.manifests)
          .filter(|manifest| manifest.standing == standing)
          .count();
        (count > 0).then(|| paint(&format!("{count} {}", standing.label()), standing.style()))
      })
      .collect::<Vec<_>>();
    out.push_str(&format!("\n{}\n", summary.join(&paint(" · ", dim))));
    out
  }
}

struct Cell {
  text: String,
  style: Style,
}

impl Cell {
  fn new(text: &str, style: Style) -> Self {
    Self {
      text: text.to_string(),
      style,
    }
  }

  fn plain(text: &str) -> Self {
    Self::new(text, Style::new())
  }
}

/// Columns as wide as their widest cell, two spaces apart.
struct Table<const N: usize> {
  rows: Vec<[Cell; N]>,
}

impl<const N: usize> Table<N> {
  fn new(header: [&str; N], style: Style) -> Self {
    Self {
      rows: vec![header.map(|title| Cell::new(title, style.clone()))],
    }
  }

  fn push(&mut self, row: [Cell; N]) {
    self.rows.push(row);
  }

  fn render(&self, colors: bool) -> String {
    let mut widths = [0; N];
    for row in &self.rows {
      for (width, cell) in widths.iter_mut().zip(row) {
        *width = (*width).max(measure_text_width(&cell.text));
      }
    }

    let mut out = String::new();
    for row in &self.rows {
      let mut line = String::new();
      for (column, (cell, width)) in row.iter().zip(widths).enumerate() {
        line.push_str(
          &cell
            .style
            .apply_to(&cell.text)
            .force_styling(colors)
            .to_string(),
        );
        if column + 1 < N {
          let padding = width - measure_text_width(&cell.text) + 2;
          line.push_str(&" ".repeat(padding));
        }
      }
      out.push_str(line.trim_end());
      out.push('\n');
    }
    out
  }
}
