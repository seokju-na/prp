use crate::{
  config::{GithubConfig, HttpConfig},
  git::Git,
  http_util::{self, HttpClient},
};
use anyhow::{Context, Result, ensure};
use reqwest::{
  Method, StatusCode, Url,
  blocking::{Body, RequestBuilder},
  header::{ACCEPT, CONTENT_LENGTH, CONTENT_TYPE},
};
use serde::Deserialize;
use serde_json::json;
use std::{
  cell::RefCell,
  collections::BTreeMap,
  fs::File,
  path::{Path, PathBuf},
  time::Duration,
};

type PullRequestInfo = Option<(String, String)>;

pub struct Github {
  http: HttpClient,
  api_url: String,
  owner: String,
  repo: String,
  token: Option<String>,
  commit_prs: RefCell<BTreeMap<String, PullRequestInfo>>,
}

impl Github {
  pub fn new(
    root: &Path,
    config: &GithubConfig,
    http_config: &HttpConfig,
    token: Option<String>,
  ) -> Result<Self> {
    let (owner, repo) = match (&config.owner, &config.repo) {
      (Some(owner), Some(repo)) => (owner.clone(), repo.clone()),
      _ => {
        let remote = Git {
          root: root.to_owned(),
        }
        .run(&["remote", "get-url", "origin"])
        .context("configure github.owner and github.repo, or add a GitHub origin remote")?;
        repository_from_remote(remote.trim(), &config.api_url)?
      }
    };
    ensure!(
      !owner.is_empty() && !repo.is_empty() && !owner.contains('/') && !repo.contains('/'),
      "invalid github.owner or github.repo"
    );
    Ok(Self {
      http: HttpClient::new(http_config)?,
      api_url: config.api_url.clone(),
      owner,
      repo,
      token,
      commit_prs: RefCell::new(BTreeMap::new()),
    })
  }

  fn request(&self, method: Method, path: &[&str]) -> Result<RequestBuilder> {
    let mut parts = vec!["repos", &self.owner, &self.repo];
    parts.extend(path);
    let mut builder = self
      .http
      .request(method, http_util::url(&self.api_url, &parts)?)
      .header(ACCEPT, "application/vnd.github+json")
      .header("X-GitHub-Api-Version", "2022-11-28");
    if let Some(token) = &self.token {
      builder = builder.bearer_auth(token);
    }
    Ok(builder)
  }

  pub fn preflight(&self) -> Result<()> {
    ensure!(
      self.token.as_ref().is_some_and(|t| !t.is_empty()),
      "GitHub writes require --github-token or GITHUB_TOKEN"
    );
    self
      .http
      .send(self.request(Method::GET, &[])?)?
      .ensure_success()
      .context("cannot access GitHub repository")
  }

  pub fn commit_pr(&self, sha: &str) -> Result<PullRequestInfo> {
    if let Some(cached) = self.commit_prs.borrow().get(sha) {
      return Ok(cached.clone());
    }
    #[derive(Deserialize)]
    struct User {
      login: String,
    }
    #[derive(Deserialize)]
    struct Pull {
      number: u64,
      user: User,
    }
    let result = (|| -> Result<PullRequestInfo> {
      let pulls: Vec<Pull> = self
        .http
        .send(self.request(Method::GET, &["commits", sha, "pulls"])?)?
        .json()?;
      Ok(
        pulls
          .into_iter()
          .next()
          .map(|pr| (pr.user.login, pr.number.to_string())),
      )
    })();
    // Metadata is optional; avoid repeating a failing lookup for every package's changelog.
    self
      .commit_prs
      .borrow_mut()
      .insert(sha.to_owned(), result.as_ref().ok().cloned().flatten());
    result
  }

