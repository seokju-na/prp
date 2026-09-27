mod support;

use std::{fs, process::Command};
use support::Repo;

fn registry_api_config(server: &mockito::ServerGuard) -> String {
  format!(
    "\n[registries]\nnpm_url = '{0}/npm'\ncargo_index_url = '{0}/cargo'\njsr_api_url = '{0}/jsr'\n[http]\nretry_base_ms = 0\ntimeout_secs = 2\n",
    server.url()
  )
}

#[test]
fn prepare_consumes_selected_commit_notes_and_preserves_others() {
  let r = Repo::new();
  r.npm("packages/core", "@test/core", "1.0.0", "{}");
  r.commit("initial");
  r.git(&["tag", "@test/core-v1.0.0"]);
  r.write("packages/core/index.js", "export const value = 2;\n");
  r.write(
    ".prp/changes/1.md",
    "---\ntitle: Initialization improved\nbreaking: true\n---\n\nDetailed explanation.\n",
  );
  let selected = r.commit("improve initialization");
  r.write("packages/core/other.js", "export default true;\n");
  r.write(
    ".prp/changes/2.md",
    "---\ntitle: Another change\n---\n\nKeep this note.\n",
  );
  r.commit("another change");
  r.cli(&[
    "prepare",
    "@test/core",
    "minor",
    "--changes",
    &selected,
    "--skip-commit",
  ]);
  assert_eq!(r.version("packages/core/package.json"), "1.1.0");
  let log = r.read("CHANGELOG.md");
  assert!(log.contains("## `@test/core` v1.1.0"));
  assert!(log.contains("### **[BREAKING]** Initialization improved"));
  assert!(!log.contains("Another change"));
  assert!(!r.path().join(".prp/changes/1.md").exists());
  assert!(r.path().join(".prp/changes/2.md").exists());
  assert!(!r.path().join(".prp/releases").exists());
}

#[test]
fn workspace_discovery_display_name_and_dependency_changes() {
  let r = Repo::new();
  r.write(
    ".prp/config.toml",
    "[packages.\"packages/app\"]\ndisplay_name = \"Application\"\n",
  );
  r.write(
    "package.json",
    r#"{"private":true,"workspaces":["packages/*"]}"#,
  );
  r.npm("packages/core", "@test/core", "1.0.0", "{}");
  r.npm(
    "packages/app",
    "@test/app",
    "1.0.0",
    r#"{"@test/core":"workspace:^"}"#,
  );
  r.commit("initial");
  r.git(&["tag", "@test/core-v1.0.0"]);
  r.git(&["tag", "@test/app-v1.0.0"]);
  r.write("packages/core/index.js", "export const value = 2;\n");
  r.commit("core changed without a conventional scope");
  let preview = r.cli(&["prepare", "packages/core", "patch", "--dry-run"]);
  assert!(preview.contains("@test/app: affected, not selected"));
  r.cli(&["prepare", "packages/app", "patch", "--skip-commit"]);
  assert!(r.read("CHANGELOG.md").contains("## `Application` v1.0.1"));
  assert!(
    r.read("CHANGELOG.md")
      .contains("core changed without a conventional scope")
  );
  assert_eq!(r.version("packages/core/package.json"), "1.0.0");
  assert!(r.read("packages/app/package.json").contains("workspace:^"));
}

#[test]
fn explicit_commit_note_added_later_is_consumed() {
  let r = Repo::new();
  r.npm("packages/a", "a", "0.1.0", "{}");
  let sha = r.commit("feature added");
  r.cli(&[
    "change",
    "--title",
    "Notable feature",
    "--body",
    "Explanation",
    "--commit",
    &sha,
  ]);
  r.commit("describe previous change");
  r.cli(&["prepare", "a", "minor", "--changes", &sha, "--skip-commit"]);
  assert!(
    r.read("packages/a/CHANGELOG.md")
      .contains("Notable feature")
  );
  assert_eq!(
    fs::read_dir(r.path().join(".prp/changes")).unwrap().count(),
    0
  );
}

