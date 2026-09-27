use anyhow::{Result, bail, ensure};
use semver::{BuildMetadata, Prerelease, Version};

pub fn bump(current: &Version, rule: &str, preid: &str) -> Result<Version> {
  if let Ok(exact) = Version::parse(rule) {
    ensure!(
      exact.cmp_precedence(current).is_gt(),
      "{exact} must be greater than {current}"
    );
    return Ok(exact);
  }
  let mut next = current.clone();
  next.build = BuildMetadata::EMPTY;
  let increment = |n: u64| {
    n.checked_add(1)
      .ok_or_else(|| anyhow::anyhow!("version overflow"))
  };
  match rule {
    "major" | "premajor" => {
      if rule == "premajor" || current.pre.is_empty() || current.minor != 0 || current.patch != 0 {
        next.major = increment(next.major)?;
      }
      next.minor = 0;
      next.patch = 0;
      next.pre = Prerelease::EMPTY;
    }
    "minor" | "preminor" => {
      if rule == "preminor" || current.pre.is_empty() || current.patch != 0 {
        next.minor = increment(next.minor)?;
      }
      next.patch = 0;
      next.pre = Prerelease::EMPTY;
    }
    "patch" | "prepatch" => {
      if rule == "prepatch" || current.pre.is_empty() {
        next.patch = increment(next.patch)?;
      }
      next.pre = Prerelease::EMPTY;
    }
    "prerelease" => {
      if current.pre.is_empty() {
        next.patch = increment(next.patch)?;
        next.pre = Prerelease::new(&format!("{preid}.0"))?;
      } else {
        let mut parts: Vec<String> = current.pre.as_str().split('.').map(str::to_owned).collect();
        if let Some(last) = parts.last_mut().filter(|p| p.parse::<u64>().is_ok()) {
          *last = increment(last.parse()?)?.to_string();
        } else {
          parts.push("0".into());
        }
        next.pre = Prerelease::new(&parts.join("."))?;
      }
    }
    "release" => {
      ensure!(
        !current.pre.is_empty(),
        "{current} is already a stable version"
      );
      next.pre = Prerelease::EMPTY;
    }
    _ => bail!(
      "unknown bump type '{rule}'; use patch, minor, major, premajor, preminor, prepatch, prerelease, release, or an exact version"
    ),
  }
  if matches!(rule, "premajor" | "preminor" | "prepatch") {
    next.pre = Prerelease::new(&format!("{preid}.0"))?;
  }
  ensure!(
    next.cmp_precedence(current).is_gt(),
    "{next} must be greater than {current}"
  );
  Ok(next)
}
