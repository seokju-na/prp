#![cfg(unix)]
#[path = "support/release_world.rs"]
mod release_world;
mod support;
use release_world::World;
use serde_json::json;
use std::{collections::BTreeMap, fs, os::unix::fs::PermissionsExt};

// This independent inventory is the release contract of the checked-in fixture.
const PUBLIC: &[(&str, &str)] = &[
  ("cargo", "fixture-wvb"),
  ("cargo", "fixture-wvb-tauri"),
  ("jsr", "@fixture/deno"),
  ("npm", "@fixture/config"),
  ("npm", "create-fixture-wvb"),
  ("npm", "@fixture/node"),
  ("npm", "@fixture/node-darwin-arm64"),
  ("npm", "@fixture/node-linux-x64-gnu"),
  ("npm", "@fixture/tauri"),
  ("npm", "@fixture/electron"),
  ("npm", "@fixture/remote"),
  ("npm", "@fixture/remote-local"),
];
const TAGS: &[&str] = &[
  "core",
  "config",
  "create-wvb",
  "node",
  "deno",
  "ffi",
  "tauri",
  "electron",
  "remote-base",
  "remote-local",
];

// An ignored test is also our executable mock publisher. The shell adapter forwards arguments
// through a per-process environment, so no Node/Python/package-manager runtime is required.
#[test]
#[ignore = "subprocess entry point; invoked by tools/mock-publish.sh"]
fn mock_publish() {
  release_world::publish();
}

fn start() -> World {
  let world = World::new();
  world.repo.commit("initial release workspace");
  world.repo.git(&["push", "-u", "origin", "main"]);
  world
}
fn baseline(world: &World) {
  for (registry, name) in PUBLIC {
    world.seed(registry, name, "0.3.0");
  }
  for tag in TAGS {
    world.repo.git(&["tag", &format!("{tag}/0.3.0")]);
  }
  world.repo.git(&["push", "origin", "--tags"]);
}
fn counts(world: &World, kind: &str) -> BTreeMap<String, usize> {
  let mut counts = BTreeMap::new();
  for e in world.events().iter().filter(|e| e["kind"] == kind) {
    *counts
      .entry(e["key"].as_str().unwrap().to_owned())
      .or_default() += 1;
  }
  counts
}
fn complete(world: &World, head: &str) {
  let published = counts(world, "publish");
  assert_eq!(published.len(), PUBLIC.len(), "{published:?}");
  assert!(published.values().all(|count| *count == 1));
  for (registry, name) in PUBLIC {
    let key = format!("{registry}|{name}|0.3.0");
    assert!(published.contains_key(&key), "missing {key}");
    assert_eq!(world.versions()[&key]["head"], head);
  }
  for tag in TAGS {
    assert_eq!(world.remote_tag(&format!("{tag}/0.3.0")), head);
  }
  assert_eq!(world.github.lock().unwrap().len(), TAGS.len());
  assert_eq!(
    world.repo.git(&["rev-parse", "HEAD"]),
    head,
    "publishing must not create a commit"
  );
  world.assert_clean();
}
fn assert_error(output: std::process::Output, expected: &str) {
  assert!(
    !output.status.success(),
    "unexpected success: {}",
    String::from_utf8_lossy(&output.stdout)
  );
  assert!(
    String::from_utf8_lossy(&output.stderr).contains(expected),
    "expected {expected}\nstdout:{}\nstderr:{}",
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  );
}

