mod support;

use std::{
  collections::BTreeMap,
  fs,
  path::{Path, PathBuf},
};
use support::Repo;

fn monorepo() -> Repo {
  let mut repo = Repo::from_fixture("monorepo");
  for path in [
    "/npm/@fixture%2Fcore/1.0.0",
    "/npm/@fixture%2Fapp/1.0.0",
    "/npm/@fixture%2Fbridge/1.0.0",
    "/npm/@fixture%2Fbridge-linux-x64/1.0.0",
    "/jsr/scopes/fixture/packages/bridge/versions/1.0.0",
    "/jsr/scopes/fixture/packages/utils/versions/1.0.0",
  ] {
    repo
      .registry
      .mock("GET", path)
      .with_status(200)
      .with_body("{}")
      .create();
  }
  for name in ["core", "consumer", "bridge"] {
    repo
      .registry
      .mock("GET", format!("/cargo/pr/p-/prp-fixture-{name}").as_str())
      .with_status(200)
      .with_body("{\"vers\":\"1.0.0\"}\n")
      .create();
  }
  repo.commit("initial fixture workspace");
  for package in [
    "@fixture/core",
    "@fixture/app",
    "bridge",
    "prp-fixture-core",
    "prp-fixture-consumer",
    "@fixture/utils",
  ] {
    repo.git(&["tag", &format!("{package}-v1.0.0")]);
  }
  repo
}

fn tree(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
  walkdir::WalkDir::new(root)
    .into_iter()
    .filter_entry(|entry| entry.file_name() != ".git")
    .map(Result::unwrap)
    .filter(|entry| entry.file_type().is_file())
    .map(|entry| {
      (
        entry.path().strip_prefix(root).unwrap().to_owned(),
        fs::read(entry.path()).unwrap(),
      )
    })
    .collect()
}

fn json(repo: &Repo, path: &str) -> serde_json::Value {
  serde_json::from_str(&repo.read(path)).unwrap()
}

fn cargo(repo: &Repo, path: &str) -> toml::Value {
  toml::from_str(&repo.read(path)).unwrap()
}

#[test]
fn discovers_mixed_workspaces_and_propagates_dependencies_without_writing() {
  let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/monorepo");
  let original = tree(&source);
  for (package, file, dependents) in [
    (
      "packages/core",
      "packages/core/src/index.js",
      vec!["bridge", "@fixture/app"],
    ),
    (
      "crates/core",
      "crates/core/src/lib.rs",
      vec!["bridge", "@fixture/app", "prp-fixture-consumer"],
    ),
    (
      "deno/utils",
      "deno/utils/mod.ts",
      vec!["bridge", "@fixture/app"],
    ),
  ] {
    let repo = monorepo();
    repo.write(file, &repo.read(file).replace('1', "2"));
    repo.commit("update shared implementation");
    let files = tree(repo.path());
    let refs = repo.git(&["show-ref"]);
    let output = repo.cli(&["prepare", package, "minor", "--dry-run"]);
    for dependent in &dependents {
      assert!(
        output.contains(&format!("{dependent}: affected, not selected")),
        "{output}"
      );
    }
    assert_eq!(
      output.matches(": affected, not selected").count(),
      dependents.len(),
      "{output}"
    );
    assert_eq!(tree(repo.path()), files);
    assert_eq!(repo.git(&["show-ref"]), refs);
    assert!(repo.git(&["status", "--porcelain"]).is_empty());
  }
  assert_eq!(
    tree(&source),
    original,
    "tests must not modify checked-in fixtures"
  );
}

