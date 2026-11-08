use crate::changelog::{self, ChangelogEntry};
use crate::config::{
  ResolvedChangelogConfig, ResolvedGitConfig, ResolvedPackageConfig, ResolvedPackagesConfig,
};
use crate::fs_util;
use crate::manifest::{Manifest, ManifestKind};
use crate::plan::{PackagePlan, Plan};
use crate::registry::Registries;
use anyhow::{Context, Result, anyhow, ensure};
use relative_path::{RelativePath, RelativePathBuf};
use semver::Version;
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

type ManifestGroup = BTreeMap<PathBuf, ManifestGroupData>;

#[derive(Clone, Debug)]
struct ManifestGroupData {
  package_path: RelativePathBuf,
  /// Listed in `manifests`, so it must be usable. Discovered files
  /// without a version are skipped instead.
  from_config: bool,
}

/// Two packages are the same when they are at the same path.
#[derive(Clone, Debug)]
pub struct Package {
  path: PathBuf,
  relative_path: RelativePathBuf,
  name: String,
  manifests: Vec<Manifest>,
  config: ResolvedPackageConfig,
}

impl PartialEq for Package {
  fn eq(&self, other: &Self) -> bool {
    self.relative_path == other.relative_path
  }
}

impl Eq for Package {}

impl PartialOrd for Package {
  fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
    Some(self.cmp(other))
  }
}

impl Ord for Package {
  fn cmp(&self, other: &Self) -> Ordering {
    self.relative_path.cmp(&other.relative_path)
  }
}

impl Package {
  pub fn path(&self) -> &Path {
    &self.path
  }

  pub fn relative_path(&self) -> &RelativePath {
    &self.relative_path
  }

  pub fn name(&self) -> &str {
    &self.name
  }

  pub fn manifests(&self) -> &[Manifest] {
    &self.manifests
  }

  /// The manifests a release publishes.
  pub fn publishable_manifests(&self) -> impl Iterator<Item = &Manifest> {
    self
      .manifests
      .iter()
      .filter(|manifest| self.publishes(manifest))
  }

  /// Whether a release publishes `manifest`: it is public, and so is the package.
  pub fn publishes(&self, manifest: &Manifest) -> bool {
    !self.config.skip_publish && manifest.can_publish()
  }

  pub fn config(&self) -> &ResolvedPackageConfig {
    &self.config
  }

  /// The identifier used in config-like files: the path, with `.` for the root.
  pub fn id(&self) -> String {
    match self.relative_path.as_str() {
      "" => ".".to_string(),
      path => path.to_string(),
    }
  }

  pub fn matches(&self, input: &str) -> bool {
    self.id() == input
      || self.name == input
      || self
        .manifests
        .iter()
        .any(|manifest| manifest.name() == input)
  }

  /// The version of the package's top-level manifest.
  pub fn version(&self) -> &Version {
    self.manifests[0].version()
  }

  pub fn tag(&self, config: &ResolvedGitConfig, version: &Version) -> String {
    self
      .config
      .release_tag_format
      .as_ref()
      .unwrap_or(&config.release_tag_format)
      .replace("{package_name}", &self.name)
      .replace("{version}", &version.to_string())
  }

  /// What `plan` adds to the changelogs for this package.
  pub fn changelog_entries(
    &self,
    root: &Path,
    plan: &Plan,
    config: &ResolvedChangelogConfig,
    registries: &Registries,
  ) -> Vec<ChangelogEntry> {
    let Some(section) = self.changelog_section(plan, config, registries) else {
      return vec![];
    };
    let Some(planned) = plan.get(self) else {
      return vec![];
    };
    let [own, shared] = self.changelog_locations(root, config, planned.next_version());
    let shared = (config.root && shared.0 != own.0).then_some(shared);
    [Some(own), shared]
      .into_iter()
      .flatten()
      .map(|(path, heading)| ChangelogEntry {
        path,
        heading,
        section: section.clone(),
      })
      .collect()
  }

  /// The body of this package's changelog section, if `plan` releases it.
  pub fn changelog_section(
    &self,
    plan: &Plan,
    config: &ResolvedChangelogConfig,
    registries: &Registries,
  ) -> Option<String> {
    let planned = plan.get(self)?;
    let links = config
      .show_registries
      .then(|| self.registry_links(planned, registries))
      .flatten();
    Some(changelog::render_section(
      config,
      &planned.changes,
      &plan.dependency_updates(planned),
      links.as_deref(),
    ))
  }

  /// The notes of the current version, for its GitHub release.
  pub fn release_notes(&self, root: &Path, config: &ResolvedChangelogConfig) -> Result<String> {
    for (path, heading) in self.changelog_locations(root, config, self.version()) {
      let raw = fs_util::read_to_string_optional(&path)?.unwrap_or_default();
      if let Some(notes) = changelog::extract_section(&raw, &heading) {
        return Ok(notes);
      }
    }
    anyhow::bail!(
      "Missing release notes for {} v{}",
      self.name,
      self.version()
    )
  }

