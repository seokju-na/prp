use crate::config::{HttpConfig, ResolvedGitHubConfig};
use crate::git::Git;
use crate::http::HttpClient;
use anyhow::{Context, Result, ensure};
use reqwest::header::{ACCEPT, CONTENT_LENGTH, CONTENT_TYPE};
use reqwest::{Method, RequestBuilder, Response, StatusCode, Url};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::json;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

const PUBLIC_API_HOST: &str = "api.github.com";
const PUBLIC_UPLOAD_ORIGIN: &str = "https://uploads.github.com";
const API_VERSION: &str = "2022-11-28";
const PAGE_SIZE: usize = 100;
const UPLOAD_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Debug, Clone)]
pub struct PullRequest {
  pub base: String,
  pub head: String,
  pub title: String,
  pub body: String,
  pub draft: bool,
  pub labels: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Release {
  pub tag: String,
  pub title: String,
  pub body: String,
  pub prerelease: bool,
  pub assets: Vec<PathBuf>,
}

#[derive(Debug, Deserialize)]
struct PullResponse {
  number: u64,
  html_url: String,
}

#[derive(Debug, Deserialize)]
struct ReleaseResponse {
  id: u64,
  html_url: String,
  upload_url: String,
}

#[derive(Debug, Deserialize)]
struct AssetResponse {
  id: u64,
  name: String,
  size: u64,
  state: String,
}

/// The repository's GitHub API. Every write is safe to repeat, so running a
/// failed command again finishes the work.
#[derive(Debug)]
pub struct GitHub {
  http: HttpClient,
  api_url: Url,
  owner: String,
  repo: String,
  token: String,
}

impl GitHub {
  pub fn new(
    root: &Path,
    config: &ResolvedGitHubConfig,
    http_config: &HttpConfig,
    token: Option<&str>,
  ) -> Result<Self> {
    let token = token
      .filter(|token| !token.is_empty())
      .context("GitHub requires --github-token or GITHUB_TOKEN")?;
    let (owner, repo) = match (&config.owner, &config.repo) {
      (Some(owner), Some(repo)) => (owner.clone(), repo.clone()),
      _ => {
        let remote = Git::new(root)
          .run(&["remote", "get-url", "origin"])
          .context("Configure github.owner and github.repo, or add a GitHub origin")?;
        repository_from_remote(remote.trim(), &config.api_url)?
      }
    };
    let api_url = Url::parse(&config.api_url)
      .with_context(|| format!("Invalid GitHub API url: {}", config.api_url))?;
    Ok(Self {
      http: HttpClient::new(http_config)?,
      api_url,
      owner,
      repo,
      token: token.to_string(),
    })
  }

  fn url(&self, path: &[&str]) -> Result<Url> {
    let mut url = self.api_url.clone();
    url
      .path_segments_mut()
      .map_err(|_| anyhow::anyhow!("Invalid GitHub API url: {}", self.api_url))?
      .pop_if_empty()
      .extend(["repos", &self.owner, &self.repo])
      .extend(path);
    Ok(url)
  }

  fn request(&self, method: Method, url: Url) -> RequestBuilder {
    self
      .http
      .request(method, url)
      .bearer_auth(&self.token)
      .header(ACCEPT, "application/vnd.github+json")
      .header("X-GitHub-Api-Version", API_VERSION)
  }

  async fn send(&self, request: RequestBuilder) -> Result<Response> {
    let response = self.http.send(request.build()?).await?;
    Ok(response)
  }

  /// Sends a request that has to succeed, and reads its JSON response.
  async fn send_json<T: DeserializeOwned>(&self, request: RequestBuilder) -> Result<T> {
    let response = ensure_success(self.send(request).await?).await?;
    response.json().await.context("Invalid GitHub response")
  }

  /// Opens a pull request, or updates the one open for the same branch.
  pub async fn upsert_pull_request(&self, pr: &PullRequest) -> Result<String> {
    let pull = match self.find_open_pull_request(&pr.head).await? {
      Some(pull) => {
        let number = pull.number.to_string();
        let url = self.url(&["pulls", &number])?;
        let body = json!({ "title": pr.title, "body": pr.body });
        self
          .send_json(self.request(Method::PATCH, url).json(&body))
          .await
          .context("Failed to update the pull request")?
      }
      None => {
        let body = json!({
          "base": pr.base,
          "head": pr.head,
          "title": pr.title,
          "body": pr.body,
          "draft": pr.draft,
        });
        let request = self
          .request(Method::POST, self.url(&["pulls"])?)
          .json(&body);
        self
          .send_json::<PullResponse>(request)
          .await
          .context("Failed to create the pull request")?
      }
    };

    if !pr.labels.is_empty() {
      let number = pull.number.to_string();
      let url = self.url(&["issues", &number, "labels"])?;
      let body = json!({ "labels": pr.labels });
      let response = self
        .send(self.request(Method::POST, url).json(&body))
        .await?;
      ensure_success(response)
        .await
        .with_context(|| format!("Failed to label pull request #{number}"))?;
    }
    Ok(pull.html_url)
  }