#[test]
fn prepares_selected_notes_nested_manifests_and_registry_links_on_a_release_branch() {
  let repo = monorepo();
  repo.write(
    "packages/bridge/index.js",
    "export const bridge = () => 2;\n",
  );
  repo.cli(&[
    "change",
    "--title",
    "Bridge initialization",
    "--body",
    "Bridge initialization is now explicit.",
    "--breaking",
  ]);
  let selected = repo.commit("feat: bridge initialization");
  // A separate note must survive because its commit is not selected.
  repo.write(
    ".prp/changes/later.md",
    "---\ntitle: Deferred bridge change\n---\n\nKeep for a later release.\n",
  );
  repo.write(
    "packages/bridge/internal/index.js",
    "export const internal = false;\n",
  );
  repo.commit("later bridge change");
  let files = tree(repo.path());
  let head = repo.git(&["rev-parse", "HEAD"]);
  repo.cli(&[
    "prepare-release",
    "packages/bridge",
    "minor",
    "--changes",
    &selected,
    "--skip-pr",
  ]);
  assert_eq!(repo.git(&["rev-parse", "HEAD"]), head);
  assert_eq!(
    tree(repo.path()),
    files,
    "prepare must preserve the current checkout"
  );
  let branch = repo.git(&[
    "for-each-ref",
    "--format=%(refname:short)",
    "refs/heads/prp",
  ]);
  assert!(branch.starts_with("prp/main_"));
  repo.git(&["merge", "--ff-only", &branch]);

  for path in [
    "packages/bridge/package.json",
    "packages/bridge/deno.json",
    "packages/bridge/npm/linux-x64/package.json",
    "packages/bridge/internal/package.json",
  ] {
    assert_eq!(repo.version(path), "1.1.0", "{path}");
  }
  assert_eq!(
    cargo(&repo, "packages/bridge/Cargo.toml")["package"]["version"].as_str(),
    Some("1.1.0")
  );
  let bridge = json(&repo, "packages/bridge/package.json");
  assert_eq!(
    bridge["optionalDependencies"]["@fixture/bridge-linux-x64"],
    "1.1.0"
  );
  assert_eq!(bridge["dependencies"]["@fixture/core"], "workspace:^");
  assert_eq!(repo.version("packages/core/package.json"), "1.0.0");
  assert_eq!(repo.version("packages/app/package.json"), "1.0.0");
  assert_eq!(repo.version("deno/utils/deno.json"), "1.0.0");
  assert_eq!(
    repo.read("crates/core/Cargo.toml"),
    fs::read_to_string(
      Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/monorepo/crates/core/Cargo.toml")
    )
    .unwrap()
  );

  let root_log = repo.read("CHANGELOG.md");
  assert!(root_log.contains("## `Native Bridge` v1.1.0"));
  assert!(root_log.contains("## Initial release"));
  for log in [root_log, repo.read("packages/bridge/CHANGELOG.md")] {
    assert!(log.contains("### **[BREAKING]** Bridge initialization"));
    assert!(log.contains("Bridge initialization is now explicit."));
    assert!(log.contains("feat: bridge initialization"));
    assert!(!log.contains("later bridge change"));
    assert!(!log.contains("Deferred bridge change"));
    assert!(!log.contains("bridge-internal"));
    for url in [
      "https://www.npmjs.com/package/@fixture/bridge/v/1.1.0",
      "https://www.npmjs.com/package/@fixture/bridge-linux-x64/v/1.1.0",
      "https://crates.io/crates/prp-fixture-bridge/1.1.0",
      "https://jsr.io/@fixture/bridge@1.1.0",
    ] {
      assert!(log.contains(url), "missing {url}: {log}");
    }
  }
  let mut notes = fs::read_dir(repo.path().join(".prp/changes"))
    .unwrap()
    .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
    .collect::<Vec<_>>();
  notes.sort();
  assert_eq!(notes, ["initial.md", "later.md"]);
  assert!(!repo.path().join(".prp/releases").exists());
  assert!(repo.git(&["status", "--porcelain"]).is_empty());
}

