//! Stateful, loopback-only release services shared by black-box CLI scenarios.
//! A publisher subprocess writes receipts; HTTP registry reads observe those same receipts.
use crate::support::{Repo, isolate_git};
use mockito::{Matcher, Request, ServerGuard};
use serde_json::{Value, json};
use std::{
  collections::BTreeMap,
  fs,
  io::Write,
  path::{Path, PathBuf},
  process::{Command, Output},
  sync::{Arc, Mutex},
};

pub struct World {
  pub repo: Repo,
  pub remote: tempfile::TempDir,
  pub server: ServerGuard,
  pub github: Arc<Mutex<BTreeMap<String, Value>>>,
  pub faults: Arc<Mutex<BTreeMap<String, usize>>>,
  unexpected: Arc<Mutex<Vec<String>>>,
}

pub fn read_json(path: &Path) -> Value {
  serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}
pub fn write_json(path: &Path, value: &Value) {
  let mut temp = tempfile::NamedTempFile::new_in(path.parent().unwrap()).unwrap();
  serde_json::to_writer(temp.as_file_mut(), value).unwrap();
  temp.persist(path).unwrap();
}
pub fn event(root: &Path, value: Value) {
  let mut f = fs::OpenOptions::new()
    .create(true)
    .append(true)
    .open(root.join(".prp/mock/events.jsonl"))
    .unwrap();
  writeln!(f, "{value}").unwrap();
}
fn decode(path: &str) -> String {
  let mut bytes = Vec::new();
  let mut input = path.bytes();
  while let Some(byte) = input.next() {
    if byte == b'%' {
      let hex = [input.next().unwrap(), input.next().unwrap()];
      bytes.push(u8::from_str_radix(std::str::from_utf8(&hex).unwrap(), 16).unwrap());
    } else {
      bytes.push(byte);
    }
  }
  String::from_utf8(bytes).unwrap()
}
fn registry_response(root: &Path, request: &Request) -> (usize, String) {
  let state = read_json(&root.join(".prp/mock/versions.json"));
  let path = decode(request.path());
  let parts: Vec<_> = path.trim_start_matches('/').split('/').collect();
  if parts[0] == "cargo" {
    let name = parts.last().unwrap();
    let entries = state
      .as_object()
      .unwrap()
      .iter()
      .filter(|(key, _)| key.starts_with(&format!("cargo|{name}|")))
      .map(|(key, _)| {
        json!({"name":name,"vers":key.rsplit('|').next().unwrap(),"yanked":false}).to_string()
      })
      .collect::<Vec<_>>();
    return (
      if entries.is_empty() { 404 } else { 200 },
      entries.join("\n"),
    );
  }
  let (registry, name, version) = match parts.as_slice() {
    ["npm", scope, name, version] => ("npm", format!("{scope}/{name}"), *version),
    ["npm", name, version] => ("npm", name.to_string(), *version),
    [
      "jsr",
      "scopes",
      scope,
      "packages",
      name,
      "versions",
      version,
    ] => ("jsr", format!("@{scope}/{name}"), *version),
    _ => return (500, format!("unexpected registry path {path}")),
  };
  let public = state
    .get(format!("{registry}|{name}|{version}"))
    .is_some_and(|v| v["visibility"] == "public");
  (
    if public { 200 } else { 404 },
    json!({"name":name,"version":version}).to_string(),
  )
}

