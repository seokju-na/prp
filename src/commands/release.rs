use crate::action::{Action, ActionRunner};
use crate::commands::Outcome;
use crate::fs_util;
use crate::git::Git;
use crate::github::{GitHub, Release};
use crate::manifest::{Manifest, ManifestKind};
use crate::npm;
use crate::package::Package;
use crate::registry::Registries;
use crate::release_record::ReleaseRecord;
use crate::workspace::Workspace;
use anyhow::{Context, Result, anyhow, ensure};
use clap::Args;
use semver::Version;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use tokio::task::JoinSet;

#[derive(Args, Default)]
pub struct ReleaseArgs {
  /// The release to run. Defaults to the newest one matching the working directory.
  pub id: Option<String>,
  /// Publish npm packages under this dist-tag instead of the default one.
  #[arg(long)]
  pub dist_tag: Option<String>,
  /// Show what would happen without changing anything or querying registries.
  #[arg(long)]
  pub dry_run: bool,
  #[arg(long, env = "GITHUB_TOKEN", hide_env_values = true)]
  pub github_token: Option<String>,
}

type PublishKey = (ManifestKind, String, Version);

fn publish_key(manifest: &Manifest) -> PublishKey {
  (
    manifest.kind(),
    manifest.name().to_string(),
    manifest.version().clone(),
  )
}

pub async fn release(workspace: &Workspace, args: ReleaseArgs) -> Result<Outcome> {
  let git = &workspace.git;
  let packages = &workspace.packages;
  let branch = git.branch().ok();
  let (id, record) = ReleaseRecord::select(
    &workspace.root,
    branch.as_deref(),
    args.id.as_deref(),
    packages,
  )?;
  println!("Releasing {id}");

  let selected = record
    .resolve(packages)?
    .into_iter()
    .map(|(package, _)| package);
  let order = workspace.topo_sort(selected);

  let assets = resolve_all_assets(workspace, &order, args.dry_run)?;
  let github = connect_github(workspace, &args, !assets.is_empty())?;
  let registries = Registries::new(&workspace.config.registry, &workspace.config.http)?;
  let published = published_versions(&order, &registries, args.dry_run).await;

  let releaser = Releaser {
    workspace,
    git,
    runner: ActionRunner::new(&workspace.root, args.dry_run)
      .with_registries(&registries)
      .with_github(github.as_ref()),
    args: &args,
    published,
    assets,
  };

  let mut failed = BTreeSet::new();
  let mut report = vec![];
  for package in order {
    let blocked_by = workspace
      .dependencies_of(package)
      .into_iter()
      .filter(|dependency| failed.contains(dependency))
      .map(Package::name)
      .collect::<Vec<_>>();
    let result = match blocked_by.is_empty() {
      true => releaser.release(package).await,
      false => Err(anyhow!("Blocked by failed {}", blocked_by.join(", "))),
    };
    if let Err(error) = &result {
      eprintln!("[{}] {error:#}", package.name());
      failed.insert(package);
    }
    report.push((package, result.is_ok()));
  }

  println!();
  for (package, succeeded) in report {
    let status = if succeeded { "released" } else { "failed" };
    println!("{status}: {} v{}", package.name(), package.version());
  }
  ensure!(failed.is_empty(), "{} package(s) failed", failed.len());
  Ok(Outcome::Done)
}

/// Connects up front, so a missing token stops the release before it starts.
fn connect_github(
  workspace: &Workspace,
  args: &ReleaseArgs,
  needed: bool,
) -> Result<Option<GitHub>> {
  if args.dry_run || !needed {
    return Ok(None);
  }
  let config = &workspace.config;
  let token = args.github_token.as_deref();
  GitHub::new(&workspace.root, &config.github, &config.http, token).map(Some)
}

fn uses_github_release(workspace: &Workspace, package: &Package) -> bool {
  !workspace.config.github.skip_release && !package.config().skip_github_release
}

/// Assets of the packages with a GitHub release. Resolved first: a release must
/// not go out and only then find its assets missing.
fn resolve_all_assets<'a>(
  workspace: &Workspace,
  order: &[&'a Package],
  dry_run: bool,
) -> Result<BTreeMap<&'a Package, Vec<PathBuf>>> {
  let mut all_assets = BTreeMap::new();
  for &package in order {
    if !uses_github_release(workspace, package) {
      continue;
    }
    let mut assets = vec![];
    for pattern in &package.config().assets {
      let found = fs_util::find_glob_files(package.path(), pattern)?;
      ensure!(
        dry_run || !found.is_empty(),
        "Asset pattern \"{pattern}\" of package \"{}\" matches no file",
        package.name()
      );
      assets.extend(found);
    }
    all_assets.insert(package, assets);
  }
  Ok(all_assets)
}