#[test]
fn initial_release_plans_publishes_native_artifacts_and_retries_without_changes() {
  let world = start();
  let head = world.repo.git(&["rev-parse", "HEAD"]);
  let plan = world.plan();
  assert_eq!(plan["head"], head);
  assert_eq!(plan["schema_version"], 1);
  assert_eq!(plan["packages"].as_array().unwrap().len(), TAGS.len());
  assert_eq!(
    plan["packages"]
      .as_array()
      .unwrap()
      .iter()
      .map(|p| p["manifests"].as_array().unwrap().len())
      .sum::<usize>(),
    PUBLIC.len()
  );
  assert!(world.events().is_empty());
  assert!(world.repo.git(&["tag"]).is_empty());
  world.run(&["release", "--dry-run"]);
  assert!(world.events().is_empty());
  assert!(
    !world
      .repo
      .path()
      .join("packages/node/npm/darwin-arm64/binding.node")
      .exists()
  );
  world.build();
  world.run(&["release", "--expected-head", &head]);
  complete(&world, &head);
  let events = world.events();
  let position = |key: &str| {
    events
      .iter()
      .position(|e| e["kind"] == "publish" && e["key"] == key)
      .unwrap()
  };
  for (dependency, dependent) in [
    ("cargo|fixture-wvb|0.3.0", "cargo|fixture-wvb-tauri|0.3.0"),
    (
      "npm|@fixture/node-darwin-arm64|0.3.0",
      "npm|@fixture/node|0.3.0",
    ),
    (
      "npm|@fixture/node-linux-x64-gnu|0.3.0",
      "npm|@fixture/node|0.3.0",
    ),
    ("npm|@fixture/node|0.3.0", "npm|@fixture/electron|0.3.0"),
    ("npm|@fixture/config|0.3.0", "npm|@fixture/remote|0.3.0"),
    (
      "npm|@fixture/remote|0.3.0",
      "npm|@fixture/remote-local|0.3.0",
    ),
  ] {
    assert!(
      position(dependency) < position(dependent),
      "{dependency} must precede {dependent}"
    );
  }
  let versions = world.versions();
  for platform in ["darwin-arm64", "linux-x64-gnu"] {
    let file = format!("fixture.{platform}.node");
    let bytes = fs::read(
      world
        .repo
        .path()
        .join(format!("packages/node/artifacts/{file}")),
    )
    .unwrap();
    assert_eq!(
      versions[format!("npm|@fixture/node-{platform}|0.3.0")]["files"]["binding.node"],
      json!(bytes)
    );
    let github = world.github.lock().unwrap();
    let asset = github["node/0.3.0"]["assets"]
      .as_array()
      .unwrap()
      .iter()
      .find(|a| a["name"] == file)
      .unwrap();
    assert_eq!(asset["bytes"], json!(bytes));
  }
  assert_eq!(
    versions["npm|@fixture/node|0.3.0"]["args"],
    json!(["--access=public", "--provenance", "--staged"])
  );
  assert_eq!(
    versions["cargo|fixture-wvb|0.3.0"]["args"],
    json!(["--locked"])
  );
  for (tag, count) in [("node/0.3.0", 2), ("ffi/0.3.0", 3), ("deno/0.3.0", 3)] {
    assert_eq!(
      world.github.lock().unwrap()[tag]["assets"]
        .as_array()
        .unwrap()
        .len(),
      count
    );
  }
  world.run(&["release"]);
  assert_eq!(
    world.events(),
    events,
    "a completed release must cause no writes or publisher invocations"
  );
  // A fresh checkout may have only remote tags. Inspect and reuse them without overwriting.
  for tag in TAGS {
    world.repo.git(&["tag", "-d", &format!("{tag}/0.3.0")]);
  }
  world.run(&["release"]);
  assert_eq!(world.events(), events);
  world
    .repo
    .write("README.md", "Documentation after release.\n");
  world.repo.commit("docs after release");
  assert!(world.plan()["packages"].as_array().unwrap().is_empty());
  world.assert_clean();
}

#[test]
fn squash_merge_and_later_commit_select_registry_versions_not_the_parent_diff() {
  let world = start();
  baseline(&world);
  world.repo.write(
    "packages/node/src/index.js",
    "export const fixtureValue = 4;\n",
  );
  world.repo.cli(&[
    "change",
    "--title",
    "Native initialization",
    "--body",
    "Initialize explicitly.",
  ]);
  world.repo.commit("new node API");
  world
    .repo
    .cli(&["prepare", "packages/node", "minor", "--skip-pr"]);
  let branch = world.repo.git(&[
    "for-each-ref",
    "--format=%(refname:short)",
    "refs/heads/prp",
  ]);
  let prepared = world.repo.git(&["rev-parse", &branch]);
  world.repo.git(&["merge", "--squash", &branch]);
  let merged = world.repo.commit("release: node v0.4.0 (squashed)");
  assert_ne!(prepared, merged);
  assert_eq!(
    fs::read_dir(world.repo.path().join(".prp/changes"))
      .map(|entries| entries.count())
      .unwrap_or(0),
    0
  );
  world.repo.write(
    "README.md",
    "An unrelated commit after release preparation.\n",
  );
  let head = world.repo.commit("docs after preparation");
  let plan = world.plan();
  assert_eq!(plan["packages"].as_array().unwrap().len(), 1);
  assert_eq!(plan["packages"][0]["tag"], "node/0.4.0");
  assert_eq!(
    plan["packages"][0]["manifests"].as_array().unwrap().len(),
    3
  );
  // CI can reject a changed checkout instead of silently publishing the later source.
  assert_error(
    world.raw(&["release", "--expected-head", &merged]),
    "HEAD differs",
  );
  assert!(world.events().is_empty());
  world.repo.git(&["checkout", "--detach", &merged]);
  world.build();
  world.run(&["release", "--expected-head", &merged]);
  assert_eq!(world.remote_tag("node/0.4.0"), merged);
  assert_ne!(world.remote_tag("node/0.4.0"), head);
  assert_eq!(counts(&world, "publish").len(), 3);
  assert_eq!(world.github.lock().unwrap().len(), 1);
  world.assert_clean();
}

