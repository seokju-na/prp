use std::{
  fs,
  path::Path,
  process::{Command, Output},
};
use tempfile::TempDir;

pub fn isolate_git(command: &mut Command) -> &mut Command {
  command
    .env("GIT_CONFIG_NOSYSTEM", "1")
    .env("GIT_CONFIG_GLOBAL", "")
    .env("GIT_CONFIG_COUNT", "0")
    .env_remove("GIT_DIR")
    .env_remove("GIT_WORK_TREE")
    .env_remove("GIT_INDEX_FILE")
    .env_remove("GIT_OBJECT_DIRECTORY")
    .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
}

pub struct Repo {
  dir: TempDir,
  // Every test has a loopback registry by default. No implicit public registry requests.
  pub registry: mockito::ServerGuard,
}
#[allow(dead_code)]
impl Repo {
  pub fn from_fixture(name: &str) -> Self {
    let repo = Self::new();
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
      .join("tests/fixtures")
      .join(name);
    assert!(source.is_dir(), "missing fixture {}", source.display());
    for entry in walkdir::WalkDir::new(&source) {
      let entry = entry.unwrap();
      assert!(
        !entry.file_type().is_symlink(),
        "fixture symlinks are not supported"
      );
      if entry.file_type().is_file() {
        let destination = repo
          .path()
          .join(entry.path().strip_prefix(&source).unwrap());
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        fs::copy(entry.path(), destination).unwrap();
      }
    }
    let mut config: toml::Value = toml::from_str(&repo.read(".prp/config.toml")).unwrap();
    config.as_table_mut().unwrap().remove("registries");
    repo.configure(&toml::to_string(&config).unwrap());
    repo
  }

  pub fn new() -> Self {
    let mut registry = mockito::Server::new();
    registry
      .mock("GET", mockito::Matcher::Any)
      .expect(0)
      .with_status(404)
      .create();
    let repo = Self {
      dir: tempfile::tempdir().unwrap(),
      registry,
    };
    repo.git(&["init", "-b", "main"]);
    repo.git(&["config", "user.name", "Test"]);
    repo.git(&["config", "user.email", "test@example.invalid"]);
    repo.git(&["config", "commit.gpgsign", "false"]);
    repo.git(&["config", "tag.gpgsign", "false"]);
    repo.write(".gitignore", "/.prp/artifacts/\n/published.log\n");
    repo.configure("[packages]\ninclude = [\"packages/*\", \"crates/*\"]\n");
    repo
  }
  pub fn path(&self) -> &Path {
    self.dir.path()
  }
  pub fn write(&self, path: &str, text: &str) {
    let p = self.path().join(path);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
  }
  pub fn read(&self, path: &str) -> String {
    fs::read_to_string(self.path().join(path)).unwrap()
  }
  pub fn configure(&self, text: &str) {
    let mut config: toml::Value = toml::from_str(text).unwrap();
    if config.get("registries").is_none() {
      config.as_table_mut().unwrap().insert(
        "registries".into(),
        toml::Value::Table(toml::map::Map::from_iter(
          [
            ("npm_url", "npm"),
            ("cargo_index_url", "cargo"),
            ("jsr_api_url", "jsr"),
          ]
          .map(|(key, path)| (key.into(), format!("{}/{path}", self.registry.url()).into())),
        )),
      );
    }
    self.write(
      ".prp/config.toml",
      &toml::to_string_pretty(&config).unwrap(),
    );
  }
  pub fn git(&self, args: &[&str]) -> String {
    let result = isolate_git(&mut Command::new("git"))
      .args(args)
      .current_dir(self.path())
      .output()
      .unwrap();
    assert!(
      result.status.success(),
      "git {args:?}: {}",
      String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8(result.stdout).unwrap().trim().into()
  }
  pub fn commit(&self, msg: &str) -> String {
    self.git(&["add", "."]);
    self.git(&["commit", "-m", msg]);
    self.git(&["rev-parse", "HEAD"])
  }
  pub fn raw(&self, args: &[&str]) -> Output {
    self.command(args).output().unwrap()
  }
  pub fn command(&self, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_prp"));
    isolate_git(&mut command)
      .args(args)
      .current_dir(self.path())
      .env_remove("GITHUB_TOKEN")
      .env_remove("GH_TOKEN")
      .env_remove("PRP_FIXTURE_FAIL");
    command
  }
  pub fn cli(&self, args: &[&str]) -> String {
    let result = self.raw(args);
    assert!(
      result.status.success(),
      "prp {args:?}\nstdout: {}\nstderr: {}",
      String::from_utf8_lossy(&result.stdout),
      String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8(result.stdout).unwrap()
  }
  pub fn npm(&self, dir: &str, name: &str, version: &str, dependencies: &str) {
    self.write(
      &format!("{dir}/package.json"),
      &format!(r#"{{"name":"{name}","version":"{version}","dependencies":{dependencies}}}"#),
    );
    self.write(&format!("{dir}/index.js"), "export const value = 1;\n");
  }
  pub fn version(&self, path: &str) -> String {
    serde_json::from_str::<serde_json::Value>(&self.read(path)).unwrap()["version"]
      .as_str()
      .unwrap()
      .into()
  }
}
