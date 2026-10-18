use crate::{
  action::Edits,
  changelog,
  config::{CONFIG_PATH, Config},
  dependency::Graph,
  git::{Commit, Git},
  github::Github,
  manifest::{Manifest, Versions},
  package::{Package, patterns},
  version,
};
use anyhow::{Context, Result, ensure};
use clap::Args;
use semver::Version;
use std::{
  collections::{BTreeMap, BTreeSet},
  io::IsTerminal,
  path::{Path, PathBuf},
};

#[derive(Args, Default)]
pub struct Prepare {
  /// Package directory or manifest name. Omit to review all candidates interactively.
  pub package: Option<String>,
  /// Bump type (patch, minor, major, premajor, preminor, prepatch, prerelease, release) or exact version.
  #[arg(requires = "package")]
  pub bump: Option<String>,
  /// Changelog commit IDs, comma-separated; use "none" for no commit entries.
  #[arg(long, requires = "package")]
  pub changes: Option<String>,
  #[arg(long)]
  pub from: Option<String>,
  #[arg(long, default_value = "HEAD")]
  pub to: String,
  /// Must resolve to checked-out HEAD; the PR base uses the current branch.
  #[arg(long = "in")]
  pub reference: Option<String>,
  #[arg(long)]
  pub allow_dirty: bool,
  /// Write changes locally without committing, pushing, or opening a PR.
  #[arg(long)]
  pub skip_commit: bool,
  /// Create a local release branch without pushing or opening a PR.
  #[arg(long)]
  pub skip_pr: bool,
  /// Persist skip_publish = true in the selected packages' configuration.
  #[arg(long)]
  pub skip_publish: bool,
  #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
  pub draft: bool,
  #[arg(long)]
  pub dry_run: bool,
  #[arg(long, default_value = "rc")]
  pub preid: String,
  #[arg(long, env = "GITHUB_TOKEN", hide_env_values = true)]
  pub github_token: Option<String>,
}

struct Target {
  index: usize,
  next: Version,
  commits: Vec<Commit>,
}