/// Versions already in their registry. A failed lookup counts as missing, so the
/// publish either goes through or is rejected as a duplicate.
async fn published_versions(
  order: &[&Package],
  registries: &Registries,
  dry_run: bool,
) -> BTreeSet<PublishKey> {
  let mut published = BTreeSet::new();
  if dry_run {
    return published;
  }

  let mut lookups = JoinSet::new();
  for manifest in order
    .iter()
    .flat_map(|package| package.publishable_manifests())
  {
    let registries = registries.clone();
    let key = publish_key(manifest);
    lookups.spawn(async move {
      let (kind, name, version) = &key;
      let exists = registries.exists(*kind, name, version).await;
      (key, exists)
    });
  }
  while let Some(lookup) = lookups.join_next().await {
    match lookup {
      Ok((key, Ok(true))) => {
        published.insert(key);
      }
      Ok((_, Ok(false))) => {}
      Ok(((_, name, version), Err(error))) => {
        eprintln!("warning: cannot look up {name}@{version}: {error:#}");
      }
      Err(error) => eprintln!("warning: a registry lookup did not finish: {error}"),
    }
  }
  published
}

/// Releases packages one at a time. Every step skips what is already done, so
/// running a failed release again only does what is missing.
struct Releaser<'a> {
  workspace: &'a Workspace,
  git: &'a Git,
  runner: ActionRunner<'a>,
  args: &'a ReleaseArgs,
  published: BTreeSet<PublishKey>,
  assets: BTreeMap<&'a Package, Vec<PathBuf>>,
}

impl Releaser<'_> {
  async fn release(&self, package: &Package) -> Result<()> {
    let tag = package.tag(&self.workspace.config.git, package.version());

    let mut actions = self.publish_actions(package)?;
    actions.extend(self.tag_actions(&tag));
    if let Some(assets) = self.assets.get(package) {
      actions.push(Action::EnsureRelease(Release {
        title: format!("{} v{}", package.name(), package.version()),
        body: package.release_notes(&self.workspace.root, &self.workspace.config.changelog)?,
        prerelease: !package.version().pre.is_empty(),
        assets: assets.clone(),
        tag,
      }));
    }
    self.runner.run(&actions).await
  }

  fn publish_actions(&self, package: &Package) -> Result<Vec<Action>> {
    let pending = package
      .publishable_manifests()
      .filter(|manifest| !self.published.contains(&publish_key(manifest)))
      .collect::<Vec<_>>();
    if pending.is_empty() {
      return Ok(vec![]);
    }

    let mut actions = package
      .config()
      .before_publish_scripts
      .iter()
      .map(|script| {
        let mut words = shell_words::split(&script.command)
          .with_context(|| format!("Invalid script: {}", script.command))?;
        ensure!(!words.is_empty(), "Script of {} is empty", package.name());
        let cmd = words.remove(0);
        words.extend(script.args.iter().cloned());
        Ok(Action::RunProcess {
          cmd,
          args: words,
          cwd: package.path().to_path_buf(),
        })
      })
      .collect::<Result<Vec<_>>>()?;
    actions.extend(pending.into_iter().map(|manifest| Action::Publish {
      kind: manifest.kind(),
      name: manifest.name().to_string(),
      version: manifest.version().clone(),
      dir: manifest.dir().to_path_buf(),
      args: self.publish_args(manifest),
    }));
    Ok(actions)
  }

  fn publish_args(&self, manifest: &Manifest) -> Vec<String> {
    if manifest.kind() != ManifestKind::PackageJson {
      return vec![];
    }
    npm::dist_tag(manifest.version())
      .or(self.args.dist_tag.as_deref())
      .map_or(vec![], |tag| vec!["--tag".to_string(), tag.to_string()])
  }

  fn tag_actions(&self, tag: &str) -> Vec<Action> {
    let root = self.workspace.root.as_path();
    let mut actions = vec![];
    if !self.git.tag_exists(tag) {
      actions.push(Action::process("git", &["tag", tag], root));
    }
    let tag_ref = format!("refs/tags/{tag}");
    actions.push(Action::process(
      "git",
      &["push", "--quiet", "origin", &tag_ref],
      root,
    ));
    actions
  }
}