  async fn find_open_pull_request(&self, head: &str) -> Result<Option<PullResponse>> {
    let mut url = self.url(&["pulls"])?;
    url
      .query_pairs_mut()
      .append_pair("state", "open")
      .append_pair("head", &format!("{}:{head}", self.owner));
    let pulls: Vec<PullResponse> = self
      .send_json(self.request(Method::GET, url))
      .await
      .context("Failed to list pull requests")?;
    Ok(pulls.into_iter().next())
  }

  /// Creates a release for an already pushed tag, or reuses the one a previous
  /// attempt left, and uploads the assets it is missing.
  pub async fn ensure_release(&self, release: &Release) -> Result<String> {
    let existing = self.find_release(&release.tag).await?;
    let reused = existing.is_some();
    let found = match existing {
      Some(found) => found,
      None => {
        let body = json!({
          "tag_name": release.tag,
          "name": release.title,
          "body": release.body,
          "prerelease": release.prerelease,
        });
        let request = self
          .request(Method::POST, self.url(&["releases"])?)
          .json(&body);
        self
          .send_json(request)
          .await
          .with_context(|| format!("Failed to create GitHub release {}", release.tag))?
      }
    };

    let uploaded = match reused {
      true => self.list_assets(found.id).await?,
      false => HashMap::new(),
    };
    for path in &release.assets {
      self
        .upload_asset(&found, &uploaded, path)
        .await
        .with_context(|| format!("Failed to upload {}", path.display()))?;
    }
    Ok(found.html_url)
  }

  async fn find_release(&self, tag: &str) -> Result<Option<ReleaseResponse>> {
    let url = self.url(&["releases", "tags", tag])?;
    let response = self.send(self.request(Method::GET, url)).await?;
    if response.status() == StatusCode::NOT_FOUND {
      return Ok(None);
    }
    let response = ensure_success(response)
      .await
      .with_context(|| format!("Failed to look up GitHub release {tag}"))?;
    let release = response.json().await.context("Invalid GitHub response")?;
    Ok(Some(release))
  }

  async fn list_assets(&self, release_id: u64) -> Result<HashMap<String, AssetResponse>> {
    let release_id = release_id.to_string();
    let mut assets = HashMap::new();
    for page in 1.. {
      let mut url = self.url(&["releases", &release_id, "assets"])?;
      url
        .query_pairs_mut()
        .append_pair("per_page", &PAGE_SIZE.to_string())
        .append_pair("page", &page.to_string());
      let batch: Vec<AssetResponse> = self
        .send_json(self.request(Method::GET, url))
        .await
        .context("Failed to list release assets")?;
      let is_last = batch.len() < PAGE_SIZE;
      assets.extend(batch.into_iter().map(|asset| (asset.name.clone(), asset)));
      if is_last {
        break;
      }
    }
    Ok(assets)
  }

  async fn upload_asset(
    &self,
    release: &ReleaseResponse,
    uploaded: &HashMap<String, AssetResponse>,
    path: &Path,
  ) -> Result<()> {
    let name = path
      .file_name()
      .and_then(|name| name.to_str())
      .context("Asset has no valid file name")?;
    let data = tokio::fs::read(path).await?;

    if let Some(previous) = uploaded.get(name) {
      if previous.state == "uploaded" && previous.size == data.len() as u64 {
        return Ok(());
      }
      // A broken upload has to go first, as names are unique within a release.
      let id = previous.id.to_string();
      let url = self.url(&["releases", "assets", &id])?;
      ensure_success(self.send(self.request(Method::DELETE, url)).await?).await?;
    }

    let template = release.upload_url.split('{').next().unwrap_or_default();
    let mut url = Url::parse(template).context("Invalid GitHub upload url")?;
    self.ensure_trusted_upload_url(&url)?;
    url.query_pairs_mut().append_pair("name", name);
    let request = self
      .request(Method::POST, url)
      .header(CONTENT_TYPE, "application/octet-stream")
      .header(CONTENT_LENGTH, data.len())
      .timeout(UPLOAD_TIMEOUT)
      .body(data);
    ensure_success(self.send(request).await?).await?;
    Ok(())
  }

