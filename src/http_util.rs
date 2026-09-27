use crate::config::HttpConfig;
use anyhow::{Context, Result, ensure};
use reqwest::{
  Method, StatusCode, Url,
  blocking::{Client, RequestBuilder},
  header::{HeaderMap, RETRY_AFTER},
};
use serde::de::DeserializeOwned;
use std::{
  thread,
  time::{Duration, SystemTime},
};

/// Shared transport. Only GET requests are replayed; mutating requests are sent once.
#[derive(Clone)]
pub struct HttpClient {
  client: Client,
  policy: HttpConfig,
}

pub struct Response {
  pub status: StatusCode,
  pub body: Vec<u8>,
}
impl Response {
  pub fn json<T: DeserializeOwned>(&self) -> Result<T> {
    self.ensure_success()?;
    serde_json::from_slice(&self.body).context("invalid API JSON response")
  }
  pub fn ensure_success(&self) -> Result<()> {
    ensure!(
      self.status.is_success(),
      "API returned HTTP {}",
      self.status
    );
    Ok(())
  }
}

impl HttpClient {
  pub fn new(policy: &HttpConfig) -> Result<Self> {
    let client = Client::builder()
      .user_agent(concat!("prp/", env!("CARGO_PKG_VERSION")))
      .connect_timeout(Duration::from_secs(policy.timeout_secs))
      .timeout(Duration::from_secs(policy.timeout_secs))
      .redirect(reqwest::redirect::Policy::none())
      .retry(reqwest::retry::never())
      .build()?;
    Ok(Self {
      client,
      policy: policy.clone(),
    })
  }
  pub fn request(&self, method: Method, url: Url) -> RequestBuilder {
    self.client.request(method, url)
  }
  pub fn get(&self, url: Url) -> Result<Response> {
    self.send(self.request(Method::GET, url))
  }
  pub fn send(&self, builder: RequestBuilder) -> Result<Response> {
    let request = builder.build()?;
    let retryable = request.method() == Method::GET;
    let max_retries = if retryable { self.policy.retries } else { 0 };
    for attempt in 0..=max_retries {
      let next = request
        .try_clone()
        .context("HTTP request body cannot be replayed")?;
      match self.client.execute(next) {
        Ok(response) => {
          let status = response.status();
          let headers = response.headers().clone();
          let transient = status.is_server_error()
            || status == StatusCode::TOO_MANY_REQUESTS
            || (status == StatusCode::FORBIDDEN
              && (headers.contains_key(RETRY_AFTER)
                || headers
                  .get("x-ratelimit-remaining")
                  .is_some_and(|v| v == "0")));
          if transient && attempt < max_retries {
            self.wait(attempt, &headers)?;
            continue;
          }
          match response.bytes() {
            Ok(body) => {
              return Ok(Response {
                status,
                body: body.to_vec(),
              });
            }
            Err(error) if attempt < max_retries && (error.is_timeout() || error.is_body()) => {
              self.wait(attempt, &HeaderMap::new())?
            }
            Err(error) => return Err(error.without_url().into()),
          }
        }
        Err(error)
          if attempt < max_retries
            && (error.is_timeout() || error.is_connect() || error.is_request()) =>
        {
          self.wait(attempt, &HeaderMap::new())?
        }
        Err(error) => return Err(error.without_url().into()),
      }
    }
    unreachable!("the final HTTP attempt always returns")
  }
  /// Streaming bodies cannot be cloned and must never be retried implicitly.
  pub fn send_once(&self, builder: RequestBuilder) -> Result<Response> {
    let response = builder.send().map_err(reqwest::Error::without_url)?;
    let status = response.status();
    let body = response
      .bytes()
      .map_err(reqwest::Error::without_url)?
      .to_vec();
    Ok(Response { status, body })
  }
  fn wait(&self, attempt: u32, headers: &HeaderMap) -> Result<()> {
    let cap = Duration::from_secs(self.policy.max_retry_delay_secs);
    let backoff = Duration::from_millis(
      self
        .policy
        .retry_base_ms
        .saturating_mul(1u64 << attempt.min(10)),
    )
    .min(cap);
    let server_delay = retry_after(headers, SystemTime::now());
    // Do not retry earlier than the server requested, or block indefinitely on a rate limit.
    ensure!(
      server_delay.is_none_or(|d| d <= cap),
      "API retry delay exceeds {} seconds; try again later",
      self.policy.max_retry_delay_secs
    );
    let delay = server_delay.unwrap_or(backoff).max(backoff);
    eprintln!(
      "HTTP lookup retry {}/{} in {} ms",
      attempt + 1,
      self.policy.retries,
      delay.as_millis()
    );
    thread::sleep(delay);
    Ok(())
  }
}

