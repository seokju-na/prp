use anyhow::{Result, ensure};
use semver::{BuildMetadata, Prerelease, Version};
use std::cmp::Ordering;

/// `Major`, `Minor` and `Patch` come from commits; the others are chosen by hand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BumpRule {
  Major,
  Premajor(Prerelease),
  Minor,
  Preminor(Prerelease),
  Patch,
  Prepatch(Prerelease),
  Prerelease(Prerelease),
  Release,
  Exact(Version),
}

impl BumpRule {
  pub fn name(&self) -> &'static str {
    match self {
      Self::Major => "major",
      Self::Premajor(_) => "premajor",
      Self::Minor => "minor",
      Self::Preminor(_) => "preminor",
      Self::Patch => "patch",
      Self::Prepatch(_) => "prepatch",
      Self::Prerelease(_) => "prerelease",
      Self::Release => "release",
      Self::Exact(_) => "exact",
    }
  }

  pub fn prerelease(&self) -> Option<&Prerelease> {
    match self {
      Self::Premajor(pre) | Self::Preminor(pre) | Self::Prepatch(pre) | Self::Prerelease(pre) => {
        Some(pre)
      }
      _ => None,
    }
  }

  /// The rule actually applied to `version`: a 0.x version takes breaking
  /// changes as minor bumps.
  pub fn for_version(self, version: &Version) -> Self {
    match self {
      Self::Major if version.major == 0 => Self::Minor,
      rule => rule,
    }
  }

  fn rank(&self) -> u8 {
    match self {
      Self::Major | Self::Premajor(_) => 3,
      Self::Minor | Self::Preminor(_) => 2,
      Self::Exact(_) => 0,
      _ => 1,
    }
  }

  pub fn max(self, other: Self) -> Self {
    if other.rank() > self.rank() {
      other
    } else {
      self
    }
  }
}

/// The bump a commit asks for under the Conventional Commits convention.
/// Anything else that reaches a package still is a change, hence a patch.
pub fn commit_bump_rule(summary: &str, body: &str) -> BumpRule {
  let breaking_footer = body
    .lines()
    .any(|line| line.starts_with("BREAKING CHANGE:") || line.starts_with("BREAKING-CHANGE:"));
  let header = summary.split_once(':').map_or("", |(header, _)| header);
  if breaking_footer || header.ends_with('!') {
    return BumpRule::Major;
  }
  match header.split('(').next().unwrap_or("").trim() {
    "feat" => BumpRule::Minor,
    _ => BumpRule::Patch,
  }
}

pub fn bump(current: &Version, rule: &BumpRule) -> Result<Version> {
  if let BumpRule::Exact(version) = rule {
    ensure!(
      version.cmp_precedence(current) == Ordering::Greater,
      "{version} must be greater than {current}"
    );
    return Ok(version.clone());
  }
  let mut next = current.clone();
  next.build = BuildMetadata::EMPTY;
  next.pre = Prerelease::EMPTY;

  match rule {
    BumpRule::Major => {
      if current.pre.is_empty() || current.minor != 0 || current.patch != 0 {
        next.major = inc(next.major)?;
      }
      next.minor = 0;
      next.patch = 0;
    }
    BumpRule::Premajor(pre) => {
      next.major = inc(next.major)?;
      next.minor = 0;
      next.patch = 0;
      next.pre = first_pre(pre)?;
    }
    BumpRule::Minor => {
      if current.pre.is_empty() || current.patch != 0 {
        next.minor = inc(next.minor)?;
      }
      next.patch = 0;
    }
    BumpRule::Preminor(pre) => {
      next.minor = inc(next.minor)?;
      next.patch = 0;
      next.pre = first_pre(pre)?;
    }
    BumpRule::Patch => {
      if current.pre.is_empty() {
        next.patch = inc(next.patch)?;
      }
    }
    BumpRule::Prepatch(pre) => {
      next.patch = inc(next.patch)?;
      next.pre = first_pre(pre)?;
    }
    BumpRule::Prerelease(pre) => {
      if current.pre.is_empty() {
        next.patch = inc(next.patch)?;
        next.pre = first_pre(pre)?;
      } else {
        next.pre = next_pre(&current.pre, pre)?;
      }
    }
    BumpRule::Release => {
      ensure!(
        !current.pre.is_empty(),
        "{current} is already a stable version"
      );
    }
    BumpRule::Exact(_) => unreachable!("returned above"),
  }
  Ok(next)
}