#[test]
fn prepare_branch_preserves_checkout_and_staging() {
  let r = Repo::new();
  r.npm("packages/a", "a", "0.1.0", "{}");
  r.commit("initial");
  r.write("unrelated.txt", "staged user work\n");
  r.git(&["add", "unrelated.txt"]);
  r.write("unrelated.txt", "unstaged user work\n");
  let head = r.git(&["rev-parse", "HEAD"]);
  let index = r.git(&["write-tree"]);
  r.cli(&["prepare", "a", "patch", "--skip-pr", "--allow-dirty"]);
  assert_eq!(r.git(&["rev-parse", "HEAD"]), head);
  assert_eq!(r.git(&["write-tree"]), index);
  assert_eq!(r.read("unrelated.txt"), "unstaged user work\n");
  assert_eq!(r.version("packages/a/package.json"), "0.1.0");
  let branch = r.git(&[
    "for-each-ref",
    "--format=%(refname:short)",
    "refs/heads/prp",
  ]);
  assert!(branch.starts_with("prp/main_"));
  let manifest = r.git(&["show", &format!("{branch}:packages/a/package.json")]);
  assert!(manifest.contains("0.1.1"));
  assert!(
    !r.git(&["ls-tree", "--name-only", &branch])
      .contains("unrelated.txt")
  );
}

#[test]
fn dry_run_does_not_write_notes_manifests_or_refs() {
  let r = Repo::new();
  r.npm("packages/a", "a", "1.2.3", "{}");
  r.commit("initial");
  let refs = r.git(&["show-ref"]);
  r.cli(&["prepare", "a", "minor", "--dry-run"]);
  assert_eq!(r.version("packages/a/package.json"), "1.2.3");
  assert_eq!(r.git(&["show-ref"]), refs);
  assert!(r.git(&["status", "--porcelain"]).is_empty());
}

#[test]
fn release_compares_first_parent_and_ignores_new_or_reverted_packages() {
  let r = Repo::new();
  r.npm("packages/a", "a", "1.0.0", "{}");
  r.commit("initial");
  assert!(r.cli(&["release", "--dry-run"]).contains("no parent"));
  r.npm("packages/b", "b", "1.0.0", "{}");
  r.commit("add package");
  assert!(
    r.cli(&["release", "--dry-run"])
      .contains("No versions increased")
  );
  r.npm("packages/a", "a", "1.0.1", "{}");
  r.commit("release: a");
  let result = r.cli(&["release", "--dry-run"]);
  assert!(result.contains("Release a v1.0.1"));
  assert!(!result.contains("Release b"));
  assert!(r.git(&["tag"]).is_empty());
  r.npm("packages/a", "a", "1.0.0", "{}");
  r.commit("revert release");
  assert!(
    r.cli(&["release", "--dry-run"])
      .contains("No versions increased")
  );
}

#[test]
fn merge_release_compares_first_parent() {
  let r = Repo::new();
  r.npm("packages/a", "a", "1.0.0", "{}");
  r.commit("initial");
  r.git(&["checkout", "-b", "release"]);
  r.npm("packages/a", "a", "1.0.1", "{}");
  r.commit("bump");
  r.git(&["checkout", "main"]);
  r.write("README.md", "ordinary main commit");
  r.commit("docs");
  r.git(&["merge", "--no-ff", "release", "-m", "merge release"]);
  assert!(
    r.cli(&["release", "--dry-run"])
      .contains("Release a v1.0.1")
  );
}