impl World {
  pub fn new() -> Self {
    let repo = Repo::from_fixture("webview-bundle");
    repo.write(".prp/mock/versions.json", "{}");
    repo.write(".prp/mock/control.json", "{}");
    let remote = tempfile::tempdir().unwrap();
    assert!(
      isolate_git(&mut Command::new("git"))
        .args(["init", "--bare"])
        .arg(remote.path())
        .output()
        .unwrap()
        .status
        .success()
    );
    repo.git(&["remote", "add", "origin", remote.path().to_str().unwrap()]);
    let mut server = mockito::Server::new();
    let github = Arc::new(Mutex::new(BTreeMap::<String, Value>::new()));
    let faults = Arc::new(Mutex::new(BTreeMap::<String, usize>::new()));
    let unexpected = Arc::new(Mutex::new(Vec::new()));
    for method in ["GET", "POST"] {
      let unexpected = unexpected.clone();
      server
        .mock(method, Matcher::Any)
        .expect(0)
        .with_status(500)
        .with_body_from_request(move |r| {
          let message = format!("{} {}", r.method(), r.path_and_query());
          unexpected.lock().unwrap().push(message.clone());
          message.into_bytes()
        })
        .create();
    }
    let status_root = repo.path().to_owned();
    let body_root = status_root.clone();
    let registry_faults = faults.clone();
    server
      .mock("GET", Matcher::Regex("^/(npm|cargo|jsr)/".into()))
      .expect(0)
      .with_status_code_from_request(move |r| {
        registry_faults
          .lock()
          .unwrap()
          .get(r.path())
          .copied()
          .unwrap_or_else(|| registry_response(&status_root, r).0)
      })
      .with_body_from_request(move |r| registry_response(&body_root, r).1.into_bytes())
      .create();
    server
      .mock("GET", "/repos/fixture/workspace")
      .match_header("authorization", "Bearer fixture-token")
      .with_body("{}")
      .create();
    let state = github.clone();
    let response_state = github.clone();
    server
      .mock(
        "GET",
        Matcher::Regex("^/repos/fixture/workspace/releases/tags/".into()),
      )
      .match_header("authorization", "Bearer fixture-token")
      .with_status_code_from_request(move |r| {
        if state
          .lock()
          .unwrap()
          .contains_key(&decode(r.path().split("/tags/").nth(1).unwrap()))
        {
          200
        } else {
          404
        }
      })
      .with_body_from_request(move |r| {
        response_state
          .lock()
          .unwrap()
          .get(&decode(r.path().split("/tags/").nth(1).unwrap()))
          .unwrap_or(&Value::Null)
          .to_string()
          .into_bytes()
      })
      .create();
    let state = github.clone();
    let release_faults = faults.clone();
    let release_body_faults = faults.clone();
    let url = server.url();
    let root = repo.path().to_owned();
    server
      .mock("POST", "/repos/fixture/workspace/releases")
      .match_header("authorization", "Bearer fixture-token")
      .with_status_code_from_request(move |_| {
        *release_faults
          .lock()
          .unwrap()
          .get("create-release")
          .unwrap_or(&201)
      })
      .with_body_from_request(move |r| {
        if release_body_faults
          .lock()
          .unwrap()
          .contains_key("create-release")
        {
          return b"{}".to_vec();
        }
        let mut release: Value = serde_json::from_slice(r.body().unwrap()).unwrap();
        let tag = release["tag_name"].as_str().unwrap().to_owned();
        let mut state = state.lock().unwrap();
        // A duplicate POST is a test failure, not a successful idempotent operation.
        assert!(
          !state.contains_key(&tag),
          "duplicate release creation: {tag}"
        );
        let id = state.len() + 1;
        release["id"] = id.into();
        release["html_url"] = format!("{url}/releases/{id}").into();
        release["upload_url"] = format!("{url}/uploads/{id}/assets{{?name,label}}").into();
        release["assets"] = json!([]);
        state.insert(tag.clone(), release.clone());
        event(&root, json!({"kind":"release","tag":tag}));
        release.to_string().into_bytes()
      })
      .create();
    let state = github.clone();
    server
      .mock(
        "GET",
        Matcher::Regex("^/repos/fixture/workspace/releases/[0-9]+/assets$".into()),
      )
      .match_query(Matcher::Any)
      .match_header("authorization", "Bearer fixture-token")
      .with_body_from_request(move |r| {
        let id: u64 = r.path().split('/').nth(5).unwrap().parse().unwrap();
        let url = reqwest::Url::parse(&format!("http://localhost{}", r.path_and_query())).unwrap();
        let query: BTreeMap<_, _> = url.query_pairs().into_owned().collect();
        let page: usize = query["page"].parse().unwrap();
        let per_page: usize = query["per_page"].parse().unwrap();
        assert!(page > 0 && per_page > 0 && per_page <= 100);
        let state = state.lock().unwrap();
        let assets = state.values().find(|v| v["id"] == id).unwrap()["assets"]
          .as_array()
          .unwrap();
        let start = ((page - 1) * per_page).min(assets.len());
        serde_json::to_vec(&assets[start..(start + per_page).min(assets.len())]).unwrap()
      })
      .create();
    let state = github.clone();
    let upload_faults = faults.clone();
    let body_faults = faults.clone();
    let root = repo.path().to_owned();
    server.mock("POST", Matcher::Regex("^/uploads/[0-9]+/assets$".into()))
      .match_query(Matcher::Any)
      .match_header("authorization", "Bearer fixture-token")
      .match_header("content-type", "application/octet-stream")
      .with_status_code_from_request(move |r| {
        let url = reqwest::Url::parse(&format!("http://localhost{}", r.path_and_query())).unwrap();
        let name = url.query_pairs().find(|(k,_)| k == "name").unwrap().1.into_owned();
        *upload_faults.lock().unwrap().get(&format!("upload:{name}")).unwrap_or(&201)
      })
      .with_body_from_request(move |r| {
        let url = reqwest::Url::parse(&format!("http://localhost{}", r.path_and_query())).unwrap();
        let name = url.query_pairs().find(|(k,_)| k == "name").unwrap().1.into_owned();
        if body_faults.lock().unwrap().contains_key(&format!("upload:{name}")) { return b"{}".to_vec(); }
        let id: u64 = r.path().split('/').nth(2).unwrap().parse().unwrap();
        let mut state = state.lock().unwrap();
        let release = state.values_mut().find(|v| v["id"] == id).unwrap();
        let assets = release["assets"].as_array_mut().unwrap();
        assert!(!assets.iter().any(|a| a["name"] == name), "duplicate asset upload: {name}");
        assets.push(json!({"name":name,"size":r.body().unwrap().len(),"state":"uploaded","bytes":r.body().unwrap()}));
        event(&root, json!({"kind":"upload","name":name}));
        b"{}".to_vec()
      }).create();
    let mut config: toml::Value = toml::from_str(&repo.read(".prp/config.toml")).unwrap();
    for registry in ["npm", "cargo", "jsr"] {
      config["packages"][format!("{registry}_publish_command")] = shell_words::join([
        "sh",
        repo.path().join("tools/mock-publish.sh").to_str().unwrap(),
        registry,
      ])
      .into();
    }
    for (key, path) in [
      ("npm_url", "npm"),
      ("cargo_index_url", "cargo"),
      ("jsr_api_url", "jsr"),
    ] {
      config["registries"][key] = format!("{}/{path}", server.url()).into();
    }
    config["github"]["api_url"] = server.url().into();
    repo.configure(&toml::to_string(&config).unwrap());
    Self {
      repo,
      remote,
      server,
      github,
      faults,
      unexpected,
    }
  }
  pub fn command(&self, args: &[&str]) -> Command {
    let mut command = self.repo.command(args);
    command
      .env("PRP_MOCK_EXECUTABLE", std::env::current_exe().unwrap())
      .env("GITHUB_TOKEN", "fixture-token");
    command
  }
  pub fn raw(&self, args: &[&str]) -> Output {
    self.command(args).output().unwrap()
  }
  pub fn run(&self, args: &[&str]) -> String {
    let out = self.raw(args);
    assert!(
      out.status.success(),
      "prp {args:?}\n{}\n{}",
      String::from_utf8_lossy(&out.stdout),
      String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
  }
  pub fn plan(&self) -> Value {
    serde_json::from_str(&self.run(&["release", "--check", "--json"])).unwrap()
  }
  pub fn events(&self) -> Vec<Value> {
    fs::read_to_string(self.repo.path().join(".prp/mock/events.jsonl"))
      .unwrap_or_default()
      .lines()
      .map(|l| serde_json::from_str(l).unwrap())
      .collect()
  }
  pub fn versions(&self) -> Value {
    read_json(&self.repo.path().join(".prp/mock/versions.json"))
  }
  pub fn control(&self, value: Value) {
    write_json(&self.repo.path().join(".prp/mock/control.json"), &value);
  }
  pub fn seed(&self, registry: &str, name: &str, version: &str) {
    let mut state = self.versions();
    state[format!("{registry}|{name}|{version}")] = json!({"visibility":"public"});
    write_json(&self.repo.path().join(".prp/mock/versions.json"), &state);
  }
  pub fn build(&self) {
    let head = self.repo.git(&["rev-parse", "HEAD"]);
    // Each build job collects only its own package's output. Other jobs need not exist yet.
    for platform in ["darwin-arm64", "linux-x64-gnu"] {
      self.repo.write(
        &format!("packages/node/fixture.{platform}.node"),
        &format!("native\0{head}:{platform}"),
      );
      self.run(&["artifacts", "--package", "packages/node"]);
      fs::remove_file(
        self
          .repo
          .path()
          .join(format!("packages/node/fixture.{platform}.node")),
      )
      .unwrap();
    }
    for file in [
      "android.zip",
      "apple.zip",
      "WebViewBundleFFI.xcframework.zip",
    ] {
      self.repo.write(
        &format!("packages/ffi/.output/{file}"),
        &format!("archive:{head}:{file}"),
      );
      self.run(&["artifacts", "--package", "packages/ffi"]);
      fs::remove_file(
        self
          .repo
          .path()
          .join(format!("packages/ffi/.output/{file}")),
      )
      .unwrap();
    }
    for file in [
      "libfixture-darwin.dylib",
      "libfixture-linux.so",
      "libfixture-linux.so.sha256",
    ] {
      self.repo.write(
        &format!("packages/deno/.output/{file}"),
        &format!("deno:{head}:{file}"),
      );
    }
    self.run(&["artifacts", "--package", "packages/deno"]);
    fs::remove_dir_all(self.repo.path().join("packages/deno/.output")).unwrap();
  }
  pub fn remote_tag(&self, tag: &str) -> String {
    let output = Command::new("git")
      .arg("--git-dir")
      .arg(self.remote.path())
      .args(["rev-parse", &format!("refs/tags/{tag}^{{commit}}")])
      .output()
      .unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap().trim().into()
  }
  pub fn assert_clean(&self) {
    assert!(
      self.unexpected.lock().unwrap().is_empty(),
      "unexpected HTTP requests: {:?}",
      self.unexpected
    );
    assert!(self.repo.git(&["status", "--porcelain"]).is_empty());
  }
}

/// Runs in a separate test-harness process, called only through the fixture shell adapter.
/// It reads real manifests and files from the publisher's working directory.
pub fn publish() {
  let registry =
    std::env::var("PRP_MOCK_REGISTRY").expect("mock publisher must be launched by the fixture");
  let cwd = std::env::current_dir().unwrap();
  let output = Command::new("git")
    .args(["rev-parse", "--show-toplevel"])
    .output()
    .unwrap();
  assert!(output.status.success());
  let root = PathBuf::from(String::from_utf8(output.stdout).unwrap().trim());
  let control = read_json(&root.join(".prp/mock/control.json"));
  let args = std::env::var("PRP_MOCK_ARGS").unwrap();
  let (name, version, files, dependencies) = match registry.as_str() {
    "npm" | "jsr" => {
      let manifest = read_json(&cwd.join(if registry == "npm" {
        "package.json"
      } else {
        "deno.json"
      }));
      assert_ne!(
        manifest["private"], true,
        "private package reached publisher"
      );
      let files = manifest["files"]
        .as_array()
        .map(|v| v.iter().map(|f| f.as_str().unwrap().to_owned()).collect())
        .unwrap_or_else(|| vec!["mod.ts".into()]);
      let mut deps = Vec::new();
      for table in ["dependencies", "optionalDependencies"] {
        if let Some(entries) = manifest[table].as_object() {
          deps.extend(entries.keys().cloned());
        }
      }
      (
        manifest["name"].as_str().unwrap().to_owned(),
        manifest["version"].as_str().unwrap().to_owned(),
        files,
        deps,
      )
    }
    "cargo" => {
      let manifest: toml::Value =
        toml::from_str(&fs::read_to_string(cwd.join("Cargo.toml")).unwrap()).unwrap();
      assert_ne!(manifest["package"]["publish"].as_bool(), Some(false));
      let deps = if manifest.get("dependencies").is_some() {
        vec!["fixture-wvb".into()]
      } else {
        vec![]
      };
      (
        manifest["package"]["name"].as_str().unwrap().to_owned(),
        manifest["package"]["version"].as_str().unwrap().to_owned(),
        vec!["src/lib.rs".into()],
        deps,
      )
    }
    _ => panic!("unknown mock registry {registry}"),
  };
  let key = format!("{registry}|{name}|{version}");
  let mut state = read_json(&root.join(".prp/mock/versions.json"));
  let relative = cwd.strip_prefix(&root).unwrap().to_string_lossy();
  event(
    &root,
    json!({"kind":"attempt","key":key,"cwd":relative,"args":args.lines().collect::<Vec<_>>()}),
  );
  if control["fail_publish"] == key {
    panic!("injected publish failure for {key}");
  }
  if state.get(&key).is_some() {
    // Exercise the production duplicate classifier for staged npm versions invisible to GET.
    assert_eq!(registry, "npm");
    eprintln!(
      "npm ERR! EPUBLISHCONFLICT: You cannot publish over the previously published versions"
    );
    std::process::exit(1);
  }
  for dep in dependencies {
    assert!(
      state
        .as_object()
        .unwrap()
        .keys()
        .any(|k| k.starts_with(&format!("{registry}|{dep}|"))),
      "dependency {dep} not yet published before {key}"
    );
  }
  let packed: BTreeMap<String, Vec<u8>> = files
    .into_iter()
    .map(|file: String| {
      let bytes = fs::read(cwd.join(&file))
        .unwrap_or_else(|e| panic!("missing publish input {relative}/{file}: {e}"));
      (file, bytes)
    })
    .collect();
  let head = Command::new("git")
    .args(["rev-parse", "HEAD"])
    .output()
    .unwrap();
  let head = String::from_utf8(head.stdout).unwrap().trim().to_owned();
  let visibility = if registry == "npm" && control["stage_npm"] == true {
    "staged"
  } else {
    "public"
  };
  state[&key] = json!({"visibility":visibility,"head":head,"files":packed,"cwd":relative,"args":args.lines().collect::<Vec<_>>()});
  write_json(&root.join(".prp/mock/versions.json"), &state);
  event(&root, json!({"kind":"publish","key":key,"head":head}));
}