fn inc(n: u64) -> Result<u64> {
  n.checked_add(1)
    .ok_or_else(|| anyhow::anyhow!("version overflow"))
}

fn is_numeric(part: &str) -> bool {
  part.parse::<u64>().is_ok()
}

fn split_counter(pre: &str) -> (&str, Option<&str>) {
  match pre.rsplit_once('.') {
    Some((id, counter)) if is_numeric(counter) => (id, Some(counter)),
    None if is_numeric(pre) => ("", Some(pre)),
    _ => (pre, None),
  }
}

fn first_pre(pre: &Prerelease) -> Result<Prerelease> {
  if pre.is_empty() {
    return Ok(Prerelease::new("0")?);
  }
  Ok(pre.clone())
}

fn next_pre(current: &Prerelease, pre: &Prerelease) -> Result<Prerelease> {
  let (id, counter) = split_counter(pre.as_str());
  let mut parts: Vec<String> = current.as_str().split('.').map(str::to_owned).collect();
  if let Some(part) = parts.iter_mut().rfind(|part| is_numeric(part)) {
    *part = inc(part.parse()?)?.to_string();
  } else {
    ensure!(
      counter.is_some() || current.as_str() != id,
      "prerelease {current} already exists"
    );
    parts.push(counter.unwrap_or("0").into());
  }

  // Keep counting only within the same series; `beta.foo` restarts because `foo` is no counter.
  let id_len = id.split('.').count();
  let same_series =
    parts.get(id_len).is_some_and(|part| is_numeric(part)) && parts[..id_len].join(".") == id;
  if !id.is_empty() && !same_series {
    return Ok(pre.clone());
  }
  Ok(Prerelease::new(&parts.join("."))?)
}

#[cfg(test)]
mod tests {
  use super::*;
  use rstest::rstest;

  #[rstest]
  #[case("0.0.0", "1.0.0")]
  #[case("0.2.3", "1.0.0")]
  #[case("1.2.3", "2.0.0")]
  #[case("1.2.3+build.5", "2.0.0")]
  #[case("1.0.0-beta.1", "1.0.0")]
  #[case("1.2.0-beta.1", "2.0.0")]
  #[case("1.2.3-beta.1", "2.0.0")]
  #[case("1.2.3-beta", "2.0.0")]
  #[case("1.2.3-5", "2.0.0")]
  fn major(#[case] current: Version, #[case] expected: Version) {
    assert_eq!(bump(&current, &BumpRule::Major).unwrap(), expected);
  }

  #[rstest]
  #[case("0.0.0", "0.1.0")]
  #[case("0.2.3", "0.3.0")]
  #[case("1.2.3", "1.3.0")]
  #[case("1.2.3+build.5", "1.3.0")]
  #[case("1.0.0-beta.1", "1.0.0")]
  #[case("1.2.0-beta.1", "1.2.0")]
  #[case("1.2.3-beta.1", "1.3.0")]
  #[case("1.2.3-beta", "1.3.0")]
  #[case("1.2.3-5", "1.3.0")]
  fn minor(#[case] current: Version, #[case] expected: Version) {
    assert_eq!(bump(&current, &BumpRule::Minor).unwrap(), expected);
  }

  #[rstest]
  #[case("0.0.0", "0.0.1")]
  #[case("0.2.3", "0.2.4")]
  #[case("1.2.3", "1.2.4")]
  #[case("1.2.3+build.5", "1.2.4")]
  #[case("1.0.0-beta.1", "1.0.0")]
  #[case("1.2.0-beta.1", "1.2.0")]
  #[case("1.2.3-beta.1", "1.2.3")]
  #[case("1.2.3-beta", "1.2.3")]
  #[case("1.2.3-5", "1.2.3")]
  fn patch(#[case] current: Version, #[case] expected: Version) {
    assert_eq!(bump(&current, &BumpRule::Patch).unwrap(), expected);
  }