#[test]
fn cargo_workspace_inheritance_aliases_and_comments() {
  let r = Repo::new();
  r.write(".prp/config.toml", "[packages]\ninclude = []\n");
  r.write("Cargo.toml", "[workspace]\nmembers = [\"crates/*\"]\n[workspace.package]\nversion = \"0.2.0\" # shared version\n[workspace.dependencies]\nrenamed = { package = \"core-lib\", path = \"crates/core\", version = \"0.1.0\" } # keep this\n");
  r.write(
    "crates/core/Cargo.toml",
    "[package]\nname = \"core-lib\"\nversion = \"0.1.0\"\n",
  );
  r.write("crates/app/Cargo.toml", "[package]\nname = \"app\"\nversion.workspace = true\n[dependencies]\nrenamed.workspace = true\n");
  r.commit("initial");
  r.cli(&["prepare", "core-lib", "minor", "--skip-commit"]);
  let workspace = r.read("Cargo.toml");
  assert!(workspace.contains("version = \"0.2.0\" } # keep this"));
  assert!(workspace.contains("# shared version"));
  r.commit("release core");
  r.cli(&["prepare", "app", "patch", "--skip-commit"]);
  assert!(
    r.read("Cargo.toml")
      .contains("version = \"0.2.1\" # shared version")
  );
  assert!(
    r.read("crates/app/Cargo.toml")
      .contains("version.workspace = true")
  );
  r.commit("release app");
  let output = r.cli(&["release", "--dry-run"]);
  assert!(output.contains("Release app v0.2.1"));
  assert!(!output.contains("Release core-lib"));
}

#[test]
fn artifacts_round_trip_with_destination() {
  let r = Repo::new();
  r.write(".prp/config.toml", "[packages]\ninclude = [\"packages/*\"]\n[packages.\"packages/a\"]\nartifacts = [{ source = [\"build/*.node\"], dest = \"artifacts/\" }]\n");
  r.npm("packages/a", "a", "0.1.0", "{}");
  r.write("packages/a/build/a.node", "binary content");
  r.cli(&["artifacts"]);
  assert_eq!(
    r.read(".prp/artifacts/packages/a/artifacts/a.node"),
    "binary content"
  );
  fs::remove_file(r.path().join("packages/a/build/a.node")).unwrap();
  r.cli(&["artifacts", "--restore"]);
  assert_eq!(r.read("packages/a/artifacts/a.node"), "binary content");
}

