use crate::{
  artifacts, changelog,
  cli::Release,
  config::Config,
  dependency::Graph,
  git::Git,
  github::Github,
  http_util::HttpClient,
  manifest::{Manifest, Manifests},
  package::Package,
  registry::{self, Registry},
};
use anyhow::{Context, Result, ensure};
use std::{
  collections::{BTreeMap, BTreeSet},
  fs,
};

pub fn run(git: &Git, config: &Config, packages: &[Package], args: Release) -> Result<()> {
  ensure!(
    !git.shallow()?,
    "release requires full Git history; fetch with --unshallow and --tags"
  );
  let head = git.resolve("HEAD")?;
  let parents = git.parents(&head)?;
  let Some(parent) = parents.first() else {
    println!("HEAD has no parent; nothing to release.");
    return Ok(());
  };
  let mut selected = BTreeSet::new();
  for (idx, package) in packages.iter().enumerate() {
    for manifest in &package.manifests {
      let committed = git.read_at(&head, manifest.path())?;
      ensure!(
        committed.as_deref() == Some(fs::read_to_string(git.root.join(manifest.path()))?.as_str()),
        "{} differs from HEAD; release requires committed manifests",
        manifest.path().display()
      );
      let previous = Manifests::read(manifest.path(), &|p| git.read_at(parent, p))?;
      if previous.is_some_and(|m| manifest.version().cmp_precedence(m.version()).is_gt()) {
        selected.insert(idx);
      }
    }
  }
  if selected.is_empty() {
    println!("No versions increased in HEAD; nothing to release.");
    return Ok(());
  }
  git
    .run(&["diff", "--quiet", "HEAD", "--"])
    .context("release requires committed source files; commit or restore tracked changes first")?;
  let graph = Graph::new(packages);
  let order = graph.order(&selected, packages)?;
  let mut tags = BTreeMap::new();
  for idx in &order {
    let package = &packages[*idx];
    ensure!(
      package
        .manifests
        .iter()
        .all(|m| m.version() == package.version()),
      "{} has inconsistent manifest versions",
      package.name
    );
    let tag = package.tag(config, package.version());
    git.run(&["check-ref-format", &format!("refs/tags/{tag}")])?;
    ensure!(
      git
        .run(&["show-ref", "--verify", &format!("refs/tags/{tag}")])
        .is_err(),
      "tag {tag} already exists; tag/GitHub recovery is not supported in 0.1.0"
    );
    ensure!(
      !tags.values().any(|t| t == &tag),
      "duplicate release tag {tag}"
    );
    tags.insert(*idx, tag);
    for manifest in &package.manifests {
      if manifest.can_publish() && !package.config.skip_publish {
        manifest
          .registry()
          .command(&config.packages, manifest.version())?;
      }
    }
    manifest_order(&package.manifests)?;
    for script in &package.config.before_publish_scripts {
      ensure!(
        !shell_words::split(&script.command)?.is_empty(),
        "{}: empty before_publish command",
        package.name
      );
    }
  }
  let gh_enabled = !args.skip_github_release && !config.github.skip_releases;
  let github = if !args.dry_run
    && gh_enabled
    && order
      .iter()
      .any(|i| !packages[*i].config.skip_github_release)
  {
    Some(Github::new(
      &git.root,
      &config.github,
      &config.http,
      args.github_token,
    )?)
  } else {
    None
  };
  if !args.dry_run {
    if !args.skip_push {
      git.run(&["remote", "get-url", "origin"]).context("origin is required to push release tags; use --skip-push --skip-github-release for local tags")?;
    }
    if let Some(github) = &github {
      github.preflight()?;
    }
  }
  let http = HttpClient::new(&config.http)?;
  let mut pending = BTreeMap::<usize, BTreeSet<usize>>::new();
  for idx in &order {
    let package = &packages[*idx];
    let mut manifests = BTreeSet::new();
    if !package.config.skip_publish {
      for (manifest_idx, manifest) in package.manifests.iter().enumerate() {
        if !manifest.can_publish() {
          continue;
        }
        let published = if args.dry_run {
          false
        } else {
          match manifest.registry().exists(
            &http,
            &config.registries,
            manifest.name(),
            manifest.version(),
          ) {
            Ok(found) => found,
            Err(error) => {
              eprintln!(
                "{} {}@{}: registry lookup failed ({error:#}); attempting publish",
                manifest.registry().label(),
                manifest.name(),
                manifest.version()
              );
              false
            }
          }
        };
        if published {
          println!(
            "{} {}@{}: already published; skip",
            manifest.registry().label(),
            manifest.name(),
            manifest.version()
          );
        } else {
          manifests.insert(manifest_idx);
        }
      }
    }
    pending.insert(*idx, manifests);
  }
  // Fully published packages need no hooks or build inputs unless release assets are needed.
  let targets = order
    .iter()
    .filter(|i| !pending[i].is_empty() || (gh_enabled && !packages[**i].config.skip_github_release))
    .map(|i| packages[*i].clone())
    .collect::<Vec<_>>();
  artifacts::restore(
    &git.root,
    config,
    &targets,
    args.dry_run,
    args.allow_missing_artifacts || args.dry_run,
  )?;
  for idx in order {
    let package = &packages[idx];
    let tag = &tags[&idx];
    println!("Release {} v{} ({tag})", package.name, package.version());
    if !pending[&idx].is_empty() {
      for script in &package.config.before_publish_scripts {
        let mut words = shell_words::split(&script.command)?;
        words.extend(script.args.iter().cloned());
        registry::execute(&git.root.join(&package.path), &words, args.dry_run)
          .with_context(|| format!("{}: before_publish failed", package.name))?;
      }
      for manifest_idx in manifest_order(&package.manifests)? {
        if !pending[&idx].contains(&manifest_idx) {
          continue;
        }
        let manifest = &package.manifests[manifest_idx];
        if !args.dry_run {
          let current = Manifests::read(manifest.path(), &|p| match fs::read_to_string(
            git.root.join(p),
          ) {
            Ok(raw) => Ok(Some(raw)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
          })?
          .context("manifest lost its version during before_publish")?;
          ensure!(
            current.version() == manifest.version() && current.name() == manifest.name(),
            "{}: before_publish changed name/version",
            manifest.path().display()
          );
        }
        let registry = manifest.registry();
        let words = registry.command(&config.packages, manifest.version())?;
        registry::publish(
          &registry,
          &git.root.join(manifest.path().parent().unwrap()),
          &words,
          args.dry_run,
        )
        .with_context(|| format!("{}: {} publish failed", manifest.name(), registry.label()))?;
      }
    }
    if args.dry_run {
      println!(
        "would tag {head} as {tag}{}",
        if args.skip_push {
          ""
        } else {
          " and push to origin"
        }
      );
    } else {
      git.run(&["tag", tag, &head])?;
      if !args.skip_push {
        git.run(&[
          "push",
          "origin",
          &format!("refs/tags/{tag}:refs/tags/{tag}"),
        ])?;
      }
    }
    if gh_enabled && !package.config.skip_github_release {
      let assets = artifacts::assets(
        &git.root,
        package,
        args.allow_missing_artifacts || args.dry_run,
      )?;
      if args.dry_run {
        println!(
          "would create GitHub release {tag} with {} assets",
          assets.len()
        );
      } else {
        github.as_ref().context("missing GitHub client")?.release(
          tag,
          &format!("{} v{}", package.name, package.version()),
          &changelog::release_body(&git.root, &config.changelog, package)?,
          &assets,
          !package.version().pre.is_empty(),
        )?;
      }
    }
  }
  Ok(())
}

fn manifest_order(manifests: &[Manifests]) -> Result<Vec<usize>> {
  fn visit(
    i: usize,
    manifests: &[Manifests],
    states: &mut [u8],
    out: &mut Vec<usize>,
  ) -> Result<()> {
    ensure!(
      states[i] != 1,
      "cyclic manifest dependency involving {}",
      manifests[i].name()
    );
    if states[i] == 2 {
      return Ok(());
    }
    states[i] = 1;
    for dep in manifests[i].dependencies() {
      if let Some(j) = manifests
        .iter()
        .position(|m| m.name() == dep.name && m.registry() == dep.registry)
        && i != j
      {
        visit(j, manifests, states, out)?;
      }
    }
    states[i] = 2;
    out.push(i);
    Ok(())
  }
  let mut out = vec![];
  let mut states = vec![0; manifests.len()];
  for idx in 0..manifests.len() {
    visit(idx, manifests, &mut states, &mut out)?;
  }
  Ok(out)
}