pub fn run(git: &Git, config: &Config, packages: &[Package], args: Prepare) -> Result<()> {
  ensure!(
    !git.shallow()?,
    "prepare requires full Git history and release tags; fetch with --unshallow and --tags"
  );
  ensure!(
    args.allow_dirty || git.clean()?,
    "working tree is dirty; commit changes or use --allow-dirty for unrelated edits"
  );
  let head = git.resolve("HEAD")?;
  let base = git.branch()?;
  ensure!(
    base == config.git.default_branch || patterns(&config.git.release_branches)?.is_match(&base),
    "{base} is not a configured release branch"
  );
  if let Some(reference) = &args.reference {
    ensure!(
      git.resolve(reference)? == head,
      "--in must refer to checked-out HEAD in 0.1.0"
    );
  }
  let to = git.resolve(&args.to)?;
  ensure!(
    to == head,
    "--to must resolve to HEAD in 0.1.0; checkout the desired release source first"
  );
  let from = args.from.as_deref().map(|s| git.resolve(s)).transpose()?;
  if let Some(from) = &from {
    git
      .run(&["merge-base", "--is-ancestor", from, &to])
      .context("--from must be an ancestor of HEAD")?;
  }
  let interactive = args.package.is_none() || args.bump.is_none();
  ensure!(
    !interactive || std::io::stdin().is_terminal(),
    "non-interactive prepare requires <package> <bump_type>"
  );
  if !args.skip_pr && !args.skip_commit && !args.dry_run {
    git
      .run(&[
        "fetch",
        "--tags",
        "origin",
        &format!("refs/heads/{base}:refs/remotes/origin/{base}"),
      ])
      .context("cannot fetch release branch and tags from origin")?;
    ensure!(
      git.resolve(&format!("refs/remotes/origin/{base}"))? == head,
      "HEAD is not the tip of origin/{base}; update your checkout before preparing a release PR"
    );
  }
  let graph = Graph::new(packages);
  let mut paths: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
  let mut direct = vec![Vec::<Commit>::new(); packages.len()];
  let mut history = vec![Vec::<Commit>::new(); packages.len()];
  for (idx, package) in packages.iter().enumerate() {
    let tag = match &from {
      Some(reference) => Some(reference.clone()),
      None => git.latest_tag(package.tag_format(config), &package.name, &to)?,
    };
    let commits = git.commits(tag.as_deref(), &to)?;
    let ignored = patterns(
      package
        .config
        .change_ignore
        .as_ref()
        .unwrap_or(&config.packages.changes_ignore),
    )?;
    for commit in &commits {
      if commit
        .summary
        .starts_with(&config.github.release_pr_title_format)
        && !config.github.release_pr_title_format.is_empty()
      {
        continue;
      }
      if !paths.contains_key(&commit.sha) {
        paths.insert(commit.sha.clone(), git.changed_paths(&commit.sha)?);
      }
      let changed = &paths[&commit.sha];
      if changed.iter().any(|path| {
        let owner = packages
          .iter()
          .filter(|p| path.starts_with(&p.path))
          .max_by_key(|p| p.path.components().count());
        owner.is_some_and(|p| p.path == package.path)
          && path
            .strip_prefix(&package.path)
            .is_ok_and(|relative| !ignored.is_match(relative))
      }) {
        direct[idx].push(commit.clone());
      }
    }
    history[idx] = commits;
  }
  let seeds = direct
    .iter()
    .enumerate()
    .filter(|(_, c)| !c.is_empty())
    .map(|(i, _)| i)
    .collect();
  let affected = graph.affected(&seeds);
  let selected = if let Some(input) = &args.package {
    let matched: Vec<_> = packages
      .iter()
      .enumerate()
      .filter(|(_, p)| p.matches(input))
      .map(|(i, _)| i)
      .collect();
    ensure!(
      matched.len() == 1,
      "package '{input}' is missing or ambiguous; use its directory path"
    );
    BTreeSet::from([matched[0]])
  } else {
    affected.clone()
  };
  if selected.is_empty() {
    println!("No changes to prepare.");
    return Ok(());
  }
  let order = graph.order(&selected, packages)?;
  let mut targets: Vec<Target> = Vec::new();
  for idx in order {
    let package = &packages[idx];
    let mut choices = direct[idx].clone();
    let upstream = graph.upstream(idx);
    for dependency in &upstream {
      if let Some(target) = targets.iter().find(|t| t.index == *dependency) {
        for commit in &target.commits {
          if !choices.iter().any(|c| c.sha == commit.sha) {
            choices.push(commit.clone());
          }
        }
      } else {
        for commit in &direct[*dependency] {
          if !choices.iter().any(|c| c.sha == commit.sha) {
            choices.push(commit.clone());
          }
        }
      }
    }
    let dependency_commits: BTreeSet<_> = choices.iter().map(|c| c.sha.clone()).collect();
    if interactive {
      for commit in &history[idx] {
        if !choices.iter().any(|c| c.sha == commit.sha) {
          choices.push(commit.clone());
        }
      }
    }
    let commits = if let Some(changes) = &args.changes {
      if changes == "none" {
        vec![]
      } else {
        let mut picked = Vec::new();
        for reference in changes.split(',') {
          let sha = git.resolve(reference.trim())?;
          let commit = history[idx]
            .iter()
            .find(|c| c.sha == sha)
            .with_context(|| format!("{reference} is outside {}'s change range", package.name))?;
          if !picked.iter().any(|c: &Commit| c.sha == sha) {
            picked.push(commit.clone());
          }
        }
        picked
      }
    } else if interactive && !choices.is_empty() {
      let labels: Vec<_> = choices
        .iter()
        .map(|c| {
          format!(
            "{} {}{}",
            &c.sha[..7],
            c.summary,
            if direct[idx].iter().any(|d| d.sha == c.sha) {
              ""
            } else if dependency_commits.contains(&c.sha) {
              " (dependency)"
            } else {
              " (other change)"
            }
          )
        })
        .collect();
      let defaults: Vec<_> = choices
        .iter()
        .map(|c| direct[idx].iter().any(|d| d.sha == c.sha))
        .collect();
      let chosen = dialoguer::MultiSelect::new()
        .with_prompt(format!("{}: commits to include in changelog", package.name))
        .items(&labels)
        .defaults(&defaults)
        .interact()?;
      chosen.into_iter().map(|i| choices[i].clone()).collect()
    } else {
      choices
    };
    let next = if let Some(rule) = &args.bump {
      version::bump(package.version(), rule, &args.preid)?
    } else {
      let rules = ["patch", "minor", "major"];
      let mut labels = rules
        .iter()
        .map(|rule| {
          version::bump(package.version(), rule, &args.preid).map(|v| format!("{rule} ({v})"))
        })
        .collect::<Result<Vec<_>>>()?;
      labels.extend(["exact version".into(), "skip package".into()]);
      let choice = dialoguer::Select::new()
        .with_prompt(format!(
          "{}: version (current {})",
          package.name,
          package.version()
        ))
        .items(&labels)
        .default(0)
        .interact()?;
      if choice == 4 {
        continue;
      }
      if choice == 3 {
        let value = dialoguer::Input::<String>::new()
          .with_prompt("Next version")
          .validate_with(|s: &String| -> Result<(), String> {
            let v = Version::parse(s).map_err(|e| e.to_string())?;
            if package
              .manifests
              .iter()
              .all(|m| v.cmp_precedence(m.version()).is_gt())
            {
              Ok(())
            } else {
              Err("version must exceed every manifest's current version".into())
            }
          })
          .interact_text()?;
        Version::parse(&value)?
      } else {
        version::bump(package.version(), rules[choice], &args.preid)?
      }
    };
    for manifest in &package.manifests {
      ensure!(
        next.cmp_precedence(manifest.version()).is_gt(),
        "{} is at {}; choose an exact version above every manifest",
        manifest.name(),
        manifest.version()
      );
    }
    println!(
      "{}: {} -> {} ({} changelog commits)",
      package.name,
      package.version(),
      next,
      commits.len()
    );
    targets.push(Target {
      index: idx,
      next,
      commits,
    });
  }
  if targets.is_empty() {
    println!("No packages selected.");
    return Ok(());
  }
  for idx in &affected {
    if !targets.iter().any(|t| t.index == *idx) {
      println!(
        "{}: affected, not selected for a version bump",
        packages[*idx].name
      );
    }
  }
  // A shared Cargo workspace version cannot be changed for only some inheriting members.
  let mut shared = BTreeMap::<PathBuf, Version>::new();
  for target in &targets {
    for m in &packages[target.index].manifests {
      if m.inherits_version() {
        let source = m.workspace().unwrap();
        if let Some(previous) = shared.insert(source.to_owned(), target.next.clone()) {
          ensure!(
            previous == target.next,
            "members sharing {} must use the same version",
            source.display()
          );
        }
      }
    }
  }
  for (idx, package) in packages.iter().enumerate() {
    for m in &package.manifests {
      if m.inherits_version() && m.workspace().is_some_and(|w| shared.contains_key(w)) {
        ensure!(
          targets.iter().any(|t| t.index == idx),
          "{} inherits the same workspace version; select all inheriting members together",
          package.name
        );
      }
    }
  }
  let github = if !args.skip_pr && !args.skip_commit && !args.dry_run {
    Some(Github::new(
      &git.root,
      &config.github,
      &config.http,
      args.github_token.clone(),
    )?)
  } else {
    None
  };
  if !args.skip_pr && !args.skip_commit && !args.dry_run {
    git
      .run(&["remote", "get-url", "origin"])
      .context("origin is required to create a release PR")?;
    github
      .as_ref()
      .context("missing GitHub client")?
      .preflight()?;
  }
  let notes = changelog::load(git)?;
  let mut consumed = BTreeSet::new();
  let mut edits = Edits::new(&git.root);
  let mut versions = Versions::new();
  let mut body = String::from("Prepared releases:\n\n");
  for target in &targets {
    let package = &packages[target.index];
    for manifest in &package.manifests {
      manifest.set_version(&mut edits, &target.next)?;
      versions.insert(
        (manifest.registry(), manifest.name().to_owned()),
        target.next.clone(),
      );
    }
    let dependencies = graph.dependencies[target.index]
      .iter()
      .filter_map(|dep| targets.iter().find(|t| t.index == *dep))
      .map(|t| format!("Update {} to {}", packages[t.index].name, t.next))
      .collect::<Vec<_>>();
    let (text, used) = changelog::render(
      &config.changelog,
      &target.commits,
      &notes,
      &dependencies,
      github.as_ref(),
    );
    let text = if args.skip_publish {
      text
    } else {
      changelog::with_registries(&config.changelog, package, &target.next, text)
    };
    consumed.extend(used);
    changelog::append(&mut edits, &config.changelog, package, &target.next, &text)?;
    body.push_str(&format!(
      "## {} v{}\n\n{text}\n\n",
      package.display_name, target.next
    ));
  }
  for target in &targets {
    for manifest in &packages[target.index].manifests {
      manifest.update_dependencies(&mut edits, &versions)?;
    }
  }
  for path in consumed {
    edits.remove(&path);
  }
  if args.skip_publish {
    let path = Path::new(CONFIG_PATH);
    let mut doc = edits
      .read(path)?
      .context("config disappeared")?
      .parse::<toml_edit::DocumentMut>()?;
    for target in &targets {
      doc["packages"][&packages[target.index].id()]["skip_publish"] = toml_edit::value(true);
    }
    edits.write(path, doc.to_string());
  }
  edits.check_clean_targets(git)?;
  edits.show();
  if args.dry_run {
    println!("Dry run: no files, commits, branches, pushes, or PRs were created.");
    return Ok(());
  }
  if args.skip_commit {
    return edits.apply();
  }
  let digest_input = edits
    .files
    .iter()
    .map(|(p, c)| format!("{}\0{:?}", p.display(), c))
    .collect::<Vec<_>>()
    .join("\0");
  let digest = git.input(
    &["hash-object", "--stdin"],
    &format!("{head}\0{digest_input}"),
    &[],
  )?;
  let branch = config
    .github
    .release_pr_branch_format
    .replace("{branch}", &base)
    .replace("{commit}", &digest.trim()[..12]);
  let title = if targets.len() == 1 {
    format!(
      "{}{} v{}",
      config.github.release_pr_title_format, packages[targets[0].index].name, targets[0].next
    )
  } else {
    format!(
      "{}{} packages",
      config.github.release_pr_title_format,
      targets.len()
    )
  };
  let commit = edits.commit(git, &branch, &title)?;
  println!(
    "Created {branch} at {}. Your current checkout is unchanged.",
    &commit[..7]
  );
  if !args.skip_pr {
    git.run(&[
      "push",
      "origin",
      &format!("refs/heads/{branch}:refs/heads/{branch}"),
    ])?;
    github
      .as_ref()
      .context("missing GitHub client")?
      .pull_request(&base, &branch, &title, &body, &config.github, args.draft)?;
  }
  Ok(())
}