  #[rstest]
  #[case("0.2.3", "", "1.0.0-0")]
  #[case("0.2.3", "beta.0", "1.0.0-beta.0")]
  #[case("1.2.3", "", "2.0.0-0")]
  #[case("1.2.3", "beta.0", "2.0.0-beta.0")]
  #[case("1.2.3+build.5", "", "2.0.0-0")]
  #[case("1.2.3+build.5", "beta.0", "2.0.0-beta.0")]
  #[case("1.0.0-beta.1", "", "2.0.0-0")]
  #[case("1.0.0-beta.1", "beta.0", "2.0.0-beta.0")]
  #[case("1.2.0-beta.1", "", "2.0.0-0")]
  #[case("1.2.0-beta.1", "beta.0", "2.0.0-beta.0")]
  #[case("1.2.3-beta.1", "", "2.0.0-0")]
  #[case("1.2.3-beta.1", "beta.0", "2.0.0-beta.0")]
  #[case("1.2.3", "beta.1", "2.0.0-beta.1")]
  #[case("1.2.3", "beta", "2.0.0-beta")]
  #[case("1.2.3", "1", "2.0.0-1")]
  #[case("1.2.3", "a.b.0", "2.0.0-a.b.0")]
  fn premajor(#[case] current: Version, #[case] pre: Prerelease, #[case] expected: Version) {
    assert_eq!(bump(&current, &BumpRule::Premajor(pre)).unwrap(), expected);
  }

  #[rstest]
  #[case("0.2.3", "", "0.3.0-0")]
  #[case("0.2.3", "beta.0", "0.3.0-beta.0")]
  #[case("1.2.3", "", "1.3.0-0")]
  #[case("1.2.3", "beta.0", "1.3.0-beta.0")]
  #[case("1.2.3+build.5", "", "1.3.0-0")]
  #[case("1.2.3+build.5", "beta.0", "1.3.0-beta.0")]
  #[case("1.0.0-beta.1", "", "1.1.0-0")]
  #[case("1.0.0-beta.1", "beta.0", "1.1.0-beta.0")]
  #[case("1.2.0-beta.1", "", "1.3.0-0")]
  #[case("1.2.0-beta.1", "beta.0", "1.3.0-beta.0")]
  #[case("1.2.3-beta.1", "", "1.3.0-0")]
  #[case("1.2.3-beta.1", "beta.0", "1.3.0-beta.0")]
  #[case("1.2.3", "beta.1", "1.3.0-beta.1")]
  #[case("1.2.3", "beta", "1.3.0-beta")]
  #[case("1.2.3", "1", "1.3.0-1")]
  #[case("1.2.3", "a.b.0", "1.3.0-a.b.0")]
  fn preminor(#[case] current: Version, #[case] pre: Prerelease, #[case] expected: Version) {
    assert_eq!(bump(&current, &BumpRule::Preminor(pre)).unwrap(), expected);
  }

  #[rstest]
  #[case("0.2.3", "", "0.2.4-0")]
  #[case("0.2.3", "beta.0", "0.2.4-beta.0")]
  #[case("1.2.3", "", "1.2.4-0")]
  #[case("1.2.3", "beta.0", "1.2.4-beta.0")]
  #[case("1.2.3+build.5", "", "1.2.4-0")]
  #[case("1.2.3+build.5", "beta.0", "1.2.4-beta.0")]
  #[case("1.0.0-beta.1", "", "1.0.1-0")]
  #[case("1.0.0-beta.1", "beta.0", "1.0.1-beta.0")]
  #[case("1.2.0-beta.1", "", "1.2.1-0")]
  #[case("1.2.0-beta.1", "beta.0", "1.2.1-beta.0")]
  #[case("1.2.3-beta.1", "", "1.2.4-0")]
  #[case("1.2.3-beta.1", "beta.0", "1.2.4-beta.0")]
  #[case("1.2.3", "beta.1", "1.2.4-beta.1")]
  #[case("1.2.3", "beta", "1.2.4-beta")]
  #[case("1.2.3", "1", "1.2.4-1")]
  #[case("1.2.3", "a.b.0", "1.2.4-a.b.0")]
  fn prepatch(#[case] current: Version, #[case] pre: Prerelease, #[case] expected: Version) {
    assert_eq!(bump(&current, &BumpRule::Prepatch(pre)).unwrap(), expected);
  }

