use std::{collections::BTreeMap, path::Path, sync::Arc};

use async_trait::async_trait;
use base64::Engine as _;
use futures_util::StreamExt;
use indicatif::ProgressBar;
use pako_core::Sha256Digest;
use reqwest::{
    header::{ACCEPT, AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE, LOCATION, RANGE},
    Client, StatusCode,
};
use sha2::{Digest as _, Sha256};
use tokio::{
    fs::{File, OpenOptions},
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{OwnedSemaphorePermit, Semaphore},
};
use url::Url;

use crate::{reference::Reference, Descriptor, OciReference};

const MANIFEST_ACCEPT: &str = concat!(
    "application/vnd.oci.image.index.v1+json, ",
    "application/vnd.oci.image.manifest.v1+json"
);

#[async_trait]
pub trait Registry: Send + Sync {
    async fn resolve_manifest(&self, reference: &OciReference) -> anyhow::Result<Descriptor>;

    async fn fetch_manifest(
        &self,
        reference: &OciReference,
    ) -> anyhow::Result<(Sha256Digest, Vec<u8>)>;

    async fn fetch_blob(
        &self,
        reference: &OciReference,
        digest: Sha256Digest,
        destination: &Path,
    ) -> anyhow::Result<()>;

    async fn fetch_blob_with_progress(
        &self,
        reference: &OciReference,
        digest: Sha256Digest,
        destination: &Path,
        progress: &ProgressBar,
    ) -> anyhow::Result<()>;

    async fn fetch_blob_impl(
        &self,
        reference: &OciReference,
        digest: Sha256Digest,
        destination: &Path,
        shared_progress: Option<&ProgressBar>,
    ) -> anyhow::Result<()>;

    async fn push_blob(
        &self,
        reference: &OciReference,
        source: &Path,
    ) -> anyhow::Result<Sha256Digest>;

    async fn push_blob_with_progress(
        &self,
        reference: &OciReference,
        source: &Path,
        progress: &ProgressBar,
    ) -> anyhow::Result<Sha256Digest>;

    async fn push_blob_impl(
        &self,
        reference: &OciReference,
        source: &Path,
        shared_progress: Option<&ProgressBar>,
    ) -> anyhow::Result<Sha256Digest>;

    async fn push_manifest(
        &self,
        reference: &OciReference,
        media_type: &str,
        bytes: &[u8],
    ) -> anyhow::Result<Sha256Digest>;
}

#[derive(Clone)]
pub struct OciClient {
    client: Client,
    credentials: Option<Arc<Credentials>>,
    download_limit: Arc<Semaphore>,
    scheme: String,
}

#[derive(Debug)]
struct Credentials {
    username: String,
    password: String,
}

impl std::fmt::Debug for OciClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OciClient")
            .field("authenticated", &self.credentials.is_some())
            .field("scheme", &self.scheme)
            .finish_non_exhaustive()
    }
}

impl OciClient {
    pub fn new() -> anyhow::Result<Self> {
        let client = Client::builder()
            .user_agent(concat!("pako/", env!("CARGO_PKG_VERSION")))
            .build()?;

        Ok(Self {
            client,
            credentials: None,
            download_limit: Arc::new(Semaphore::new(6)),
            scheme: "https".into(),
        })
    }

    /// Limit the number of blob downloads sharing this client.
    #[must_use]
    pub fn with_download_limit(mut self, limit: usize) -> Self {
        self.download_limit = Arc::new(Semaphore::new(limit.max(1)));
        self
    }

    #[must_use]
    pub fn with_basic_auth(mut self, username: String, password: String) -> Self {
        self.credentials = Some(Arc::new(Credentials { username, password }));
        self
    }

    /// Enable plain HTTP for local development registries only.
    #[must_use]
    pub fn insecure_http(mut self) -> Self {
        self.scheme = "http".into();
        self
    }

    fn endpoint(&self, reference: &OciReference, path: &str) -> anyhow::Result<Url> {
        Ok(Url::parse(&format!(
            "{}://{}/v2/{}/{}",
            self.scheme, reference.registry, reference.repository, path
        ))?)
    }

