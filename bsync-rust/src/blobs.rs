// iroh-blobs integration: upload PNG bytes to get a content hash, download by hash.

use anyhow::Context;
use iroh::EndpointId;
use iroh_blobs::store::mem::MemStore;
use iroh_blobs::{BlobsProtocol, Hash};

/// Container for blobs resources. Cloneable — all handles share the same underlying store.
#[derive(Clone)]
pub struct BlobsHandle {
    pub store: MemStore,
    pub blobs: BlobsProtocol,
}

impl Default for BlobsHandle {
    fn default() -> Self {
        let store = MemStore::new();
        let blobs = BlobsProtocol::new(&store, None);
        Self { store, blobs }
    }
}

impl BlobsHandle {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add PNG bytes to the blob store, returning the content hash.
    pub async fn add(&self, data: &[u8]) -> anyhow::Result<Hash> {
        let tag = self.store.blobs().add_slice(data).await?;
        Ok(tag.hash)
    }

    /// Download a blob from a remote peer and return its bytes.
    /// The downloader establishes the connection to `from` internally.
    pub async fn download(
        &self,
        hash: Hash,
        from: EndpointId,
        endpoint: &iroh::Endpoint,
    ) -> anyhow::Result<Vec<u8>> {
        let downloader = self.store.downloader(endpoint);
        downloader
            .download(hash, vec![from])
            .await
            .context("blob download failed")?;

        let bytes = self
            .store
            .blobs()
            .get_bytes(hash)
            .await
            .context("failed to read downloaded blob")?;

        Ok(bytes.into())
    }
}
