use crate::config::HttpConfig;
use anyhow::Result;
use reqwest::{Client, Method, Request, RequestBuilder, Response, Url};
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct HttpClient {
  client: Client,
}

impl HttpClient {
  pub fn new(config: &HttpConfig) -> Result<Self> {
    let mut builder = Client::builder().user_agent(
      config
        .user_agent
        .clone()
        .unwrap_or(concat!("prp/", env!("CARGO_PKG_VERSION")).to_string()),
    );

    if let Some(timeout_secs) = config.timeout_secs {
      builder = builder.timeout(Duration::from_secs(timeout_secs));
    }

    let client = builder.build()?;

    Ok(Self { client })
  }

  pub fn request(&self, method: Method, url: Url) -> RequestBuilder {
    self.client.request(method, url)
  }

  pub async fn send(&self, req: Request) -> Result<Response> {
    let response = self.client.execute(req).await?;
    Ok(response)
  }
}
