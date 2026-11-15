use crate::action::{Action, ActionRunner};
use crate::commands::Outcome;
use crate::manifest::{Manifest, ManifestKind};
use crate::package::Package;
use crate::registry::Registries;
use crate::snapshot::Snapshot;
use crate::workspace::Workspace;
use anyhow::{Context, Result, bail};
use clap::Args;

#[derive(Args, Default)]
pub struct SnapshotArgs {
  /// Overrides `snapshot.prerelease_format`, such as "canary-<sha>-<yyyymmdd>".
  pub prerelease_format: Option<String>,
  /// Publish npm packages under this dist-tag instead of the default one.
  #[arg(long)]
  pub dist_tag: Option<String>,
  /// Show what would happen without changing anything.
  #[arg(long)]
  pub dry_run: bool,
}

pub async fn snapshot(workspace: &Workspace, args: SnapshotArgs) -> Result<Outcome> {
  let config = &workspace.config.snapshot;
  let head = workspace.git.resolve("HEAD")?;
  let format = args
    .prerelease_format
    .as_deref()
    .unwrap_or(&config.prerelease_format);
  let prerelease = Snapshot::prerelease(format, &head[..7], None)?;
  let snapshot = Snapshot::new(workspace, &prerelease);

  let publishing = workspace
    .packages
    .iter()
    .filter(|package| package.publishable_manifests().next().is_some());
  let order = workspace.topo_sort(publishing);
  if order.is_empty() {
    println!("No package to publish");
    return Ok(Outcome::NoChanges);
  }
  let dist_tag = args
    .dist_tag
    .as_deref()
    .or(config.npm_dist_tag.as_deref())
    .or_else(|| Snapshot::default_dist_tag(format));
  let npm_dist_tag = npm_dist_tag(&order, dist_tag)?;

  let manifests = snapshot
    .manifests
    .iter()
    .map(|(manifest, version)| (*manifest, version));
  let mut actions = workspace.bump_version_actions(manifests, &snapshot.versions())?;
  for package in order {
    actions.extend(package.before_publish_actions()?);
    for manifest in package.publishable_manifests() {
      actions.push(publish_action(&snapshot, manifest, npm_dist_tag)?);
    }
  }

  let registries = Registries::new(&workspace.config.registry, &workspace.config.http)?;
  ActionRunner::new(&workspace.root, args.dry_run)
    .with_registries(&registries)
    .run(&actions)
    .await?;
  Ok(Outcome::Done)
}

/// A snapshot must never become `latest`, so npm packages need an explicit dist-tag.
fn npm_dist_tag<'a>(order: &[&Package], dist_tag: Option<&'a str>) -> Result<Option<&'a str>> {
  let has_npm = order
    .iter()
    .flat_map(|package| package.publishable_manifests())
    .any(|manifest| manifest.kind() == ManifestKind::PackageJson);
  match (has_npm, dist_tag) {
    (false, _) => Ok(None),
    (true, Some(tag)) => Ok(Some(tag)),
    (true, None) => bail!("Pass --dist-tag or set snapshot.npm_dist_tag to publish npm packages"),
  }
}

fn publish_action(
  snapshot: &Snapshot,
  manifest: &Manifest,
  npm_dist_tag: Option<&str>,
) -> Result<Action> {
  let (_, version) = snapshot
    .manifests
    .iter()
    .find(|(candidate, _)| candidate.path() == manifest.path())
    .with_context(|| format!("{} has no snapshot version", manifest.name()))?;
  let args = match (manifest.kind(), npm_dist_tag) {
    (ManifestKind::PackageJson, Some(tag)) => vec!["--tag".to_string(), tag.to_string()],
    _ => vec![],
  };
  Ok(Action::Publish {
    kind: manifest.kind(),
    name: manifest.name().to_string(),
    version: version.clone(),
    dir: manifest.dir().to_path_buf(),
    args,
  })
}
