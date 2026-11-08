use crate::action::Action;
use crate::cargo;
use crate::changelog::ChangelogEdits;
use crate::changes::Change;
use crate::config::ResolvedConfig;
use crate::dependency::DependencyVersion;
use crate::edit::{self, Patch};
use crate::fs_util;
use crate::git::Git;
use crate::manifest::{ManifestKind, ReleasedVersions};
use crate::package::Package;
use crate::plan::Plan;
use crate::registry::Registries;
use crate::release_record::ReleaseRecord;
use anyhow::{Context, Result, anyhow, ensure};
use serde_json::Value;
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

/// Packages adjacent to each package, by position in `Workspace::packages`.
type Adjacency = Vec<BTreeSet<usize>>;

#[derive(Debug)]
pub struct Workspace {
  pub root: PathBuf,
  pub root_cargo: Option<Value>,
  pub packages: Vec<Package>,
  pub config: ResolvedConfig,
  pub git: Git,
  dependencies: Adjacency,
  dependents: Adjacency,
}

impl Workspace {
  pub fn load(root: &Path, config: ResolvedConfig) -> Result<Self> {
    let packages = Package::load_all(root, &config.packages)?;
    ensure!(!packages.is_empty(), "Workspace has no packages");

    let root_cargo_path = root.join("Cargo.toml");
    let root_cargo_exists =
      std::fs::exists(&root_cargo_path).map_err(|e| anyhow!("Fail to read path: {e}"))?;
    let root_cargo = if root_cargo_exists {
      let data = cargo::read_cargo_toml(&root_cargo_path)?;
      Some(data)
    } else {
      None
    };

    let (dependencies, dependents) = build_graph(&packages, root_cargo.as_ref());
    let workspace = Self {
      root: root.to_path_buf(),
      root_cargo,
      packages,
      config,
      git: Git::new(root),
      dependencies,
      dependents,
    };

    Ok(workspace)
  }

  /// Moves the manifests of `plan`, and the versions pinned in the root
  /// `Cargo.toml`, to their next versions.
  pub fn version_actions(&self, plan: &Plan) -> Result<Vec<Action>> {
    let versions = plan.versions();
    let mut actions = vec![];
    for planned in &plan.packages {
      for (manifest, next) in &planned.manifests {
        actions.push(Action::FileWrite {
          path: manifest.path().to_path_buf(),
          patch: manifest.release_patch(next, &versions)?,
        });
      }
    }
    actions.extend(self.root_cargo_action(&actions, &versions)?);
    Ok(actions)
  }

  /// `manifest_actions` already cover the root `Cargo.toml` if it goes out.
  fn root_cargo_action(
    &self,
    manifest_actions: &[Action],
    versions: &ReleasedVersions,
  ) -> Result<Option<Action>> {
    let Some(root_cargo) = &self.root_cargo else {
      return Ok(None);
    };
    let path = self.root.join("Cargo.toml");
    let is_released = manifest_actions
      .iter()
      .any(|action| action.file_path() == Some(path.as_path()));
    let edits = cargo::dependency_edits(root_cargo, versions);
    if is_released || edits.is_empty() {
      return Ok(None);
    }
    let raw = std::fs::read_to_string(&path)
      .with_context(|| format!("Failed to read {}", path.display()))?;
    let patch = edit::edit_toml(&raw, &edits)?;
    Ok(Some(Action::FileWrite { path, patch }))
  }

  /// Adds the changelog entries of `plan` to the changelog files.
  pub fn changelog_actions(&self, plan: &Plan, registries: &Registries) -> Result<Vec<Action>> {
    let config = &self.config.changelog;
    let mut edits = ChangelogEdits::default();
    // Sections stack up at the top, so go backwards to keep dependencies first.
    for planned in plan.packages.iter().rev() {
      let package = planned.package;
      for entry in package.changelog_entries(&self.root, plan, config, registries) {
        edits.insert(&entry)?;
      }
    }
    edits.into_actions()
  }

  /// Records `plan` for `prp release` to carry out.
  pub fn record_action(&self, plan: &Plan, branch: &str) -> Result<Action> {
    let id = chrono::Local::now().format("%Y%m%d%H%M%S").to_string();
    Ok(Action::FileWrite {
      path: ReleaseRecord::path(&self.root, branch, &id),
      patch: Patch::new("", &ReleaseRecord::from(plan).to_toml())?,
    })
  }

  /// The change files waiting for a release.
  pub fn changes(&self) -> Result<Vec<Change>> {
    let changes = Change::load_all(&self.root)?;
    for change in &changes {
      for input in &change.packages {
        ensure!(
          self.find_package(input).is_some(),
          "{} refers to unknown package \"{input}\"",
          change.path.display()
        );
      }
    }
    Ok(changes)
  }

  /// Writes down a change. `packages` accepts whatever names a package.
  pub fn add_change(
    &self,
    title: String,
    body: &str,
    packages: &[String],
    breaking: bool,
  ) -> Result<Action> {
    let packages = packages
      .iter()
      .map(|input| {
        let input = input.trim();
        self
          .find_package(input)
          .map(Package::id)
          .with_context(|| format!("Unknown package \"{input}\""))
      })
      .collect::<Result<_>>()?;

    let timestamp = chrono::Local::now().format("%Y%m%d%H%M%S").to_string();
    let change = Change {
      path: Change::new_path(&self.root, &timestamp),
      title,
      packages,
      breaking,
      body: body.trim().to_string(),
    };
    Ok(Action::FileWrite {
      path: self.root.join(&change.path),
      patch: Patch::new("", &change.to_markdown()?)?,
    })
  }

