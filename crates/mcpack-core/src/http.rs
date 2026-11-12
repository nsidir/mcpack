use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures::stream::{self, StreamExt};
use serde::de::DeserializeOwned;
use serde::Serialize;
use sha1::{Digest, Sha1};
use tokio::io::AsyncWriteExt;
use tokio::sync::Semaphore;

use crate::error::{Error, IoContext, Result};
use crate::util;

pub const USER_AGENT: &str = concat!("mcpack/", env!("CARGO_PKG_VERSION"), " (automation-first minecraft launcher)");

/// One file to fetch.
#[derive(Debug, Clone)]
pub struct DownloadItem {
    pub url: String,
    pub dest: PathBuf,
    pub sha1: Option<String>,
    pub size: Option<u64>,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct DownloadStats {
    pub total: usize,
    pub downloaded: usize,
    pub bytes: u64,
}

#[derive(Clone)]
pub struct Http {
    client: reqwest::Client,
    concurrency: usize,
    sem: Arc<Semaphore>,
}

impl Http {
    pub fn new(concurrency: usize) -> Result<Self> {
        let client = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(Duration::from_secs(20))
            .timeout(Duration::from_secs(300))
            .build()?;
        Ok(Self { client, concurrency, sem: Arc::new(Semaphore::new(concurrency.max(1))) })
    }

    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }

    /// GET JSON with retries. `build` is called per attempt so headers/query can be set.
    pub async fn json_with<T, F>(&self, url: &str, build: F) -> Result<T>
    where
        T: DeserializeOwned,
        F: Fn(reqwest::RequestBuilder) -> reqwest::RequestBuilder,
    {
        let bytes = self.bytes_with(url, build).await?;
        serde_json::from_slice(&bytes).map_err(|e| {
            Error::Other(format!("invalid JSON from {url}: {e}"))
        })
    }

    pub async fn json<T: DeserializeOwned>(&self, url: &str) -> Result<T> {
        self.json_with(url, |r| r).await
    }

    pub async fn bytes(&self, url: &str) -> Result<Vec<u8>> {
        self.bytes_with(url, |r| r).await
    }