  #[rstest]
  #[case("1.2.3", "", "1.2.4-0")]
  #[case("1.2.3+build.5", "", "1.2.4-0")]
  #[case("1.2.3-beta.1", "", "1.2.3-beta.2")]
  #[case("1.2.3-beta", "", "1.2.3-beta.0")]
  #[case("1.2.3-alpha.5", "", "1.2.3-alpha.6")]
  #[case("1.2.3-5", "", "1.2.3-6")]
  #[case("1.2.3-beta.1.foo", "", "1.2.3-beta.2.foo")]
  #[case("1.2.3-beta.foo", "", "1.2.3-beta.foo.0")]
  #[case("1.2.3-a.b.1", "", "1.2.3-a.b.2")]
  #[case("1.2.3-0.beta", "", "1.2.3-1.beta")]
  #[case("1.2.3-alpha.1.2", "", "1.2.3-alpha.1.3")]
  #[case("1.2.3", "beta.0", "1.2.4-beta.0")]
  #[case("1.2.3+build.5", "beta.0", "1.2.4-beta.0")]
  #[case("1.2.3-beta.1", "beta.0", "1.2.3-beta.2")]
  #[case("1.2.3-beta", "beta.0", "1.2.3-beta.0")]
  #[case("1.2.3-alpha.5", "beta.0", "1.2.3-beta.0")]
  #[case("1.2.3-5", "beta.0", "1.2.3-beta.0")]
  #[case("1.2.3-beta.1.foo", "beta.0", "1.2.3-beta.2.foo")]
  #[case("1.2.3-beta.foo", "beta.0", "1.2.3-beta.0")]
  #[case("1.2.3-a.b.1", "beta.0", "1.2.3-beta.0")]
  #[case("1.2.3-0.beta", "beta.0", "1.2.3-beta.0")]
  #[case("1.2.3-alpha.1.2", "beta.0", "1.2.3-beta.0")]
  #[case("1.2.3", "beta.1", "1.2.4-beta.1")]
  #[case("1.2.3+build.5", "beta.1", "1.2.4-beta.1")]
  #[case("1.2.3-beta.1", "beta.1", "1.2.3-beta.2")]
  #[case("1.2.3-beta", "beta.1", "1.2.3-beta.1")]
  #[case("1.2.3-alpha.5", "beta.1", "1.2.3-beta.1")]
  #[case("1.2.3-5", "beta.1", "1.2.3-beta.1")]
  #[case("1.2.3-beta.1.foo", "beta.1", "1.2.3-beta.2.foo")]
  #[case("1.2.3-beta.foo", "beta.1", "1.2.3-beta.1")]
  #[case("1.2.3-a.b.1", "beta.1", "1.2.3-beta.1")]
  #[case("1.2.3-0.beta", "beta.1", "1.2.3-beta.1")]
  #[case("1.2.3-alpha.1.2", "beta.1", "1.2.3-beta.1")]
  #[case("1.2.3", "beta", "1.2.4-beta")]
  #[case("1.2.3+build.5", "beta", "1.2.4-beta")]
  #[case("1.2.3-beta.1", "beta", "1.2.3-beta.2")]
  #[case("1.2.3-alpha.5", "beta", "1.2.3-beta")]
  #[case("1.2.3-5", "beta", "1.2.3-beta")]
  #[case("1.2.3-beta.1.foo", "beta", "1.2.3-beta.2.foo")]
  #[case("1.2.3-beta.foo", "beta", "1.2.3-beta")]
  #[case("1.2.3-a.b.1", "beta", "1.2.3-beta")]
  #[case("1.2.3-0.beta", "beta", "1.2.3-beta")]
  #[case("1.2.3-alpha.1.2", "beta", "1.2.3-beta")]
  #[case("1.2.3", "1", "1.2.4-1")]
  #[case("1.2.3+build.5", "1", "1.2.4-1")]
  #[case("1.2.3-beta.1", "1", "1.2.3-beta.2")]
  #[case("1.2.3-beta", "1", "1.2.3-beta.1")]
  #[case("1.2.3-alpha.5", "1", "1.2.3-alpha.6")]
  #[case("1.2.3-5", "1", "1.2.3-6")]
  #[case("1.2.3-beta.1.foo", "1", "1.2.3-beta.2.foo")]
  #[case("1.2.3-beta.foo", "1", "1.2.3-beta.foo.1")]
  #[case("1.2.3-a.b.1", "1", "1.2.3-a.b.2")]
  #[case("1.2.3-0.beta", "1", "1.2.3-1.beta")]
  #[case("1.2.3-alpha.1.2", "1", "1.2.3-alpha.1.3")]
  #[case("1.2.3", "a.b.0", "1.2.4-a.b.0")]
  #[case("1.2.3+build.5", "a.b.0", "1.2.4-a.b.0")]
  #[case("1.2.3-beta.1", "a.b.0", "1.2.3-a.b.0")]
  #[case("1.2.3-beta", "a.b.0", "1.2.3-a.b.0")]
  #[case("1.2.3-alpha.5", "a.b.0", "1.2.3-a.b.0")]
  #[case("1.2.3-5", "a.b.0", "1.2.3-a.b.0")]
  #[case("1.2.3-beta.1.foo", "a.b.0", "1.2.3-a.b.0")]
  #[case("1.2.3-beta.foo", "a.b.0", "1.2.3-a.b.0")]
  #[case("1.2.3-a.b.1", "a.b.0", "1.2.3-a.b.2")]
  #[case("1.2.3-0.beta", "a.b.0", "1.2.3-a.b.0")]
  #[case("1.2.3-alpha.1.2", "a.b.0", "1.2.3-a.b.0")]
  fn prerelease(#[case] current: Version, #[case] pre: Prerelease, #[case] expected: Version) {
    assert_eq!(
      bump(&current, &BumpRule::Prerelease(pre)).unwrap(),
      expected
    );
  }