#[test]
fn partial_multiregistry_failure_retries_only_missing_manifests_and_blocks_dependents() {
  let world = start();
  let head = world.repo.git(&["rev-parse", "HEAD"]);
  world.build();
  world.control(json!({"fail_publish":"npm|@fixture/node|0.3.0"}));
  assert_error(world.raw(&["release"]), "@fixture/node: npm publish failed");
  let published = counts(&world, "publish");
  assert!(published.contains_key("npm|@fixture/node-linux-x64-gnu|0.3.0"));
  assert!(!published.contains_key("npm|@fixture/electron|0.3.0"));
  assert!(!world.repo.git(&["tag"]).lines().any(|t| t == "node/0.3.0"));
  world.control(json!({}));
  world.run(&["release"]);
  complete(&world, &head);
  assert_eq!(counts(&world, "attempt")["npm|@fixture/node|0.3.0"], 2);
  assert_eq!(
    counts(&world, "attempt")["npm|@fixture/node-linux-x64-gnu|0.3.0"],
    1
  );
}

#[test]
fn registry_auth_and_outage_fail_before_any_mutation() {
  let world = start();
  for status in [401, 503] {
    world.faults.lock().unwrap().insert(
      "/jsr/scopes/fixture/packages/deno/versions/0.3.0".into(),
      status,
    );
    assert_error(world.raw(&["release"]), "cannot determine registry state");
    assert!(world.events().is_empty());
    assert!(world.repo.git(&["tag"]).is_empty());
  }
  world.faults.lock().unwrap().clear();
  world.assert_clean();
}

#[test]
fn conflicting_remote_tag_stops_before_publishing() {
  let world = start();
  world.repo.git(&["tag", "config/0.3.0"]);
  world
    .repo
    .git(&["push", "origin", "refs/tags/config/0.3.0"]);
  world.repo.write("README.md", "Later source\n");
  world.repo.commit("later source");
  assert_error(world.raw(&["release"]), "check out that commit");
  assert!(world.events().is_empty());
  world.repo.git(&["tag", "-f", "config/0.3.0"]);
  assert_error(
    world.raw(&["release"]),
    "local and remote tag config/0.3.0 point to different commits",
  );
  assert!(world.events().is_empty());
  world.assert_clean();
}

#[test]
fn tag_push_failure_recovers_without_republishing_or_moving_head() {
  let world = start();
  let head = world.repo.git(&["rev-parse", "HEAD"]);
  world.build();
  let hook = world.remote.path().join("hooks/pre-receive");
  fs::write(
    &hook,
    "#!/bin/sh\necho 'injected tag push failure' >&2\nexit 1\n",
  )
  .unwrap();
  fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
  assert_error(world.raw(&["release"]), "injected tag push failure");
  assert_eq!(counts(&world, "publish").len(), 1);
  assert!(world.github.lock().unwrap().is_empty());
  fs::remove_file(hook).unwrap();
  world.run(&["release"]);
  complete(&world, &head);
  assert!(counts(&world, "attempt").values().all(|n| *n == 1));
}