    pub async fn bytes_with<F>(&self, url: &str, build: F) -> Result<Vec<u8>>
    where
        F: Fn(reqwest::RequestBuilder) -> reqwest::RequestBuilder,
    {
        let mut last_err = None;
        for attempt in 0..4u32 {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_millis(400 * 2u64.pow(attempt))).await;
            }
            let _permit = self.sem.acquire().await.expect("semaphore closed");
            let resp = match build(self.client.get(url)).send().await {
                Ok(r) => r,
                Err(e) => {
                    last_err = Some(Error::Http(e));
                    continue;
                }
            };
            let status = resp.status();
            if status.is_success() {
                match resp.bytes().await {
                    Ok(b) => return Ok(b.to_vec()),
                    Err(e) => {
                        last_err = Some(Error::Http(e));
                        continue;
                    }
                }
            }
            let body = resp.text().await.unwrap_or_default();
            let err = Error::HttpStatus {
                status: status.as_u16(),
                url: url.to_string(),
                body: body.chars().take(500).collect(),
            };
            // Retry only on 429/5xx.
            if status.as_u16() == 429 || status.is_server_error() {
                last_err = Some(err);
                continue;
            }
            return Err(err);
        }
        Err(last_err.unwrap_or_else(|| Error::Other(format!("request failed: {url}"))))
    }

    /// Download one file to `dest`, verifying sha1 if given. Skips when an existing file
    /// already matches. Returns bytes downloaded (0 if skipped).
    pub async fn download(&self, item: &DownloadItem) -> Result<u64> {
        if file_is_valid(&item.dest, item.sha1.as_deref(), item.size)? {
            return Ok(0);
        }
        util::ensure_parent(&item.dest)?;
        let mut last_err = None;
        for attempt in 0..4u32 {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_millis(400 * 2u64.pow(attempt))).await;
            }
            match self.download_once(item).await {
                Ok(n) => return Ok(n),
                Err(e @ Error::HttpStatus { status, .. }) if status != 429 && status < 500 => return Err(e),
                Err(e) => last_err = Some(e),
            }
        }
        Err(last_err.unwrap_or_else(|| Error::Other(format!("download failed: {}", item.url))))
    }

    async fn download_once(&self, item: &DownloadItem) -> Result<u64> {
        let _permit = self.sem.acquire().await.expect("semaphore closed");
        let mut resp = self.client.get(&item.url).send().await?;
        if !resp.status().is_success() {
            return Err(Error::HttpStatus {
                status: resp.status().as_u16(),
                url: item.url.clone(),
                body: String::new(),
            });
        }
        let tmp = item.dest.with_file_name(format!(
            "{}.part",
            item.dest.file_name().and_then(|n| n.to_str()).unwrap_or("download")
        ));
        let mut file = tokio::fs::File::create(&tmp).await.ctx(&tmp)?;
        let mut hasher = Sha1::new();
        let mut total = 0u64;
        while let Some(chunk) = resp.chunk().await? {
            hasher.update(&chunk);
            total += chunk.len() as u64;
            file.write_all(&chunk).await.ctx(&tmp)?;
        }
        file.flush().await.ctx(&tmp)?;
        drop(file);
        let actual = hex::encode(hasher.finalize());
        if let Some(expected) = &item.sha1 {
            if !expected.eq_ignore_ascii_case(&actual) {
                let _ = std::fs::remove_file(&tmp);
                return Err(Error::HashMismatch { what: item.url.clone(), expected: expected.clone(), actual });
            }
        }
        util::replace_file(&tmp, &item.dest)?;
        Ok(total)
    }

    /// Download many files in parallel. Fails with the first error after all attempts finish.
    pub async fn download_all(&self, items: Vec<DownloadItem>) -> Result<DownloadStats> {
        self.download_all_with(items, &|_, _| {}).await
    }

    /// Like [`download_all`], reporting `(done, total)` as files complete.
    pub async fn download_all_with(&self, items: Vec<DownloadItem>, progress: &(dyn Fn(usize, usize) + Sync)) -> Result<DownloadStats> {
        let total = items.len();
        let done = std::sync::atomic::AtomicUsize::new(0);
        let done = &done;
        let results: Vec<Result<u64>> = stream::iter(items.into_iter().map(|it| {
            let this = self.clone();
            async move {
                let r = this.download(&it).await;
                let n = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                progress(n, total);
                r.map_err(|e| match e {
                    Error::HttpStatus { status, url, body } => Error::HttpStatus { status, url, body },
                    other => Error::Other(format!("{} -> {}: {other}", it.url, it.dest.display())),
                })
            }
        }))
        .buffer_unordered(self.concurrency.max(1) * 2)
        .collect()
        .await;
        let mut stats = DownloadStats { total, ..Default::default() };
        let mut first_err = None;
        for r in results {
            match r {
                Ok(0) => {}
                Ok(n) => {
                    stats.downloaded += 1;
                    stats.bytes += n;
                }
                Err(e) => {
                    if first_err.is_none() {
                        first_err = Some(e);
                    }
                }
            }
        }
        match first_err {
            Some(e) => Err(e),
            None => Ok(stats),
        }
    }
}

/// Fast validity check: size match if known, else sha1 if known, else existence.
pub fn file_is_valid(path: &Path, sha1: Option<&str>, size: Option<u64>) -> Result<bool> {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(_) => return Ok(false),
    };
    if let Some(size) = size {
        if meta.len() != size {
            return Ok(false);
        }
        // Size matches; for small files also verify the hash to catch corruption cheaply.
        if let Some(sha) = sha1 {
            if size < 4 * 1024 * 1024 {
                return Ok(util::sha1_file(path)?.eq_ignore_ascii_case(sha));
            }
        }
        return Ok(true);
    }
    if let Some(sha) = sha1 {
        return Ok(util::sha1_file(path)?.eq_ignore_ascii_case(sha));
    }
    Ok(true)
}