  pub fn pull_request(
    &self,
    base: &str,
    branch: &str,
    title: &str,
    body: &str,
    config: &GithubConfig,
    draft: bool,
  ) -> Result<()> {
    #[derive(Deserialize)]
    struct Pull {
      number: u64,
      html_url: String,
    }
    let request = self
      .request(Method::POST, &["pulls"])?
      .json(&json!({"base": base, "head": branch, "title": title, "body": body, "draft": draft}));
    let pr: Pull = self
      .http
      .send(request)?
      .json()
      .context("failed to create GitHub PR")?;
    println!("Created PR: {}", pr.html_url);
    if !config.release_pr_labels.is_empty() {
      let request = self
        .request(Method::POST, &["issues", &pr.number.to_string(), "labels"])?
        .json(&json!({"labels": config.release_pr_labels}));
      self
        .http
        .send(request)?
        .ensure_success()
        .with_context(|| format!("PR #{} was created, but setting labels failed", pr.number))?;
    }
    Ok(())
  }

  pub fn release(
    &self,
    tag: &str,
    name: &str,
    body: &str,
    assets: &[PathBuf],
    prerelease: bool,
  ) -> Result<()> {
    #[derive(Deserialize)]
    struct Release {
      id: u64,
      html_url: String,
      upload_url: String,
    }
    #[derive(Deserialize)]
    struct Asset {
      name: String,
      size: u64,
      state: String,
    }
    let response = self
      .http
      .send(self.request(Method::GET, &["releases", "tags", tag])?)?;
    let existing = response.status != StatusCode::NOT_FOUND;
    let release: Release = if existing {
      response.json().context("cannot inspect GitHub release")?
    } else {
      let request = self.request(Method::POST, &["releases"])?
        .json(&json!({"tag_name": tag, "name": name, "body": body, "prerelease": prerelease, "draft": false}));
      self
        .http
        .send(request)?
        .json()
        .context("failed to create GitHub release")?
    };
    println!(
      "{} release: {}",
      if existing {
        "Using existing"
      } else {
        "Created"
      },
      release.html_url
    );
    let mut uploaded = BTreeMap::new();
    if existing && !assets.is_empty() {
      for page in 1.. {
        let request = self
          .request(
            Method::GET,
            &["releases", &release.id.to_string(), "assets"],
          )?
          .query(&[("per_page", 100), ("page", page)]);
        let batch: Vec<Asset> = self.http.send(request)?.json()?;
        let done = batch.len() < 100;
        for asset in batch {
          uploaded.insert(asset.name.clone(), asset);
        }
        if done {
          break;
        }
      }
    }
    for asset in assets {
      let base = release
        .upload_url
        .split('{')
        .next()
        .context("missing upload URL")?;
      let mut url = Url::parse(base).context("invalid GitHub upload URL")?;
      self.validate_upload_url(&url)?;
      let filename = asset
        .file_name()
        .and_then(|n| n.to_str())
        .context("invalid release asset filename")?;
      url.query_pairs_mut().append_pair("name", filename);
      let file =
        File::open(asset).with_context(|| format!("cannot open asset {}", asset.display()))?;
      if let Some(previous) = uploaded.get(filename) {
        ensure!(
          previous.state == "uploaded" && previous.size == file.metadata()?.len(),
          "release {tag}: existing asset {filename} is incomplete or has a different size; inspect it before retrying"
        );
        println!("Asset {filename} already uploaded; skip");
        continue;
      }
      let mut request = self
        .http
        .request(Method::POST, url)
        .header(ACCEPT, "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .header(CONTENT_TYPE, "application/octet-stream")
        .header(CONTENT_LENGTH, file.metadata()?.len())
        .timeout(Duration::from_secs(300))
        .body(Body::from(file));
      if let Some(token) = &self.token {
        request = request.bearer_auth(token);
      }
      self
        .http
        .send_once(request)?
        .ensure_success()
        .with_context(|| format!("release {tag} was created, but uploading {filename} failed"))?;
      println!("Uploaded {filename}");
    }
    Ok(())
  }

  fn validate_upload_url(&self, upload: &Url) -> Result<()> {
    let api = http_util::endpoint(&self.api_url)?;
    let public_upload = api.origin() == Url::parse("https://api.github.com")?.origin()
      && upload.origin() == Url::parse("https://uploads.github.com")?.origin();
    ensure!(
      (public_upload || upload.origin() == api.origin())
        && upload.username().is_empty()
        && upload.password().is_none(),
      "GitHub returned an unexpected asset-upload host"
    );
    Ok(())
  }
}

fn repository_from_remote(remote: &str, api_url: &str) -> Result<(String, String)> {
  let (host, path) = if remote.contains("://") {
    let url = Url::parse(remote).context("invalid Git origin URL")?;
    (
      url
        .host_str()
        .context("missing Git origin host")?
        .to_owned(),
      url.path().trim_matches('/').to_owned(),
    )
  } else {
    let (host, path) = remote
      .split_once(':')
      .context("origin is not a GitHub URL; configure github.owner and github.repo")?;
    (
      host.rsplit('@').next().unwrap_or(host).to_owned(),
      path.trim_matches('/').to_owned(),
    )
  };
  let api = http_util::endpoint(api_url)?;
  let expected_host = if api.host_str() == Some("api.github.com") {
    "github.com"
  } else {
    api.host_str().unwrap()
  };
  ensure!(
    host == expected_host,
    "origin is not on {expected_host}; configure github.owner, github.repo and github.api_url explicitly"
  );
  let path = path.strip_suffix(".git").unwrap_or(&path);
  let (owner, repo) = path
    .split_once('/')
    .context("origin must specify owner/repository")?;
  ensure!(
    !owner.is_empty() && !repo.is_empty() && !repo.contains('/'),
    "invalid GitHub repository path"
  );
  Ok((owner.into(), repo.into()))
}

#[cfg(test)]
mod tests {
  use super::*;

