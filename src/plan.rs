use crate::bump::{self, BumpRule};
use crate::changes::{ChangeId, PackageChanges};
use crate::detection;
use crate::manifest::{Manifest, ReleasedVersions};
use crate::package::Package;
use crate::workspace::{PackageGroups, Workspace};
use anyhow::{Context, Result};
use semver::Version;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
  Changed,
  Dependency,
  Fixed,
}

/// A package going out in a release.
#[derive(Debug)]
pub struct PackagePlan<'a> {
  pub package: &'a Package,
  pub rule: BumpRule,
  pub reason: Reason,
  pub changes: PackageChanges,
  /// Where each manifest moves to, top-level manifest first.
  pub manifests: Vec<(&'a Manifest, Version)>,
  dependencies: Vec<&'a Package>,
}

impl PackagePlan<'_> {
  pub fn next_version(&self) -> &Version {
    &self.manifests[0].1
  }
}

/// What a release is going to do, dependencies first. Nothing changes until its
/// actions run, and they are built from its current content, so edits carry over.
#[derive(Debug)]
pub struct Plan<'a> {
  pub packages: Vec<PackagePlan<'a>>,
}

impl<'a> Plan<'a> {
  pub fn is_empty(&self) -> bool {
    self.packages.is_empty()
  }

  pub fn get(&self, package: &Package) -> Option<&PackagePlan<'a>> {
    self
      .packages
      .iter()
      .find(|planned| planned.package == package)
  }

  /// The versions going out, for whatever depends on them.
  pub fn versions(&self) -> ReleasedVersions {
    self
      .packages
      .iter()
      .flat_map(|planned| &planned.manifests)
      .map(|(manifest, version)| {
        let key = (manifest.kind(), manifest.name().to_string());
        (key, version.clone())
      })
      .collect()
  }

  /// The dependencies of `planned` going out with it.
  pub fn released_dependencies(&self, planned: &PackagePlan) -> Vec<&PackagePlan<'a>> {
    planned
      .dependencies
      .iter()
      .filter_map(|dependency| self.get(dependency))
      .collect()
  }

  /// Changelog entries for the dependencies going out with `planned`.
  pub fn dependency_updates(&self, planned: &PackagePlan) -> Vec<String> {
    self
      .released_dependencies(planned)
      .into_iter()
      .flat_map(|dependency| &dependency.manifests)
      .map(|(manifest, version)| format!("update dependencies: {}@{version}", manifest.name()))
      .collect()
  }
}

impl fmt::Display for Plan<'_> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    for planned in &self.packages {
      writeln!(
        f,
        "[{}] {} -> {} ({})",
        planned.package.name(),
        planned.package.version(),
        planned.next_version(),
        planned.rule.name()
      )?;
      for note in &planned.changes.notes {
        writeln!(f, "  * {}", note.title)?;
      }
      for commit in &planned.changes.commits {
        writeln!(f, "  - {} {}", &commit.sha[..7], commit.summary)?;
      }
      for update in self.dependency_updates(planned) {
        writeln!(f, "  - {update}")?;
      }
    }
    Ok(())
  }
}

/// What the user changed about a package's release.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Decision {
  pub skip: bool,
  /// Replaces the rule its changes ask for.
  pub rule: Option<BumpRule>,
  pub excluded: BTreeSet<ChangeId>,
}

#[derive(Debug, Clone, Default)]
pub struct Decisions<'a>(BTreeMap<&'a Package, Decision>);

impl<'a> Decisions<'a> {
  pub fn get(&self, package: &Package) -> Option<&Decision> {
    self.0.get(package)
  }

  pub fn edit(&mut self, package: &'a Package) -> &mut Decision {
    self.0.entry(package).or_default()
  }
}

/// Detects the changes once, then plans a release for any decisions about them.
pub struct Planner<'a> {
  workspace: &'a Workspace,
  detected: Vec<PackageChanges>,
  fixed: PackageGroups<'a>,
  linked: PackageGroups<'a>,
}

#[derive(Debug, Clone)]
struct Choice {
  rule: BumpRule,
  pinned: bool,
}

impl Choice {
  /// A 0.x version takes breaking changes as minor bumps, unless chosen by hand.
  fn applied_to(&self, version: &Version) -> BumpRule {
    match self.pinned {
      true => self.rule.clone(),
      false => self.rule.clone().for_version(version),
    }
  }
}

impl<'a> Planner<'a> {
  pub fn new(workspace: &'a Workspace) -> Result<Self> {
    Ok(Self {
      workspace,
      detected: detection::detect_changes(workspace)?,
      fixed: workspace.fixed_groups()?,
      linked: workspace.linked_groups()?,
    })
  }

