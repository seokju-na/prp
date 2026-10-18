use crate::{
  artifacts, changelog,
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
use clap::Args;
use serde::Serialize;
use std::{
  collections::{BTreeMap, BTreeSet},
  fs,
};

#[derive(Args)]
pub struct Release {
  /// Query registries and tags without running hooks, restoring artifacts, or publishing.
  #[arg(long)]
  pub check: bool,
  /// Print the check result as JSON for any CI system.
  #[arg(long, requires = "check")]
  pub json: bool,
  /// Require this exact commit before checking or publishing (for a split build/release pipeline).
  #[arg(long)]
  pub expected_head: Option<String>,
  /// Query registries and show remaining operations without writing or executing commands.
  #[arg(long)]
  pub dry_run: bool,
  #[arg(long)]
  pub skip_github_release: bool,
  /// Keep tags local (also requires --skip-github-release).
  #[arg(long, requires = "skip_github_release")]
  pub skip_push: bool,
  #[arg(long)]
  pub allow_missing_artifacts: bool,
  #[arg(long, env = "GITHUB_TOKEN", hide_env_values = true)]
  pub github_token: Option<String>,
}

#[derive(Serialize)]
struct PublishManifest {
  path: String,
  registry: &'static str,
  name: String,
  version: String,
  published: bool,
}

#[derive(Serialize)]
struct Target {
  #[serde(skip)]
  index: usize,
  package: String,
  path: String,
  version: String,
  tag: String,
  create_tag: bool,
  push_tag: bool,
  github_release: bool,
  manifests: Vec<PublishManifest>,
  #[serde(skip)]
  pending: BTreeSet<usize>,
}

#[derive(Serialize)]
struct Plan {
  schema_version: u32,
  head: String,
  packages: Vec<Target>,
}

pub fn run(git: &Git, config: &Config, packages: &[Package], args: Release) -> Result<()> {
  ensure!(
    !git.shallow()?,
    "release requires full Git history and tags"
  );
  let head = git.resolve("HEAD")?;
  if let Some(expected) = &args.expected_head {
    ensure!(
      *expected == head,
      "HEAD differs from --expected-head: expected {expected}, found {head}"
    );
  }
  check_sources(git, packages)?;
  let plan = plan(git, config, packages, &args, head)?;
  if args.json {
    println!("{}", serde_json::to_string_pretty(&plan)?);
    return Ok(());
  }
  println!("Release source: {}", plan.head);
  if plan.packages.is_empty() {
    println!("All configured versions and tags are already published. Nothing to release.");
    return Ok(());
  }
  for target in &plan.packages {
    println!(
      "Release {} v{} ({})",
      target.package, target.version, target.tag
    );
    for manifest in &target.manifests {
      println!(
        "{} {}@{}: {}",
        manifest.registry,
        manifest.name,
        manifest.version,
        if manifest.published {
          "already published; skip"
        } else {
          "publish required"
        }
      );
    }
  }
  if args.check {
    return Ok(());
  }

  let github = if plan.packages.iter().any(|p| p.github_release) && !args.dry_run {
    let github = Github::new(
      &git.root,
      &config.github,
      &config.http,
      args.github_token.clone(),
    )?;
    github.preflight()?;
    Some(github)
  } else {
    None
  };
  if !args.skip_push && !args.dry_run {
    git.run(&["remote", "get-url", "origin"])
      .context("origin is required to push release tags; use --skip-push --skip-github-release for local tags")?;
  }
  // Validate all build inputs before the first publication, including asset-only packages.
  let targets = plan
    .packages
    .iter()
    .filter(|p| !p.pending.is_empty() || p.github_release)
    .map(|p| packages[p.index].clone())
    .collect::<Vec<_>>();
  artifacts::restore(
    &git.root,
    config,
    &targets,
    args.dry_run,
    args.allow_missing_artifacts || args.dry_run,
  )?;
  for target in &plan.packages {
    if target.github_release {
      artifacts::assets(
        &git.root,
        &packages[target.index],
        args.allow_missing_artifacts || args.dry_run,
      )?;
    }
  }
  if !args.dry_run {
    check_sources(git, packages)?;
  }
  for target in &plan.packages {
    let package = &packages[target.index];
    if !target.pending.is_empty() {
      for script in &package.config.before_publish_scripts {
        let mut words = shell_words::split(&script.command)?;
        words.extend(script.args.iter().cloned());
        registry::execute(&git.root.join(&package.path), &words, args.dry_run)
          .with_context(|| format!("{}: before_publish failed", package.name))?;
      }
      // Hooks may generate build output, but cannot change committed release inputs.
      if !args.dry_run {
        ensure!(
          git.resolve("HEAD")? == plan.head,
          "HEAD changed during release"
        );
        check_sources(git, packages)?;
      }
      for index in manifest_order(&package.manifests)? {
        if !target.pending.contains(&index) {
          continue;
        }
        let manifest = &package.manifests[index];
        let registry = manifest.registry();
        let words = registry.command(&config.packages, manifest.version())?;
        registry::publish(
          &registry,
          &git.root.join(manifest.path().parent().unwrap()),
          &words,
          args.dry_run,
        )
        .with_context(|| format!("{}: {} publish failed", manifest.name(), registry.label()))?;
        if !args.dry_run {
          ensure!(
            git.resolve("HEAD")? == plan.head,
            "HEAD changed during release"
          );
          check_sources(git, packages)?;
        }
      }
    }
    if args.dry_run {
      if target.create_tag {
        println!("would tag {} as {}", plan.head, target.tag);
      }
      if target.push_tag {
        println!("would push tag {} to origin", target.tag);
      }
    } else {
      ensure!(
        git.resolve("HEAD")? == plan.head,
        "HEAD changed during release"
      );
      if target.create_tag {
        git.run(&["tag", &target.tag, &plan.head])?;
      }
      if target.push_tag {
        git.run(&[
          "push",
          "origin",
          &format!("refs/tags/{0}:refs/tags/{0}", target.tag),
        ])?;
      }
    }
    if target.github_release {
      let assets = artifacts::assets(
        &git.root,
        package,
        args.allow_missing_artifacts || args.dry_run,
      )?;
      if args.dry_run {
        println!(
          "would ensure GitHub release {} with {} assets",
          target.tag,
          assets.len()
        );
      } else {
        github.as_ref().context("missing GitHub client")?.release(
          &target.tag,
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

fn check_sources(git: &Git, packages: &[Package]) -> Result<()> {
  for package in packages {
    for manifest in &package.manifests {
      ensure!(
        git.read_at("HEAD", manifest.path())?.as_deref()
          == Some(fs::read_to_string(git.root.join(manifest.path()))?.as_str()),
        "{} differs from HEAD; release requires committed manifests",
        manifest.path().display()
      );
    }
  }
  git
    .run(&["diff", "--quiet", "HEAD", "--"])
    .context("release requires committed source files; commit or restore tracked changes first")?;
  ensure!(
    git
      .run(&["ls-files", "--others", "--exclude-standard"])?
      .is_empty(),
    "release requires committed source files; commit untracked files or ignore generated output first"
  );
  Ok(())
}

fn plan(
  git: &Git,
  config: &Config,
  packages: &[Package],
  args: &Release,
  head: String,
) -> Result<Plan> {
  let remote_tags = if !args.skip_push && git.run(&["remote", "get-url", "origin"]).is_ok() {
    parse_remote_tags(
      &git
        .run(&["ls-remote", "--tags", "origin"])
        .context("cannot inspect remote release tags")?,
    )
  } else {
    BTreeMap::new()
  };
  let http = HttpClient::new(&config.http)?;
  let gh_enabled = !args.skip_github_release && !config.github.skip_releases;
  let mut targets = BTreeMap::new();
  let mut tags = BTreeSet::new();
  for (index, package) in packages.iter().enumerate() {
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
    ensure!(tags.insert(tag.clone()), "duplicate release tag {tag}");
    let local = if git
      .run(&["show-ref", "--verify", &format!("refs/tags/{tag}")])
      .is_ok()
    {
      Some(git.resolve(&format!("refs/tags/{tag}"))?)
    } else {
      None
    };
    let remote = remote_tags.get(&tag);
    if let (Some(local), Some(remote)) = (&local, remote) {
      ensure!(
        local == remote,
        "local and remote tag {tag} point to different commits"
      );
    }
    let existing = remote.or(local.as_ref());
    let mut pending = BTreeSet::new();
    let mut manifests = Vec::new();
    for (mi, manifest) in package.manifests.iter().enumerate() {
      if !manifest.can_publish() || package.config.skip_publish {
        continue;
      }
      let registry = manifest.registry();
      let published = registry
        .exists(
          &http,
          &config.registries,
          manifest.name(),
          manifest.version(),
        )
        .with_context(|| {
          format!(
            "{} {}@{}: cannot determine registry state; no packages published",
            registry.label(),
            manifest.name(),
            manifest.version()
          )
        })?;
      if !published {
        pending.insert(mi);
      }
      manifests.push(PublishManifest {
        path: manifest.path().to_string_lossy().into_owned(),
        registry: registry.label(),
        name: manifest.name().into(),
        version: manifest.version().to_string(),
        published,
      });
    }
    let tag_complete = if args.skip_push {
      local.is_some()
    } else {
      remote.is_some()
    };
    let github_release = gh_enabled && !package.config.skip_github_release;
    // Tags on older commits belong to previous releases. Never publish new bytes for that version.
    if let Some(source) = existing
      && source != &head
    {
      ensure!(
        git
          .run(&["merge-base", "--is-ancestor", source, &head])
          .is_ok(),
        "tag {tag} points outside HEAD history; check out its source commit {source}"
      );
      ensure!(
        pending.is_empty() && tag_complete,
        "tag {tag} belongs to {source}; check out that commit to finish this release"
      );
      continue;
    }
    if pending.is_empty() && tag_complete && !github_release {
      continue;
    }
    manifest_order(&package.manifests)?;
    for mi in &pending {
      let m = &package.manifests[*mi];
      m.registry().command(&config.packages, m.version())?;
    }
    for script in &package.config.before_publish_scripts {
      ensure!(
        !shell_words::split(&script.command)?.is_empty(),
        "{}: empty before_publish command",
        package.name
      );
    }
    targets.insert(
      index,
      Target {
        index,
        package: package.name.clone(),
        path: package.id(),
        version: package.version().to_string(),
        tag,
        create_tag: local.is_none() && remote.is_none(),
        push_tag: !args.skip_push && remote.is_none(),
        github_release,
        manifests,
        pending,
      },
    );
  }
  let order = Graph::new(packages).order(&targets.keys().copied().collect(), packages)?;
  Ok(Plan {
    schema_version: 1,
    head,
    packages: order
      .into_iter()
      .map(|i| targets.remove(&i).unwrap())
      .collect(),
  })
}

fn parse_remote_tags(output: &str) -> BTreeMap<String, String> {
  let mut tags = BTreeMap::new();
  let mut peeled = BTreeMap::new();
  for line in output.lines() {
    if let Some((sha, reference)) = line.split_once('\t')
      && let Some(tag) = reference.strip_prefix("refs/tags/")
    {
      if let Some(tag) = tag.strip_suffix("^{}") {
        peeled.insert(tag.into(), sha.into());
      } else {
        tags.insert(tag.into(), sha.into());
      }
    }
  }
  tags.extend(peeled);
  tags
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