#[test]
fn updates_cargo_workspace_aliases_and_inherited_versions_in_source_manifests() {
  let repo = monorepo();
  repo.write("crates/core/src/lib.rs", "pub fn value() -> u32 { 2 }\n");
  repo.commit("native core changed");
  repo.cli(&["prepare", "crates/core", "minor", "--skip-commit"]);
  assert_eq!(
    cargo(&repo, "crates/core/Cargo.toml")["package"]["version"].as_str(),
    Some("1.1.0")
  );
  let workspace = cargo(&repo, "Cargo.toml");
  assert_eq!(
    workspace["workspace"]["dependencies"]["native_core"]["version"].as_str(),
    Some("^1.1.0")
  );
  assert_eq!(
    workspace["workspace"]["dependencies"]["native_core"]["package"].as_str(),
    Some("prp-fixture-core")
  );
  assert!(
    repo
      .read("Cargo.toml")
      .contains("# Keep this alias and comment.")
  );
  assert!(
    repo
      .read("packages/bridge/Cargo.toml")
      .contains("native_core.workspace = true")
  );
  repo.commit("release native core");
  repo.cli(&["prepare", "crates/consumer", "patch", "--skip-commit"]);
  assert_eq!(
    cargo(&repo, "Cargo.toml")["workspace"]["package"]["version"].as_str(),
    Some("1.0.1")
  );
  assert!(
    repo
      .read("crates/consumer/Cargo.toml")
      .contains("version.workspace = true")
  );
  repo.commit("release consumer");
  let output = repo.cli(&[
    "release",
    "--dry-run",
    "--skip-push",
    "--skip-github-release",
  ]);
  assert!(
    output.contains("Release prp-fixture-consumer v1.0.1"),
    "{output}"
  );
  assert_eq!(
    output
      .lines()
      .filter(|line| line.starts_with("Release ") && !line.starts_with("Release source:"))
      .count(),
    2
  );
}

#[cfg(unix)]
fn release_fixture(server: &mockito::ServerGuard) -> Repo {
  let repo = Repo::from_fixture("monorepo");
  let mut config: toml::Value = toml::from_str(&repo.read(".prp/config.toml")).unwrap();
  for registry in ["npm", "cargo", "jsr"] {
    config["packages"][format!("{registry}_publish_command")] = shell_words::join([
      "sh",
      repo.path().join("tools/publish.sh").to_str().unwrap(),
      registry,
    ])
    .into();
  }
  config["packages"]["packages/bridge"]
    .as_table_mut()
    .unwrap()
    .insert(
      "before_publish_scripts".into(),
      toml::Value::Array(vec![toml::Value::Table(toml::map::Map::from_iter([
        ("command".into(), "sh".into()),
        (
          "args".into(),
          toml::Value::Array(vec![
            repo
              .path()
              .join("tools/before-publish.sh")
              .to_str()
              .unwrap()
              .into(),
          ]),
        ),
      ]))]),
    );
  for (key, suffix) in [
    ("npm_url", "npm"),
    ("cargo_index_url", "cargo"),
    ("jsr_api_url", "jsr"),
  ] {
    config["registries"][key] = format!("{}/{suffix}", server.url()).into();
  }
  repo.configure(&toml::to_string_pretty(&config).unwrap());
  repo.commit("initial fixture workspace");
  for package in ["prp-fixture-core", "prp-fixture-consumer", "@fixture/utils"] {
    repo.git(&["tag", &format!("{package}-v1.0.0")]);
  }
  repo.write("packages/core/src/index.js", "export const value = 2;\n");
  let feature = repo.commit("update shared API");
  repo.git(&["checkout", "-b", "release/fixture"]);
  for package in ["packages/core", "packages/bridge", "packages/app"] {
    repo.cli(&[
      "prepare",
      package,
      "patch",
      "--changes",
      &feature,
      "--skip-commit",
    ]);
    repo.commit(&format!("prepare {package}"));
  }
  repo.git(&["checkout", "main"]);
  repo.write(
    "README.md",
    "Main branch documentation changed during release preparation.\n",
  );
  repo.commit("update main documentation");
  repo.git(&[
    "merge",
    "--no-ff",
    "release/fixture",
    "-m",
    "merge fixture release",
  ]);
  assert_eq!(
    repo
      .git(&["rev-list", "--parents", "-n", "1", "HEAD"])
      .split_whitespace()
      .count(),
    3
  );

  // Simulate a build job, artifact collection, and a clean release job.
  repo.write(
    "packages/bridge/build/bridge.node",
    "fixture-native\0binary\n",
  );
  repo.cli(&["artifacts"]);
  assert_eq!(
    repo.read(".prp/artifacts/packages/bridge/artifacts/bridge.node"),
    "fixture-native\0binary\n"
  );
  fs::remove_dir_all(repo.path().join("packages/bridge/build")).unwrap();
  repo
}