    fn authenticate(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let Some(credentials) = &self.credentials else {
            return request;
        };

        let raw = format!("{}:{}", credentials.username, credentials.password);
        let encoded = base64::engine::general_purpose::STANDARD.encode(raw);
        request.header(AUTHORIZATION, format!("Basic {encoded}"))
    }

    async fn send_checked(
        &self,
        request: reqwest::RequestBuilder,
    ) -> anyhow::Result<reqwest::Response> {
        let response = request.send().await?;
        log::trace!("registry response status {}", response.status());
        if response.status().is_success() {
            return Ok(response);
        }

        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("registry returned {status}: {body}")
    }
}

#[async_trait]
impl Registry for OciClient {
    async fn resolve_manifest(&self, reference: &OciReference) -> anyhow::Result<Descriptor> {
        let url = self.endpoint(
            reference,
            &format!("manifests/{}", reference.reference_string()),
        )?;
        let response = self
            .send_checked(
                self.authenticate(self.client.head(url))
                    .header(ACCEPT, MANIFEST_ACCEPT),
            )
            .await?;

        let digest = response
            .headers()
            .get("docker-content-digest")
            .ok_or_else(|| anyhow::anyhow!("registry omitted Docker-Content-Digest"))?
            .to_str()?
            .parse()?;
        let size = response
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse().ok())
            .unwrap_or(0);
        let media_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("application/octet-stream")
            .to_owned();