  fn client(api_url: String) -> Github {
    Github::new(
      Path::new("."),
      &GithubConfig {
        owner: Some("org".into()),
        repo: Some("repo".into()),
        api_url,
        ..Default::default()
      },
      &HttpConfig::default(),
      Some("test-token".into()),
    )
    .unwrap()
  }

  #[test]
  fn commit_pr_metadata_is_authenticated_and_cached() {
    let mut server = mockito::Server::new();
    let found = server
      .mock("GET", "/repos/org/repo/commits/abc/pulls")
      .match_header("authorization", "Bearer test-token")
      .with_body(r#"[{"number":42,"user":{"login":"author"}}]"#)
      .expect(1)
      .create();
    let failed = server
      .mock("GET", "/repos/org/repo/commits/def/pulls")
      .with_status(401)
      .expect(1)
      .create();
    let github = client(server.url());
    for _ in 0..2 {
      assert_eq!(
        github.commit_pr("abc").unwrap(),
        Some(("author".into(), "42".into()))
      );
    }
    assert!(github.commit_pr("def").is_err());
    assert_eq!(github.commit_pr("def").unwrap(), None);
    found.assert();
    failed.assert();
  }

  #[test]
  fn asset_upload_auth_is_limited_to_expected_origin() {
    let github = client("https://api.github.com".into());
    assert!(
      github
        .validate_upload_url(
          &Url::parse("https://uploads.github.com/repos/org/repo/releases/1/assets").unwrap()
        )
        .is_ok()
    );
    for url in [
      "https://untrusted.example/assets",
      "http://uploads.github.com/assets",
      "https://uploads.github.com:8443/assets",
      "https://user:pass@uploads.github.com/assets",
    ] {
      assert!(
        github
          .validate_upload_url(&Url::parse(url).unwrap())
          .is_err()
      );
    }
    let enterprise = client("https://git.example.com/api/v3".into());
    assert!(
      enterprise
        .validate_upload_url(&Url::parse("https://git.example.com/api/uploads/assets").unwrap())
        .is_ok()
    );
  }

  #[test]
  fn repository_inference_accepts_https_and_ssh() {
    for remote in [
      "https://github.com/org/repo.git",
      "git@github.com:org/repo.git",
      "ssh://git@github.com/org/repo.git",
    ] {
      assert_eq!(
        repository_from_remote(remote, "https://api.github.com").unwrap(),
        ("org".into(), "repo".into())
      );
    }
    assert!(
      repository_from_remote("git@gitlab.com:org/repo.git", "https://api.github.com").is_err()
    );
    assert_eq!(
      repository_from_remote(
        "git@git.example.com:org/repo.git",
        "https://git.example.com/api/v3"
      )
      .unwrap(),
      ("org".into(), "repo".into())
    );
  }
}