fn retry_after(headers: &HeaderMap, now: SystemTime) -> Option<Duration> {
  if let Some(value) = headers.get(RETRY_AFTER).and_then(|v| v.to_str().ok()) {
    if let Ok(seconds) = value.parse::<u64>() {
      return Some(Duration::from_secs(seconds));
    }
    if let Ok(date) = httpdate::parse_http_date(value) {
      return Some(date.duration_since(now).unwrap_or_default());
    }
  }
  if headers
    .get("x-ratelimit-remaining")
    .is_some_and(|v| v == "0")
  {
    let reset = headers
      .get("x-ratelimit-reset")?
      .to_str()
      .ok()?
      .parse::<u64>()
      .ok()?;
    return Some(
      SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_secs(reset))?
        .duration_since(now)
        .unwrap_or_default(),
    );
  }
  None
}

pub fn endpoint(base: &str) -> Result<Url> {
  let url = Url::parse(base).context("invalid API endpoint URL")?;
  ensure!(
    matches!(url.scheme(), "http" | "https")
      && url.host_str().is_some()
      && url.username().is_empty()
      && url.password().is_none()
      && url.query().is_none()
      && url.fragment().is_none(),
    "API endpoint must be an HTTP(S) URL without credentials, query, or fragment"
  );
  Ok(url)
}

pub fn url(base: &str, segments: &[&str]) -> Result<Url> {
  let mut url = endpoint(base)?;
  url
    .path_segments_mut()
    .map_err(|_| anyhow::anyhow!("API URL cannot contain path segments"))?
    .pop_if_empty()
    .extend(segments);
  Ok(url)
}

#[cfg(test)]
mod tests {
  use super::*;

  fn client() -> HttpClient {
    HttpClient::new(&HttpConfig {
      retry_base_ms: 0,
      ..Default::default()
    })
    .unwrap()
  }

  #[test]
  fn get_retries_transient_errors_but_not_auth_or_missing_resources() {
    let mut server = mockito::Server::new();
    for (status, rate_limit, attempts) in [
      (503, false, 3),
      (429, false, 3),
      (403, true, 3),
      (403, false, 1),
      (401, false, 1),
      (404, false, 1),
    ] {
      let path = format!("/{status}/{rate_limit}");
      let mut mock = server.mock("GET", path.as_str()).with_status(status);
      if rate_limit {
        mock = mock
          .with_header("x-ratelimit-remaining", "0")
          .with_header("x-ratelimit-reset", "0");
      }
      let mock = mock.expect(attempts).create();
      let response = client()
        .get(Url::parse(&format!("{}{path}", server.url())).unwrap())
        .unwrap();
      assert_eq!(response.status.as_u16(), status as u16);
      mock.assert();
    }
  }

  #[test]
  fn get_recovers_after_transient_error_and_post_is_never_replayed() {
    let mut server = mockito::Server::new();
    let failed = server
      .mock("GET", "/recover")
      .with_status(503)
      .expect(1)
      .create();
    let recovered = server
      .mock("GET", "/recover")
      .with_status(200)
      .with_body("ok")
      .expect(1)
      .create();
    let http = client();
    let response = http.get(url(&server.url(), &["recover"]).unwrap()).unwrap();
    assert_eq!(response.body, b"ok");
    failed.assert();
    recovered.assert();
    let post = server
      .mock("POST", "/create")
      .with_status(503)
      .expect(1)
      .create();
    let response = http
      .send(
        http
          .request(Method::POST, url(&server.url(), &["create"]).unwrap())
          .json(&serde_json::json!({"title":"release"})),
      )
      .unwrap();
    assert_eq!(response.status, StatusCode::SERVICE_UNAVAILABLE);
    post.assert();
  }

  #[test]
  fn excessive_retry_after_stops_without_sending_another_request() {
    let mut server = mockito::Server::new();
    let mock = server
      .mock("GET", "/limited")
      .with_status(429)
      .with_header("Retry-After", "3600")
      .expect(1)
      .create();
    let error = client()
      .get(url(&server.url(), &["limited"]).unwrap())
      .err()
      .unwrap();
    assert!(error.to_string().contains("retry delay exceeds"));
    mock.assert();
  }

  #[test]
  fn retry_after_supports_seconds_dates_and_github_reset() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let mut headers = HeaderMap::new();
    headers.insert(RETRY_AFTER, "3".parse().unwrap());
    assert_eq!(retry_after(&headers, now), Some(Duration::from_secs(3)));
    headers.insert(
      RETRY_AFTER,
      httpdate::fmt_http_date(now + Duration::from_secs(5))
        .parse()
        .unwrap(),
    );
    assert_eq!(retry_after(&headers, now), Some(Duration::from_secs(5)));
    headers.remove(RETRY_AFTER);
    headers.insert("x-ratelimit-remaining", "0".parse().unwrap());
    headers.insert("x-ratelimit-reset", "1700000010".parse().unwrap());
    assert_eq!(retry_after(&headers, now), Some(Duration::from_secs(10)));
  }
}