#[cfg(unix)]
fn registry_mocks(server: &mut mockito::ServerGuard, platform_exists: bool) -> Vec<mockito::Mock> {
  let mut mocks = Vec::new();
  for (path, status) in [
    ("/npm/@fixture%2Fcore/1.0.1", 404),
    ("/npm/@fixture%2Fapp/1.0.1", 404),
    ("/npm/@fixture%2Fbridge/1.0.1", 404),
    (
      "/npm/@fixture%2Fbridge-linux-x64/1.0.1",
      if platform_exists { 200 } else { 404 },
    ),
    ("/cargo/pr/p-/prp-fixture-bridge", 404),
    ("/jsr/scopes/fixture/packages/bridge/versions/1.0.1", 404),
  ] {
    mocks.push(
      server
        .mock("GET", path)
        .with_status(status)
        .with_body("{}")
        .expect(1)
        .create(),
    );
  }
  for (path, body) in [
    ("/cargo/pr/p-/prp-fixture-core", "{\"vers\":\"1.0.0\"}\n"),
    (
      "/cargo/pr/p-/prp-fixture-consumer",
      "{\"vers\":\"1.0.0\"}\n",
    ),
    ("/jsr/scopes/fixture/packages/utils/versions/1.0.0", "{}"),
  ] {
    mocks.push(
      server
        .mock("GET", path)
        .with_status(200)
        .with_body(body)
        .expect(1)
        .create(),
    );
  }
  mocks.push(
    server
      .mock("GET", "/npm/@fixture%2Fbridge-internal/1.0.1")
      .expect(0)
      .create(),
  );
  mocks
}

#[cfg(unix)]
#[test]
fn merged_release_restores_artifacts_publishes_in_order_and_skips_existing_versions() {
  let mut server = mockito::Server::new();
  let repo = release_fixture(&server);
  let mocks = registry_mocks(&mut server, true);
  let head = repo.git(&["rev-parse", "HEAD"]);
  let output = repo.cli(&["release", "--skip-push", "--skip-github-release"]);
  assert!(
    output.contains("@fixture/bridge-linux-x64@1.0.1: already published; skip"),
    "{output}"
  );
  let log = repo.read(".prp/publish.log");
  let lines: Vec<_> = log.lines().collect();
  assert_eq!(lines.len(), 6, "{log}");
  assert_eq!(lines[0], "npm:packages/core");
  assert_eq!(lines[1], "hook:packages/bridge");
  assert_eq!(lines[5], "npm:packages/app");
  let mut bridge = lines[2..5].to_vec();
  bridge.sort();
  assert_eq!(
    bridge,
    [
      "cargo:packages/bridge",
      "jsr:packages/bridge",
      "npm:packages/bridge"
    ]
  );
  assert_eq!(
    repo.read("packages/bridge/artifacts/bridge.node"),
    "fixture-native\0binary\n"
  );
  assert_eq!(
    repo.git(&["tag"]).lines().collect::<Vec<_>>(),
    [
      "@fixture/app-v1.0.1",
      "@fixture/core-v1.0.1",
      "@fixture/utils-v1.0.0",
      "bridge-v1.0.1",
      "prp-fixture-consumer-v1.0.0",
      "prp-fixture-core-v1.0.0"
    ]
  );
  for tag in [
    "@fixture/core-v1.0.1",
    "bridge-v1.0.1",
    "@fixture/app-v1.0.1",
  ] {
    assert_eq!(repo.git(&["rev-parse", tag]), head);
  }
  assert!(repo.git(&["status", "--porcelain"]).is_empty());
  for mock in mocks {
    mock.assert();
  }
}

#[cfg(unix)]
#[test]
fn failed_dependency_publication_stops_before_dependents_hooks_and_tags() {
  let mut server = mockito::Server::new();
  let repo = release_fixture(&server);
  let mocks = registry_mocks(&mut server, false);
  let output = repo
    .command(&["release", "--skip-push", "--skip-github-release"])
    .env("PRP_FIXTURE_FAIL", "npm:packages/core")
    .output()
    .unwrap();
  assert!(!output.status.success());
  let stderr = String::from_utf8_lossy(&output.stderr);
  assert!(
    stderr.contains("@fixture/core: npm publish failed"),
    "{stderr}"
  );
  assert_eq!(repo.read(".prp/publish.log"), "npm:packages/core\n");
  assert_eq!(repo.git(&["tag"]).lines().count(), 3);
  for mock in mocks {
    mock.assert();
  }
}