  /// The upload url comes from a response and receives the token, so it must
  /// point at GitHub.
  fn ensure_trusted_upload_url(&self, url: &Url) -> Result<()> {
    let is_public_upload = self.api_url.host_str() == Some(PUBLIC_API_HOST)
      && url.origin() == Url::parse(PUBLIC_UPLOAD_ORIGIN)?.origin();
    ensure!(
      (is_public_upload || url.origin() == self.api_url.origin())
        && url.username().is_empty()
        && url.password().is_none(),
      "GitHub returned an unexpected upload host"
    );
    Ok(())
  }
}

async fn ensure_success(response: Response) -> Result<Response> {
  let status = response.status();
  if status.is_success() {
    return Ok(response);
  }
  let body = response.text().await.unwrap_or_default();
  anyhow::bail!("GitHub responded with {status}: {}", body.trim())
}

fn repository_from_remote(remote: &str, api_url: &str) -> Result<(String, String)> {
  let (host, path) = if remote.contains("://") {
    let url = Url::parse(remote).context("Invalid Git origin URL")?;
    (
      url
        .host_str()
        .context("Missing Git origin host")?
        .to_owned(),
      url.path().trim_matches('/').to_owned(),
    )
  } else {
    let (host, path) = remote
      .split_once(':')
      .context("Origin is not a GitHub URL; configure github.owner and github.repo")?;
    (
      host.rsplit('@').next().unwrap_or(host).to_owned(),
      path.trim_matches('/').to_owned(),
    )
  };
  let api = Url::parse(api_url).with_context(|| format!("Invalid GitHub API url: {api_url}"))?;
  let api_host = api.host_str().context("GitHub API url has no host")?;
  let expected_host = match api_host {
    PUBLIC_API_HOST => "github.com",
    host => host,
  };
  ensure!(
    host == expected_host,
    "Origin is not on {expected_host}; configure github.owner, github.repo and github.api_url"
  );
  let path = path.strip_suffix(".git").unwrap_or(&path);
  let (owner, repo) = path
    .split_once('/')
    .context("Origin must specify owner/repository")?;
  ensure!(
    !owner.is_empty() && !repo.is_empty() && !repo.contains('/'),
    "Invalid GitHub repository path"
  );
  Ok((owner.into(), repo.into()))
}

#[cfg(test)]
mod tests {
  use super::*;

  fn github(api_url: &str) -> GitHub {
    let config = ResolvedGitHubConfig {
      api_url: api_url.to_string(),
      owner: Some("org".to_string()),
      repo: Some("repo".to_string()),
      skip_release: false,
      release_pr_branch_format: String::new(),
      release_pr_title_format: String::new(),
      release_pr_labels: vec![],
    };
    GitHub::new(
      Path::new("."),
      &config,
      &HttpConfig::default(),
      Some("token"),
    )
    .unwrap()
  }

  #[test]
  fn upload_urls_are_limited_to_github() {
    let public = github("https://api.github.com");
    let trusted = Url::parse("https://uploads.github.com/repos/org/repo/releases/1/assets");
    assert!(public.ensure_trusted_upload_url(&trusted.unwrap()).is_ok());
    for url in [
      "https://untrusted.example/assets",
      "http://uploads.github.com/assets",
      "https://uploads.github.com:8443/assets",
      "https://user:pass@uploads.github.com/assets",
    ] {
      let url = Url::parse(url).unwrap();
      assert!(public.ensure_trusted_upload_url(&url).is_err(), "{url}");
    }

    let enterprise = github("https://git.example.com/api/v3");
    let url = Url::parse("https://git.example.com/api/uploads/assets").unwrap();
    assert!(enterprise.ensure_trusted_upload_url(&url).is_ok());
  }

  #[test]
  fn repository_comes_from_https_and_ssh_remotes() {
    for remote in [
      "https://github.com/org/repo.git",
      "git@github.com:org/repo.git",
      "ssh://git@github.com/org/repo.git",
    ] {
      assert_eq!(
        repository_from_remote(remote, "https://api.github.com").unwrap(),
        ("org".to_string(), "repo".to_string())
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
      ("org".to_string(), "repo".to_string())
    );
  }

  #[test]
  fn requires_a_token() {
    let config = ResolvedGitHubConfig {
      api_url: "https://api.github.com".to_string(),
      owner: Some("org".to_string()),
      repo: Some("repo".to_string()),
      skip_release: false,
      release_pr_branch_format: String::new(),
      release_pr_title_format: String::new(),
      release_pr_labels: vec![],
    };
    let error = GitHub::new(Path::new("."), &config, &HttpConfig::default(), None).unwrap_err();
    assert!(error.to_string().contains("GITHUB_TOKEN"), "{error}");
  }
}