#[test]
fn asset_upload_failure_recovers_existing_release_and_only_missing_assets() {
  let world = start();
  let head = world.repo.git(&["rev-parse", "HEAD"]);
  world.build();
  world
    .faults
    .lock()
    .unwrap()
    .insert("upload:fixture.linux-x64-gnu.node".into(), 503);
  assert_error(
    world.raw(&["release"]),
    "uploading fixture.linux-x64-gnu.node failed",
  );
  assert_eq!(world.remote_tag("node/0.3.0"), head);
  assert_eq!(
    world.github.lock().unwrap()["node/0.3.0"]["assets"]
      .as_array()
      .unwrap()
      .len(),
    1
  );
  world.faults.lock().unwrap().clear();
  world.run(&["release"]);
  complete(&world, &head);
  assert!(counts(&world, "attempt").values().all(|n| *n == 1));
  assert_eq!(
    world
      .events()
      .iter()
      .filter(|e| e["kind"] == "upload" && e["name"] == "fixture.darwin-arm64.node")
      .count(),
    1
  );
}

#[test]
fn staged_npm_duplicate_rejections_are_retryable_without_duplicate_release_assets() {
  let world = start();
  let head = world.repo.git(&["rev-parse", "HEAD"]);
  world.control(json!({"stage_npm":true}));
  world.build();
  world.run(&["release"]);
  let before = world
    .events()
    .iter()
    .filter(|e| e["kind"] != "attempt")
    .cloned()
    .collect::<Vec<_>>();
  world.run(&["release"]);
  complete(&world, &head);
  assert_eq!(
    world
      .events()
      .iter()
      .filter(|e| e["kind"] != "attempt")
      .cloned()
      .collect::<Vec<_>>(),
    before
  );
  for (key, count) in counts(&world, "attempt") {
    assert_eq!(count, if key.starts_with("npm|") { 2 } else { 1 });
  }
}

#[test]
fn remote_annotated_tags_are_peeled_and_reused() {
  let world = start();
  let head = world.repo.git(&["rev-parse", "HEAD"]);
  for (registry, name) in PUBLIC {
    world.seed(registry, name, "0.3.0");
  }
  for tag in TAGS {
    let tag = format!("{tag}/0.3.0");
    world
      .repo
      .git(&["tag", "-a", &tag, "-m", "existing annotated release"]);
  }
  world.repo.git(&["push", "origin", "--tags"]);
  for tag in TAGS {
    world.repo.git(&["tag", "-d", &format!("{tag}/0.3.0")]);
  }
  world.build();
  world.run(&["release"]);
  assert!(counts(&world, "attempt").is_empty());
  for tag in TAGS {
    assert_eq!(world.remote_tag(&format!("{tag}/0.3.0")), head);
  }
  assert_eq!(world.github.lock().unwrap().len(), TAGS.len());
  world.assert_clean();
}

#[test]
fn malformed_registry_response_is_not_treated_as_an_unpublished_version() {
  let mut world = start();
  let malformed = world
    .server
    .mock("GET", "/cargo/fi/xt/fixture-wvb")
    .with_status(200)
    .with_body("not a cargo index")
    .expect(1)
    .create();
  assert_error(
    world.raw(&["release", "--check"]),
    "invalid crate index entry",
  );
  assert!(world.events().is_empty());
  assert!(world.repo.git(&["tag"]).is_empty());
  malformed.assert();
  world.assert_clean();
}

#[test]
fn github_release_creation_failure_recovers_after_the_tag_was_pushed() {
  let world = start();
  let head = world.repo.git(&["rev-parse", "HEAD"]);
  world.build();
  world
    .faults
    .lock()
    .unwrap()
    .insert("create-release".into(), 503);
  assert_error(world.raw(&["release"]), "failed to create GitHub release");
  assert_eq!(world.remote_tag("config/0.3.0"), head);
  assert!(world.github.lock().unwrap().is_empty());
  world.faults.lock().unwrap().clear();
  world.run(&["release"]);
  complete(&world, &head);
  assert!(counts(&world, "attempt").values().all(|n| *n == 1));
}

#[test]
fn missing_required_assets_stop_before_any_package_is_published() {
  let world = start();
  world.build();
  fs::remove_file(
    world
      .repo
      .path()
      .join(".prp/artifacts/packages/ffi/.output/android.zip"),
  )
  .unwrap();
  assert_error(
    world.raw(&["release"]),
    "asset pattern .output/android.zip matched no files",
  );
  assert!(world.events().is_empty());
  assert!(world.repo.git(&["tag"]).is_empty());
  world.assert_clean();
}