  pub fn workspace(&self) -> &'a Workspace {
    self.workspace
  }

  /// Every change detected for `package`, excluded or not.
  pub fn changes_of(&self, package: &Package) -> &PackageChanges {
    &self.detected[self.index(package)]
  }

  /// `package` and the packages sharing its version.
  pub fn fixed_with(&self, package: &'a Package) -> Vec<&'a Package> {
    self
      .fixed
      .iter()
      .find(|group| group.contains(package))
      .map_or(vec![package], |group| group.iter().copied().collect())
  }

  pub fn plan(&self, decisions: &Decisions<'a>) -> Result<Plan<'a>> {
    let mut changes = self
      .workspace
      .packages
      .iter()
      .zip(&self.detected)
      .map(|(package, detected)| match decisions.get(package) {
        Some(decision) => detected.without(&decision.excluded),
        None => detected.clone(),
      })
      .collect::<Vec<_>>();
    let selected = self.select(decisions);
    let choices = self.choose_rules(&selected, &changes, decisions);
    let fixed_versions = self.fixed_versions(&choices)?;

    let packages = self
      .workspace
      .topo_sort(selected.iter().copied())
      .into_iter()
      .map(|package| {
        let rule = choices[package].applied_to(package.version());
        let manifests = package
          .manifests()
          .iter()
          .map(|manifest| {
            let version = match fixed_versions.get(package) {
              Some(version) => version.clone(),
              None => bump::bump(manifest.version(), &rule)?,
            };
            Ok((manifest, version))
          })
          .collect::<Result<_>>()?;
        Ok(PackagePlan {
          package,
          rule,
          reason: self.reason(package, &selected),
          changes: std::mem::take(&mut changes[self.index(package)]),
          manifests,
          dependencies: self.workspace.dependencies_of(package),
        })
      })
      .collect::<Result<_>>()?;
    Ok(Plan { packages })
  }

  fn index(&self, package: &Package) -> usize {
    self
      .workspace
      .index_of(package)
      .expect("packages come from the workspace")
  }

  /// Changed packages, their dependents and fixed groups, short of what is
  /// skipped. Skipping one member of a fixed group skips the group.
  fn select(&self, decisions: &Decisions<'a>) -> BTreeSet<&'a Package> {
    let packages = &self.workspace.packages;
    let mut skipped = packages
      .iter()
      .filter(|package| decisions.get(package).is_some_and(|decision| decision.skip))
      .collect::<BTreeSet<_>>();
    for group in self.fixed.iter() {
      if !group.is_disjoint(&skipped) {
        skipped.extend(group);
      }
    }

    let mut selected = packages
      .iter()
      .zip(&self.detected)
      .filter(|(package, detected)| !detected.is_empty() && !skipped.contains(package))
      .map(|(package, _)| package)
      .collect::<BTreeSet<_>>();
    loop {
      let before = selected.len();
      let dependents = selected
        .iter()
        .flat_map(|&package| self.workspace.dependents_of(package))
        .collect::<Vec<_>>();
      selected.extend(dependents);
      for group in self.fixed.iter() {
        if !group.is_disjoint(&selected) {
          selected.extend(group);
        }
      }
      selected.retain(|package| !skipped.contains(package));
      if selected.len() == before {
        return selected;
      }
    }
  }

  /// A fixed group follows its pinned member, or else its highest rule; linked
  /// groups lift their members that are not pinned to their highest rule.
  fn choose_rules(
    &self,
    selected: &BTreeSet<&'a Package>,
    changes: &[PackageChanges],
    decisions: &Decisions<'a>,
  ) -> BTreeMap<&'a Package, Choice> {
    let mut choices = selected
      .iter()
      .map(|&package| {
        let choice = match decisions
          .get(package)
          .and_then(|decision| decision.rule.clone())
        {
          Some(rule) => Choice { rule, pinned: true },
          None => Choice {
            rule: changes[self.index(package)]
              .rule()
              .unwrap_or(BumpRule::Patch),
            pinned: false,
          },
        };
        (package, choice)
      })
      .collect::<BTreeMap<_, _>>();

    for group in self.fixed.iter() {
      let pinned = group
        .iter()
        .filter_map(|package| choices.get(package))
        .find(|choice| choice.pinned)
        .cloned();
      let shared = pinned.or_else(|| {
        highest_rule(group, &choices).map(|rule| Choice {
          rule,
          pinned: false,
        })
      });
      if let Some(shared) = shared {
        for package in group {
          if let Some(choice) = choices.get_mut(package) {
            *choice = shared.clone();
          }
        }
      }
    }
    for group in self.linked.iter() {
      if let Some(rule) = highest_rule(group, &choices) {
        for package in group {
          if let Some(choice) = choices.get_mut(package)
            && !choice.pinned
          {
            choice.rule = rule.clone();
          }
        }
      }
    }
    choices
  }

  /// A fixed group shares one version: its highest one, bumped.
  fn fixed_versions(
    &self,
    choices: &BTreeMap<&'a Package, Choice>,
  ) -> Result<BTreeMap<&'a Package, Version>> {
    let mut versions = BTreeMap::new();
    for group in self.fixed.iter() {
      let Some(choice) = group.iter().find_map(|package| choices.get(package)) else {
        continue;
      };
      let highest = group
        .iter()
        .map(|package| package.version())
        .max_by(|a, b| a.cmp_precedence(b))
        .context("A package group must not be empty")?;
      let next = bump::bump(highest, &choice.applied_to(highest))?;
      for package in group {
        versions.insert(*package, next.clone());
      }
    }
    Ok(versions)
  }

  fn reason(&self, package: &Package, selected: &BTreeSet<&'a Package>) -> Reason {
    if !self.changes_of(package).is_empty() {
      Reason::Changed
    } else if self
      .workspace
      .dependencies_of(package)
      .iter()
      .any(|dependency| selected.contains(dependency))
    {
      Reason::Dependency
    } else {
      Reason::Fixed
    }
  }
}

fn highest_rule(
  group: &BTreeSet<&Package>,
  choices: &BTreeMap<&Package, Choice>,
) -> Option<BumpRule> {
  group
    .iter()
    .filter_map(|package| choices.get(package))
    .map(|choice| choice.rule.clone())
    .reduce(BumpRule::max)
}