        Ok(Descriptor {
            media_type,
            digest,
            size,
            annotations: BTreeMap::default(),
            platform: None,
        })
    }

    async fn fetch_manifest(
        &self,
        reference: &OciReference,
    ) -> anyhow::Result<(Sha256Digest, Vec<u8>)> {
        let url = self.endpoint(
            reference,
            &format!("manifests/{}", reference.reference_string()),
        )?;
        let response = self
            .send_checked(
                self.authenticate(self.client.get(url))
                    .header(ACCEPT, MANIFEST_ACCEPT),
            )
            .await?;
        let bytes = response.bytes().await?.to_vec();
        let digest = Sha256Digest::calculate(&bytes);

        if let Reference::Digest(expected) = &reference.reference {
            if digest != *expected {
                anyhow::bail!("manifest digest mismatch: expected {expected}, got {digest}");
            }
        }

        Ok((digest, bytes))
    }

    async fn fetch_blob(
        &self,
        reference: &OciReference,
        digest: Sha256Digest,
        destination: &Path,
    ) -> anyhow::Result<()> {
        self.fetch_blob_impl(reference, digest, destination, None)
            .await
    }

    /// Fetch a blob while contributing bytes to a caller-owned progress bar.
    async fn fetch_blob_with_progress(
        &self,
        reference: &OciReference,
        digest: Sha256Digest,
        destination: &Path,
        progress: &ProgressBar,
    ) -> anyhow::Result<()> {
        self.fetch_blob_impl(reference, digest, destination, Some(progress))
            .await
    }

    async fn fetch_blob_impl(
        &self,
        reference: &OciReference,
        digest: Sha256Digest,
        destination: &Path,
        shared_progress: Option<&ProgressBar>,
    ) -> anyhow::Result<()> {
        let _permit: OwnedSemaphorePermit = self.download_limit.clone().acquire_owned().await?;
        if let Some(parent) = destination.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        let partial = destination.with_extension("partial");
        let offset = tokio::fs::metadata(&partial)
            .await
            .map_or(0, |metadata| metadata.len());
        let url = self.endpoint(reference, &format!("blobs/{digest}"))?;
        let mut request = self.authenticate(self.client.get(url));

        if offset > 0 {
            log::debug!("resuming blob {digest} at byte {offset}");
            request = request.header(RANGE, format!("bytes={offset}-"));
        }

        let response = self.send_checked(request).await?;
        let append = offset > 0 && response.status() == StatusCode::PARTIAL_CONTENT;
        if !append && offset > 0 {
            let _ = tokio::fs::remove_file(&partial).await;
        }

        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .append(append)
            .truncate(!append)
            .open(&partial)
            .await?;
        let progress = shared_progress;
        if append {
            if let Some(progress) = progress {
                progress.inc(offset);
            }
        }
        let mut stream = response.bytes_stream();

        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            file.write_all(&chunk).await?;
            if let Some(progress) = progress {
                progress.inc(chunk.len() as u64);
            }
        }
        file.sync_all().await?;
        drop(file);

        let actual = hash_file(&partial).await?;
        if actual != digest {
            let _ = tokio::fs::remove_file(&partial).await;
            anyhow::bail!("blob digest mismatch: expected {digest}, got {actual}");
        }

        tokio::fs::rename(partial, destination).await?;
        log::debug!("verified downloaded blob {digest}");
        Ok(())
    }

    async fn push_blob(
        &self,
        reference: &OciReference,
        source: &Path,
    ) -> anyhow::Result<Sha256Digest> {
        self.push_blob_impl(reference, source, None).await
    }

    /// Push a blob while contributing bytes to a caller-owned progress bar.
    async fn push_blob_with_progress(
        &self,
        reference: &OciReference,
        source: &Path,
        progress: &ProgressBar,
    ) -> anyhow::Result<Sha256Digest> {
        self.push_blob_impl(reference, source, Some(progress)).await
    }

    async fn push_blob_impl(
        &self,
        reference: &OciReference,
        source: &Path,
        shared_progress: Option<&ProgressBar>,
    ) -> anyhow::Result<Sha256Digest> {
        let mut file = File::open(source).await?;
        let mut data = Vec::new();
        file.read_to_end(&mut data).await?;
        let digest = Sha256Digest::calculate(&data);

        let exists_url = self.endpoint(reference, &format!("blobs/{digest}"))?;
        let exists = self
            .authenticate(self.client.head(exists_url))
            .send()
            .await?;
        if exists.status().is_success() {
            if let Some(progress) = shared_progress {
                progress.inc(data.len() as u64);
            }
            return Ok(digest);
        }

        let start_url = self.endpoint(reference, "blobs/uploads/")?;
        let response = self
            .send_checked(
                self.authenticate(self.client.post(start_url))
                    .header(CONTENT_LENGTH, "0"),
            )
            .await?;
        let location = response
            .headers()
            .get(LOCATION)
            .ok_or_else(|| anyhow::anyhow!("registry omitted upload location"))?
            .to_str()?;
        let mut upload_url = match Url::parse(location) {
            Ok(url) => url,
            Err(_) => self.endpoint(reference, location.trim_start_matches('/'))?,
        };
        upload_url
            .query_pairs_mut()
            .append_pair("digest", &digest.to_string());

        let stream_progress = shared_progress.cloned();
        let stream = futures_util::stream::unfold((data, 0_usize), move |(data, offset)| {
            let progress = stream_progress.clone();
            async move {
                if offset >= data.len() {
                    return None;
                }
                let end = (offset + 1024 * 1024).min(data.len());
                let chunk = bytes::Bytes::copy_from_slice(&data[offset..end]);
                if let Some(progress) = &progress {
                    progress.inc(chunk.len() as u64);
                }
                Some((Ok::<_, std::io::Error>(chunk), (data, end)))
            }
        });
        self.send_checked(
            self.authenticate(self.client.put(upload_url))
                .header(CONTENT_TYPE, "application/octet-stream")
                .body(reqwest::Body::wrap_stream(stream)),
        )
        .await?;

        Ok(digest)
    }

    async fn push_manifest(
        &self,
        reference: &OciReference,
        media_type: &str,
        bytes: &[u8],
    ) -> anyhow::Result<Sha256Digest> {
        let digest = Sha256Digest::calculate(bytes);
        let url = self.endpoint(
            reference,
            &format!("manifests/{}", reference.reference_string()),
        )?;

        self.send_checked(
            self.authenticate(self.client.put(url))
                .header(CONTENT_TYPE, media_type)
                .body(bytes.to_vec()),
        )
        .await?;

        Ok(digest)
    }
}

async fn hash_file(path: &Path) -> anyhow::Result<Sha256Digest> {
    let mut file = File::open(path).await?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0_u8; 128 * 1024];

    loop {
        let count = file.read(&mut buffer).await?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }

    Ok(Sha256Digest::from_bytes(hash.finalize().into()))
}