#[cfg(unix)]
#[test]
fn mocked_publish_orders_dependencies_and_stops_on_failure() {
  let mut server = mockito::Server::new();
  let lookup = server
    .mock("GET", mockito::Matcher::Any)
    .with_status(404)
    .expect(2)
    .create();
  let r = Repo::new();
  r.write(
    "publish.sh",
    "#!/bin/sh\nprintf '%s\\n' \"$PWD\" >> \"$1\"\ncase \"$PWD\" in */core) exit 9;; esac\n",
  );
  let config = format!(
    "[packages]\ninclude = [\"packages/*\"]\nnpm_publish_command = 'sh {} {}'\n",
    r.path().join("publish.sh").display(),
    r.path().join("published.log").display()
  );
  r.write(
    ".prp/config.toml",
    &(config + &registry_api_config(&server)),
  );
  r.npm("packages/app", "app", "1.0.0", r#"{"core":"^1.0.0"}"#);
  r.npm("packages/core", "core", "1.0.0", "{}");
  r.commit("initial");
  r.npm("packages/app", "app", "1.0.1", r#"{"core":"^1.0.1"}"#);
  r.npm("packages/core", "core", "1.0.1", "{}");
  r.commit("release both");
  let output = r.raw(&["release", "--skip-push", "--skip-github-release"]);
  assert!(!output.status.success());
  assert!(String::from_utf8_lossy(&output.stderr).contains("core: npm publish failed"));
  let published = r.read("published.log");
  assert_eq!(published.lines().count(), 1);
  assert!(published.trim().ends_with("/core"));
  assert!(r.git(&["tag"]).is_empty());
  lookup.assert();
}

#[test]
fn unknown_config_dirty_targets_and_version_regressions_are_errors() {
  let r = Repo::new();
  r.npm("packages/a", "a", "1.0.0", "{}");
  r.commit("initial");
  assert!(
    !r.raw(&["prepare", "a", "0.9.0", "--skip-commit"])
      .status
      .success()
  );
  r.npm("packages/a", "a", "1.1.0", "{}");
  let dirty = r.raw(&["prepare", "a", "patch", "--allow-dirty", "--skip-commit"]);
  assert!(!dirty.status.success());
  assert!(String::from_utf8_lossy(&dirty.stderr).contains("overwrite local changes"));
  r.write(
    ".prp/config.toml",
    "[packages]\ninculde = [\"packages/*\"]\n",
  );
  assert!(!r.raw(&["release", "--dry-run"]).status.success());
}

#[test]
fn nested_manifests_get_one_version_and_display_scope() {
  let r = Repo::new();
  r.npm(
    "packages/node",
    "@test/node",
    "0.1.0",
    r#"{"@test/node-linux":"0.1.0"}"#,
  );
  r.npm("packages/node/npm/linux", "@test/node-linux", "0.1.0", "{}");
  r.write(
    "packages/node/Cargo.toml",
    "[package]\nname = \"node-native\"\nversion = \"0.1.0\"\n",
  );
  r.commit("initial");
  r.cli(&["prepare", "node", "minor", "--skip-commit"]);
  assert_eq!(r.version("packages/node/npm/linux/package.json"), "0.2.0");
  let manifest: serde_json::Value =
    serde_json::from_str(&r.read("packages/node/package.json")).unwrap();
  assert_eq!(manifest["dependencies"]["@test/node-linux"], "0.2.0");
  assert!(r.read("CHANGELOG.md").contains("## `node` v0.2.0"));
  r.commit("release node");
  let log = r.cli(&["release", "--dry-run"]);
  assert!(log.contains("cargo publish"));
  assert!(log.contains("npm publish"));
  assert!(
    log.find("/npm/linux: npm publish").unwrap() < log.find("/packages/node: npm publish").unwrap()
  );
}

#[test]
fn pnpm_workspace_exclusions_and_transitive_dependency_candidates() {
  let r = Repo::new();
  r.write(".prp/config.toml", "[packages]\ninclude = []\n");
  r.write(
    "pnpm-workspace.yaml",
    "packages:\n  - 'packages/*'\n  - '!packages/ignored'\n",
  );
  r.npm("packages/a", "a", "1.0.0", "{}");
  r.npm("packages/b", "b", "1.0.0", r#"{"a":"workspace:^"}"#);
  r.npm("packages/c", "c", "1.0.0", r#"{"b":"workspace:*"}"#);
  r.npm("packages/ignored", "ignored", "1.0.0", "{}");
  r.commit("initial");
  for name in ["a", "b", "c"] {
    r.git(&["tag", &format!("{name}-v1.0.0")]);
  }
  r.write("packages/a/index.js", "export const changed = true;\n");
  r.commit("deep dependency update");
  r.cli(&["prepare", "c", "patch", "--skip-commit"]);
  assert!(
    r.read("packages/c/CHANGELOG.md")
      .contains("deep dependency update")
  );
  assert_eq!(r.version("packages/ignored/package.json"), "1.0.0");
}

#[test]
fn dev_dependencies_do_not_propagate_and_ignored_changes_stay_out() {
  let r = Repo::new();
  r.npm("packages/core", "core", "1.0.0", "{}");
  r.write(
    "packages/app/package.json",
    r#"{"name":"app","version":"1.0.0","devDependencies":{"core":"workspace:*"}}"#,
  );
  r.commit("initial");
  r.git(&["tag", "core-v1.0.0"]);
  r.git(&["tag", "app-v1.0.0"]);
  r.write("packages/core/README.md", "docs only");
  r.commit("docs-only update");
  r.write("packages/core/index.js", "export const changed = true;");
  r.commit("code update");
  let out = r.cli(&["prepare", "core", "patch", "--skip-commit"]);
  assert!(!out.contains("app: affected"));
  let log = r.read("packages/core/CHANGELOG.md");
  assert!(log.contains("code update"));
  assert!(!log.contains("docs-only update"));
}

#[test]
fn release_rejects_dirty_source_and_dry_run_never_executes_hooks() {
  let r = Repo::new();
  r.write(".prp/config.toml", "[packages]\ninclude = [\"packages/*\"]\n[packages.\"packages/a\"]\nbefore_publish_scripts = [{ command = \"definitely-not-an-executable\" }]\n");
  r.npm("packages/a", "a", "1.0.0", "{}");
  r.commit("initial");
  r.npm("packages/a", "a", "1.0.1", "{}");
  r.commit("release a");
  assert!(
    r.cli(&["release", "--dry-run"])
      .contains("definitely-not-an-executable")
  );
  r.write("packages/a/index.js", "uncommitted source");
  let out = r.raw(&["release", "--skip-push", "--skip-github-release"]);
  assert!(!out.status.success());
  assert!(String::from_utf8_lossy(&out.stderr).contains("committed source"));
}

#[cfg(unix)]
#[test]
fn all_registries_publish_with_mock_commands_and_tag_after_success() {
  let mut server = mockito::Server::new();
  let lookup = server
    .mock("GET", mockito::Matcher::Any)
    .with_status(404)
    .expect(3)
    .create();
  let r = Repo::new();
  r.write(
    "publish.sh",
    "#!/bin/sh\nprintf '%s:%s\\n' \"$1\" \"$PWD\" >> \"$2\"\n",
  );
  let script = r.path().join("publish.sh");
  let log = r.path().join("published.log");
  let mut config = String::from("[packages]\ninclude = [\"packages/*\"]\n");
  for registry in ["npm", "cargo", "jsr"] {
    config.push_str(&format!(
      "{registry}_publish_command = 'sh {} {registry} {}'\n",
      script.display(),
      log.display()
    ));
  }
  r.write(
    ".prp/config.toml",
    &(config + &registry_api_config(&server)),
  );
  r.npm("packages/a", "a", "1.0.0", "{}");
  r.write(
    "packages/a/Cargo.toml",
    "[package]\nname = \"native-a\"\nversion = \"1.0.0\"\n",
  );
  r.write(
    "packages/a/deno.json",
    r#"{"name":"@test/a","version":"1.0.0","exports":"./index.js"}"#,
  );
  r.commit("initial");
  r.cli(&["prepare", "a", "patch", "--skip-commit"]);
  r.commit("release a");
  r.cli(&["release", "--skip-push", "--skip-github-release"]);
  let out = r.read("published.log");
  assert_eq!(out.lines().count(), 3);
  for registry in ["npm", "cargo", "jsr"] {
    assert!(out.contains(&format!("{registry}:")));
  }
  assert_eq!(r.git(&["tag"]), "a-v1.0.1");
  assert_eq!(
    r.git(&["rev-parse", "a-v1.0.1"]),
    r.git(&["rev-parse", "HEAD"])
  );
  lookup.assert();
}

#[test]
fn prerelease_bump_sets_npm_channel_and_release_removes_pre() {
  let r = Repo::new();
  r.npm("packages/a", "a", "1.2.3", "{}");
  r.commit("initial");
  r.cli(&["prepare", "a", "preminor", "--skip-commit"]);
  assert_eq!(r.version("packages/a/package.json"), "1.3.0-rc.0");
  r.commit("release prerelease");
  assert!(r.cli(&["release", "--dry-run"]).contains("--tag=rc"));
  r.cli(&["prepare", "a", "prerelease", "--skip-commit"]);
  assert_eq!(r.version("packages/a/package.json"), "1.3.0-rc.1");
  r.commit("next prerelease");
  r.cli(&["prepare", "a", "release", "--skip-commit"]);
  assert_eq!(r.version("packages/a/package.json"), "1.3.0");
}

#[test]
fn artifacts_can_collect_from_dist_and_target_and_dry_run_is_read_only() {
  let r = Repo::new();
  r.write(".prp/config.toml", "[packages]\ninclude = [\"packages/*\"]\n[packages.\"packages/a\"]\nartifacts = [{ source = [\"dist/*.js\", \"target/*.node\"] }]\n");
  r.npm("packages/a", "a", "0.1.0", "{}");
  r.write("packages/a/dist/a.js", "javascript");
  r.write("packages/a/target/a.node", "binary");
  r.cli(&["artifacts", "--dry-run"]);
  assert!(!r.path().join(".prp/artifacts").exists());
  r.cli(&["artifacts"]);
  assert_eq!(r.read(".prp/artifacts/packages/a/dist/a.js"), "javascript");
  assert_eq!(r.read(".prp/artifacts/packages/a/target/a.node"), "binary");
}

#[test]
fn skip_publish_is_persisted_in_config_without_a_plan_file() {
  let r = Repo::new();
  r.npm("packages/a", "a", "1.0.0", "{}");
  r.commit("initial");
  r.cli(&["prepare", "a", "patch", "--skip-publish", "--skip-commit"]);
  assert!(r.read(".prp/config.toml").contains("skip_publish = true"));
  r.commit("release a");
  let out = r.cli(&["release", "--dry-run"]);
  assert!(out.contains("Release a v1.0.1"));
  assert!(!out.contains("npm publish"));
}

#[test]
fn github_flow_calls_api_with_auth_json_labels_and_binary_asset() {
  use mockito::Matcher;
  use serde_json::json;
  let mut server = mockito::Server::new();
  let r = Repo::new();
  r.npm("packages/a", "a", "1.0.0", "{}");
  r.write("packages/a/assets/test +#.zip", "binary\0payload");
  r.write(".prp/config.toml", &format!("[packages]\ninclude = [\"packages/*\"]\n[packages.\"packages/a\"]\nskip_publish = true\nassets = [\"assets/*.zip\"]\n[github]\nowner = \"example\"\nrepo = \"example\"\napi_url = '{}'\nrelease_pr_labels = [\"release\"]\n", server.url()));
  let sha = r.commit("initial");
  let notes = format!("### Commits\n\n- initial by Test ({})", &sha[..7]);
  let pr_body = format!("Prepared releases:\n\n## a v1.0.1\n\n{notes}\n\n");
  let access = server
    .mock("GET", "/repos/example/example")
    .match_header("authorization", "Bearer test-token")
    .match_header("user-agent", "prp/0.1.0")
    .match_header("x-github-api-version", "2022-11-28")
    .with_status(200)
    .with_body("{}")
    .expect(2)
    .create();
  let pr = server
    .mock("POST", "/repos/example/example/pulls")
    .match_header("authorization", "Bearer test-token")
    .match_body(Matcher::PartialJson(
      json!({"base":"main", "title":"release: a v1.0.1", "body":pr_body, "draft":true}),
    ))
    .with_status(201)
    .with_body(r#"{"number":42,"html_url":"https://github.com/example/example/pull/42"}"#)
    .create();
  let labels = server
    .mock("POST", "/repos/example/example/issues/42/labels")
    .match_header("authorization", "Bearer test-token")
    .match_body(Matcher::Json(json!({"labels":["release"]})))
    .with_status(200)
    .with_body("[]")
    .create();
  let release = server.mock("POST", "/repos/example/example/releases")
    .match_header("authorization", "Bearer test-token")
    .match_body(Matcher::Json(json!({"tag_name":"a-v1.0.1","name":"a v1.0.1","body":notes,"prerelease":false,"draft":false})))
    .with_status(201).with_body(json!({"html_url":"https://github.com/example/example/releases/tag/a-v1.0.1", "upload_url":format!("{}/uploads/7/assets{{?name,label}}", server.url())}).to_string()).create();
  let upload = server
    .mock("POST", "/uploads/7/assets")
    .match_query(Matcher::UrlEncoded("name".into(), "test +#.zip".into()))
    .match_header("authorization", "Bearer test-token")
    .match_header("content-type", "application/octet-stream")
    .match_body("binary\0payload")
    .with_status(201)
    .with_body("{}")
    .create();
  let remote = tempfile::tempdir().unwrap();
  assert!(
    Command::new("git")
      .args(["init", "--bare"])
      .arg(remote.path())
      .output()
      .unwrap()
      .status
      .success()
  );
  r.git(&["remote", "add", "origin", remote.path().to_str().unwrap()]);
  r.git(&["push", "-u", "origin", "main"]);
  let run = |args: &[&str]| {
    let out = Command::new(env!("CARGO_BIN_EXE_prp"))
      .args(args)
      .current_dir(r.path())
      .env("GITHUB_TOKEN", "test-token")
      .output()
      .unwrap();
    assert!(
      out.status.success(),
      "{}\n{}",
      String::from_utf8_lossy(&out.stdout),
      String::from_utf8_lossy(&out.stderr)
    );
  };
  run(&["prepare", "a", "patch"]);
  let branch = r.git(&[
    "for-each-ref",
    "--format=%(refname:short)",
    "refs/heads/prp",
  ]);
  r.git(&["merge", "--ff-only", &branch]);
  run(&["release"]);
  access.assert();
  pr.assert();
  labels.assert();
  release.assert();
  upload.assert();
  let remote_tag = Command::new("git")
    .arg("--git-dir")
    .arg(remote.path())
    .args(["rev-parse", "refs/tags/a-v1.0.1"])
    .output()
    .unwrap();
  assert!(remote_tag.status.success());
  assert_eq!(
    String::from_utf8(remote_tag.stdout).unwrap().trim(),
    r.git(&["rev-parse", "HEAD"])
  );
}

#[test]
fn excluded_nested_package_is_not_absorbed_by_root() {
  let r = Repo::new();
  r.write(
    ".prp/config.toml",
    "[packages]\ninclude = [\".\", \"packages/*\"]\nexclude = [\"packages/ignored\"]\n",
  );
  r.npm(".", "root", "1.0.0", "{}");
  r.npm("packages/ignored", "ignored", "1.0.0", "{}");
  r.commit("initial");
  r.cli(&["prepare", "root", "minor", "--skip-commit"]);
  assert_eq!(r.version("package.json"), "1.1.0");
  assert_eq!(r.version("packages/ignored/package.json"), "1.0.0");
}

#[test]
fn changelog_registry_links_use_next_version_and_respect_visibility() {
  for show in [None, Some(false)] {
    let r = Repo::new();
    if let Some(show) = show {
      r.write(
        ".prp/config.toml",
        &format!("[packages]\ninclude = [\"packages/*\"]\n[changelog]\nshow_registries = {show}\n"),
      );
    }
    r.npm("packages/node", "@scope/node", "1.0.0", "{}");
    r.write(
      "packages/node/Cargo.toml",
      "[package]\nname = \"native\"\nversion = \"1.0.0\"\n",
    );
    r.write(
      "packages/node/deno.json",
      r#"{"name":"@scope/deno","version":"1.0.0"}"#,
    );
    r.write(
      "packages/node/private/package.json",
      r#"{"name":"secret","version":"1.0.0","private":true}"#,
    );
    r.commit("initial");
    r.cli(&["prepare", "node", "minor", "--skip-commit"]);
    for file in ["CHANGELOG.md", "packages/node/CHANGELOG.md"] {
      let log = r.read(file);
      for url in [
        "https://www.npmjs.com/package/@scope/node/v/1.1.0",
        "https://crates.io/crates/native/1.1.0",
        "https://jsr.io/@scope/deno@1.1.0",
      ] {
        assert_eq!(log.contains(url), show.unwrap_or(true), "{file}: {log}");
      }
      assert!(!log.contains("/package/secret/"));
    }
  }
}

#[test]
fn already_published_manifest_skips_hooks_and_missing_artifacts() {
  let mut server = mockito::Server::new();
  let lookup = server
    .mock("GET", "/npm/@scope%2Fa/1.0.1")
    .with_status(200)
    .with_body("{}")
    .create();
  let r = Repo::new();
  r.write(".prp/config.toml", &("[packages]\ninclude = [\"packages/*\"]\nnpm_publish_command = 'must-not-run'\n[packages.\"packages/a\"]\nbefore_publish_scripts = [{command = 'must-not-run'}]\nartifacts = [{source = '*.node'}]\n".to_owned() + &registry_api_config(&server)));
  r.npm("packages/a", "@scope/a", "1.0.0", "{}");
  r.commit("initial");
  r.npm("packages/a", "@scope/a", "1.0.1", "{}");
  r.commit("release");
  assert!(
    r.cli(&["release", "--skip-push", "--skip-github-release"])
      .contains("already published; skip")
  );
  assert_eq!(r.git(&["tag"]), "@scope/a-v1.0.1");
  lookup.assert();
}

#[cfg(unix)]
#[test]
fn publish_only_missing_registries_and_retry_lookup_errors() {
  let mut server = mockito::Server::new();
  let npm = server
    .mock("GET", "/npm/@scope%2Fa/1.0.1")
    .with_status(200)
    .with_body("{}")
    .create();
  let cargo = server
    .mock("GET", "/cargo/na/ti/native-a")
    .with_status(200)
    .with_body("{\"vers\":\"1.0.1\",\"yanked\":true}\n")
    .create();
  let jsr = server
    .mock("GET", "/jsr/scopes/scope/packages/a/versions/1.0.1")
    .with_status(503)
    .expect(3)
    .create();
  let r = Repo::new();
  r.write("publish.sh", "#!/bin/sh\nprintf '%s' \"$1\" >> \"$2\"\n");
  let mut config = String::from("[packages]\ninclude = [\"packages/*\"]\n");
  for registry in ["npm", "cargo", "jsr"] {
    config.push_str(&format!(
      "{registry}_publish_command = 'sh {} {registry} {}'\n",
      r.path().join("publish.sh").display(),
      r.path().join("published.log").display()
    ));
  }
  config.push_str(&registry_api_config(&server));
  r.write(".prp/config.toml", &config);
  r.npm("packages/a", "@scope/a", "1.0.0", "{}");
  r.write(
    "packages/a/Cargo.toml",
    "[package]\nname = \"native-a\"\nversion = \"1.0.0\"\n",
  );
  r.write(
    "packages/a/deno.json",
    r#"{"name":"@scope/a","version":"1.0.0"}"#,
  );
  r.commit("initial");
  r.cli(&["prepare", "a", "patch", "--skip-commit"]);
  r.commit("release");
  let result = r.raw(&["release", "--skip-push", "--skip-github-release"]);
  assert!(
    result.status.success(),
    "{}",
    String::from_utf8_lossy(&result.stderr)
  );
  assert!(String::from_utf8_lossy(&result.stderr).contains("registry lookup failed"));
  assert_eq!(r.read("published.log"), "jsr");
  npm.assert();
  cargo.assert();
  jsr.assert();
}

#[test]
fn registry_dry_run_makes_no_http_requests() {
  let mut server = mockito::Server::new();
  let never = server.mock("GET", mockito::Matcher::Any).expect(0).create();
  let r = Repo::new();
  r.write(
    ".prp/config.toml",
    &("[packages]\ninclude = ['packages/*']\n".to_owned() + &registry_api_config(&server)),
  );
  r.npm("packages/a", "a", "1.0.0", "{}");
  r.commit("initial");
  r.npm("packages/a", "a", "1.0.1", "{}");
  r.commit("release");
  r.cli(&["release", "--dry-run"]);
  never.assert();
}