  #[rstest]
  #[case("1.0.0-beta.1", "1.0.0")]
  #[case("1.2.0-beta.1", "1.2.0")]
  #[case("1.2.3-beta.1", "1.2.3")]
  #[case("1.2.3-beta", "1.2.3")]
  #[case("1.2.3-5", "1.2.3")]
  #[case("1.2.3-beta.1+build.5", "1.2.3")]
  fn release(#[case] current: Version, #[case] expected: Version) {
    assert_eq!(bump(&current, &BumpRule::Release).unwrap(), expected);
  }

  #[rstest]
  #[case("0.3.0", "0.5.0")]
  #[case("0.3.0", "1.0.0-beta.0")]
  #[case("1.0.0-beta.1", "1.0.0")]
  fn exact(#[case] current: Version, #[case] next: Version) {
    assert_eq!(
      bump(&current, &BumpRule::Exact(next.clone())).unwrap(),
      next
    );
  }

  #[rstest]
  #[case("0.3.0", "0.3.0")]
  #[case("0.3.0", "0.2.9")]
  #[case("1.0.0", "1.0.0-beta.0")]
  fn exact_must_move_forward(#[case] current: Version, #[case] next: Version) {
    assert!(bump(&current, &BumpRule::Exact(next)).is_err());
  }

  #[rstest]
  #[case("0.0.0")]
  #[case("1.2.3")]
  #[case("1.2.3+build.5")]
  fn release_rejects_stable(#[case] current: Version) {
    assert!(bump(&current, &BumpRule::Release).is_err());
  }
}