#[test]
fn before_publish_cannot_change_committed_source_or_versions() {
  let world = World::new();
  let mut config: toml::Value = toml::from_str(&world.repo.read(".prp/config.toml")).unwrap();
  config["packages"]["packages/node"]["before_publish_scripts"] =
    toml::Value::Array(vec![toml::Value::Table(toml::map::Map::from_iter([
      ("command".into(), "sh".into()),
      (
        "args".into(),
        toml::Value::Array(vec![
          "-c".into(),
          "printf 'changed source' > src/index.js".into(),
        ]),
      ),
    ]))]);
  world.repo.configure(&toml::to_string(&config).unwrap());
  world.repo.commit("configure bad hook");
  world.build();
  assert_error(
    world.raw(&["release"]),
    "release requires committed source files",
  );
  assert!(
    !counts(&world, "publish")
      .keys()
      .any(|key| key.contains("@fixture/node"))
  );
  assert!(!world.repo.git(&["tag"]).lines().any(|t| t == "node/0.3.0"));
}

#[test]
fn uncommitted_inputs_and_artifacts_overwriting_source_stop_before_publishing() {
  let world = start();
  world
    .repo
    .write("packages/config/src/extra.js", "untracked source\n");
  assert_error(world.raw(&["release"]), "commit untracked files");
  assert!(world.events().is_empty());
  fs::remove_file(world.repo.path().join("packages/config/src/extra.js")).unwrap();
  let mut config: toml::Value = toml::from_str(&world.repo.read(".prp/config.toml")).unwrap();
  config["packages"]["packages/config"]
    .as_table_mut()
    .unwrap()
    .insert(
      "artifacts".into(),
      toml::Value::Array(vec![toml::Value::Table(toml::map::Map::from_iter([(
        "source".into(),
        "src/index.js".into(),
      )]))]),
    );
  world.repo.configure(&toml::to_string(&config).unwrap());
  world.repo.commit("misconfigured source artifact");
  world.build();
  world.repo.write(
    ".prp/artifacts/packages/config/src/index.js",
    "wrong source\n",
  );
  assert_error(
    world.raw(&["release"]),
    "release requires committed source files",
  );
  assert!(world.events().is_empty());
  assert!(world.repo.git(&["tag"]).is_empty());
}

#[test]
fn asset_retries_paginate_and_reject_conflicting_remote_metadata() {
  let world = start();
  world.build();
  world.run(&["release"]);
  let events = world.events();
  {
    let mut github = world.github.lock().unwrap();
    let assets = github.get_mut("node/0.3.0").unwrap()["assets"]
      .as_array_mut()
      .unwrap();
    assets.splice(
      0..0,
      (0..100).map(|i| json!({"name":format!("other-{i}"),"size":1,"state":"uploaded"})),
    );
  }
  world.run(&["release"]);
  assert_eq!(
    world.events(),
    events,
    "assets on later pages must not be uploaded again"
  );
  for (field, value) in [("size", json!(0)), ("state", json!("starter"))] {
    let previous = {
      let mut github = world.github.lock().unwrap();
      let asset = &mut github.get_mut("node/0.3.0").unwrap()["assets"][100];
      std::mem::replace(&mut asset[field], value)
    };
    assert_error(
      world.raw(&["release"]),
      "is incomplete or has a different size",
    );
    assert_eq!(world.events(), events);
    world.github.lock().unwrap().get_mut("node/0.3.0").unwrap()["assets"][100][field] = previous;
  }
  world.assert_clean();
}

#[test]
fn duplicate_asset_filenames_fail_before_publication() {
  let world = World::new();
  let mut config: toml::Value = toml::from_str(&world.repo.read(".prp/config.toml")).unwrap();
  config["packages"]["packages/ffi"]["assets"] =
    toml::Value::Array(vec![".output/**/*.zip".into()]);
  world.repo.configure(&toml::to_string(&config).unwrap());
  world.repo.commit("duplicate asset names");
  world.build();
  world.repo.write(
    "packages/ffi/.output/duplicate/android.zip",
    "conflicting archive",
  );
  assert_error(
    world.raw(&["release"]),
    "GitHub assets must have unique filenames",
  );
  assert!(world.events().is_empty());
  assert!(world.repo.git(&["tag"]).is_empty());
  world.assert_clean();
}
