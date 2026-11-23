//! HTTP GETs: release listings and downloads. Behind [`Fetch`] so tests serve
//! fixtures.

use std::time::Duration;

use async_trait::async_trait;

use crate::Error;

/// GETs a URL's body, refusing one larger than `limit` bytes.
#[async_trait]
pub trait Fetch: Send + Sync {
    async fn get(
        &self,
        url: &str,
        headers: &[(String, String)],
        limit: u64,
    ) -> Result<Vec<u8>, Error>;
}

/// [`Fetch`] over HTTPS.
#[derive(Debug, Clone)]
pub struct Client {
    client: reqwest::Client,
}

impl Client {
    pub fn new() -> Result<Self, Error> {
        let client = reqwest::Client::builder()
            .user_agent(concat!("wuphf-update/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(20))
            // Generous: a binary is tens of megabytes.
            .timeout(Duration::from_secs(15 * 60))
            .build()
            .map_err(|error| Error::Http(format!("build the HTTP client: {error}")))?;
        Ok(Self { client })
    }
}

#[async_trait]
impl Fetch for Client {
    async fn get(
        &self,
        url: &str,
        headers: &[(String, String)],
        limit: u64,
    ) -> Result<Vec<u8>, Error> {
        let failed = |error: reqwest::Error| Error::Http(format!("GET {url}: {error}"));
        let mut request = self.client.get(url);
        for (name, value) in headers {
            request = request.header(name, value);
        }
        let mut response = request.send().await.map_err(failed)?;
        let status = response.status();
        if !status.is_success() {
            return Err(Error::Http(format!("GET {url}: {status}")));
        }
        if response
            .content_length()
            .is_some_and(|length| length > limit)
        {
            return Err(Error::Http(format!("GET {url}: larger than {limit} bytes")));
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(failed)? {
            if body.len() as u64 + chunk.len() as u64 > limit {
                return Err(Error::Http(format!("GET {url}: larger than {limit} bytes")));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }
}