  /// Removes the change files `plan` includes: a change file has done its job
  /// once a release carries it.
  pub fn consume_changes(&self, plan: &Plan) -> Vec<Action> {
    plan
      .packages
      .iter()
      .flat_map(|planned| &planned.changes.notes)
      .map(|note| self.root.join(&note.path))
      .collect::<BTreeSet<_>>()
      .into_iter()
      .map(|path| Action::FileRemove { path })
      .collect()
  }

  /// `packages.fixed`: packages released with one shared version.
  pub fn fixed_groups(&self) -> Result<PackageGroups<'_>> {
    PackageGroups::resolve(&self.packages, &self.config.packages.fixed)
  }

  /// `packages.linked`: packages sharing the highest bump when released together.
  pub fn linked_groups(&self) -> Result<PackageGroups<'_>> {
    PackageGroups::resolve(&self.packages, &self.config.packages.linked)
  }

  /// The package going by `input`: its path, its name or a manifest name.
  pub fn find_package(&self, input: &str) -> Option<&Package> {
    self.packages.iter().find(|package| package.matches(input))
  }

  pub fn index_of(&self, package: &Package) -> Option<usize> {
    self.packages.iter().position(|other| other == package)
  }

  /// Packages that `package` directly depends on.
  pub fn dependencies_of(&self, package: &Package) -> Vec<&Package> {
    self.index_of(package).map_or(vec![], |index| {
      self.dependencies[index]
        .iter()
        .map(|&dependency| &self.packages[dependency])
        .collect()
    })
  }

  /// Packages that directly depend on `package`.
  pub fn dependents_of(&self, package: &Package) -> Vec<&Package> {
    self.index_of(package).map_or(vec![], |index| {
      self.dependents[index]
        .iter()
        .map(|&dependent| &self.packages[dependent])
        .collect()
    })
  }

  /// Orders `subset` dependencies first. Dev dependencies may form cycles, whose
  /// members are then ordered arbitrarily among themselves.
  pub fn topo_sort<'a>(&self, subset: impl IntoIterator<Item = &'a Package>) -> Vec<&Package> {
    let subset = self.indices_of(subset);
    let mut visited = BTreeSet::new();
    let mut order = Vec::with_capacity(subset.len());
    for &index in &subset {
      self.visit(index, &subset, &mut visited, &mut order);
    }
    order
      .into_iter()
      .map(|index| &self.packages[index])
      .collect()
  }

  fn visit(
    &self,
    index: usize,
    subset: &BTreeSet<usize>,
    visited: &mut BTreeSet<usize>,
    order: &mut Vec<usize>,
  ) {
    if !visited.insert(index) {
      return;
    }
    for &dependency in self.dependencies[index].intersection(subset) {
      self.visit(dependency, subset, visited, order);
    }
    order.push(index);
  }

  fn indices_of<'a>(&self, packages: impl IntoIterator<Item = &'a Package>) -> BTreeSet<usize> {
    packages
      .into_iter()
      .filter_map(|package| self.index_of(package))
      .collect()
  }
}

/// `packages.fixed` or `packages.linked`, resolved to packages.
pub struct PackageGroups<'a>(Vec<BTreeSet<&'a Package>>);

impl<'a> PackageGroups<'a> {
  fn resolve(packages: &'a [Package], groups: &[Vec<String>]) -> Result<Self> {
    let groups = groups
      .iter()
      .map(|patterns| {
        let mut members = BTreeSet::new();
        for pattern in patterns {
          let set = fs_util::build_glob_set(std::slice::from_ref(pattern))?;
          let matched = packages
            .iter()
            .filter(|package| set.is_match(package.id()) || set.is_match(package.name()))
            .collect::<Vec<_>>();
          ensure!(
            !matched.is_empty(),
            "Package group entry \"{pattern}\" matches no package"
          );
          members.extend(matched);
        }
        Ok(members)
      })
      .collect::<Result<_>>()?;
    Ok(Self(groups))
  }

  pub fn len(&self) -> usize {
    self.0.len()
  }

  pub fn iter(&self) -> impl Iterator<Item = &BTreeSet<&'a Package>> {
    self.0.iter()
  }
}

/// An edge means a manifest depends on a name another package provides, in the
/// registry the dependency points at: an npm package and a crate are unrelated.
fn build_graph(packages: &[Package], root_cargo: Option<&Value>) -> (Adjacency, Adjacency) {
  let mut providers = HashMap::new();
  for (index, package) in packages.iter().enumerate() {
    for manifest in package.manifests() {
      providers.insert((manifest.kind(), manifest.name()), index);
    }
  }

  let mut dependencies = vec![BTreeSet::new(); packages.len()];
  let mut dependents = vec![BTreeSet::new(); packages.len()];
  for (index, package) in packages.iter().enumerate() {
    for manifest in package.manifests() {
      for dependency in manifest.dependencies() {
        let name = match (dependency.kind, &dependency.version) {
          (ManifestKind::CargoToml, DependencyVersion::Workspace) => {
            cargo::workspace_dependency_name(root_cargo, &dependency.name)
          }
          _ => &dependency.name,
        };
        if let Some(&dependency) = providers.get(&(dependency.kind, name))
          && dependency != index
        {
          dependencies[index].insert(dependency);
          dependents[dependency].insert(index);
        }
      }
    }
  }
  (dependencies, dependents)
}