  /// Where a version is recorded: the package's changelog, then the root one.
  fn changelog_locations(
    &self,
    root: &Path,
    config: &ResolvedChangelogConfig,
    version: &Version,
  ) -> [(PathBuf, String); 2] {
    [
      (
        self.path.join(&config.filename),
        changelog::package_heading(version),
      ),
      (
        root.join(&config.root_filename),
        changelog::root_heading(&self.name, version),
      ),
    ]
  }

  fn registry_links(&self, planned: &PackagePlan, registries: &Registries) -> Option<String> {
    let links = planned
      .manifests
      .iter()
      .filter(|(manifest, _)| self.publishes(manifest))
      .map(|(manifest, version)| {
        let url = registries.url(manifest.kind(), manifest.name(), version);
        format!("[`{}`]({url})", manifest.name())
      })
      .collect::<Vec<_>>();
    (!links.is_empty()).then(|| links.join(", "))
  }

  pub fn load_all(root: &Path, config: &ResolvedPackagesConfig) -> Result<Vec<Package>> {
    let mut group: BTreeMap<RelativePathBuf, Vec<Manifest>> = BTreeMap::new();

    for (file_path, group_data) in Self::group_manifests(root, config)? {
      let data =
        std::fs::read(&file_path).map_err(|e| anyhow!("Failed to read manifest file: {}", e))?;

      match Manifest::parse(&file_path, data)? {
        Some(manifest_file) => group
          .entry(group_data.package_path)
          .or_default()
          .push(manifest_file),
        None => ensure!(
          !group_data.from_config,
          "Manifest {} of package \"{}\" has no \"version\" field",
          file_path.display(),
          group_data.package_path,
        ),
      }
    }

    for key in config.overrides.keys() {
      ensure!(
        group.contains_key(&RelativePath::new(key).normalize()),
        "Configured package \"{key}\" has no usable manifest"
      );
    }

    let mut packages = vec![];

    for (relative_path, mut manifests) in group {
      // Top-level manifests of the package come first.
      manifests.sort_by_key(|m| (m.path().components().count(), m.path().to_owned()));

      let package_config = config
        .overrides
        .iter()
        .find(|(key, _)| RelativePath::new(key).normalize() == relative_path)
        .map(|(_, config)| config.clone())
        .unwrap_or_default();
      let name = Self::determine_name(&relative_path, &package_config);
      let package = Self {
        path: relative_path.to_path(root),
        relative_path,
        name,
        manifests,
        config: package_config,
      };
      packages.push(package);
    }

    Ok(packages)
  }

  fn group_manifests(root: &Path, config: &ResolvedPackagesConfig) -> Result<ManifestGroup> {
    let files = fs_util::find_files_recursively(
      root,
      &ManifestKind::file_names(),
      Some(&config.includes),
      Some(&config.excludes),
    )?;

    // Configured files win over discovered ones: `npm/*/package.json` of
    // `packages/node` must not stay a package of its own.
    let mut configured_group = Self::discover_configured_group(root, config)?;
    let configured = configured_group
      .values()
      .map(|owner| owner.package_path.clone())
      .collect::<HashSet<_>>();

    for file in files {
      let relative = file
        .strip_prefix(root)
        .with_context(|| format!("Failed to resolve path: {}", file.display()))?;
      let relative_dir = relative.parent().unwrap_or(Path::new(""));
      let relative_dir = RelativePathBuf::from_path(relative_dir)
        .with_context(|| format!("Failed to resolve path: {}", file.display()))?;
      if !configured.contains(&relative_dir) {
        configured_group.entry(file).or_insert(ManifestGroupData {
          package_path: relative_dir,
          from_config: false,
        });
      }
    }

    Ok(configured_group)
  }

  fn discover_configured_group(
    root: &Path,
    config: &ResolvedPackagesConfig,
  ) -> Result<ManifestGroup> {
    let mut group = ManifestGroup::new();

    for (key, pkg_config) in &config.overrides {
      let relative_path = RelativePath::new(key).normalize();
      let package_dir = relative_path.to_path(root);

      for pattern in &pkg_config.manifests {
        let found = fs_util::find_files_recursively(
          &package_dir,
          &ManifestKind::file_names(),
          Some(&vec![pattern.clone()]),
          None,
        )?;
        ensure!(
          !found.is_empty(),
          "Manifest pattern \"{pattern}\" of package \"{key}\" matches no file"
        );

        for file in found {
          let owner = ManifestGroupData {
            package_path: relative_path.clone(),
            from_config: true,
          };
          if let Some(previous) = group.insert(file.clone(), owner) {
            ensure!(
              previous.package_path == relative_path,
              "Manifest {} is duplicated in packages \"{}\" and \"{relative_path}\"",
              file.display(),
              previous.package_path,
            );
          }
        }
      }
    }
    Ok(group)
  }

  fn determine_name(relative_path: &RelativePathBuf, config: &ResolvedPackageConfig) -> String {
    if let Some(name) = &config.name {
      return name.to_owned();
    }

    let name = relative_path.to_string();
    match name.is_empty() {
      true => "root".to_string(),
      false => name,
    }
  }
}
