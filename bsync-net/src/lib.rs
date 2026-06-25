// bsync-net — shared networking layer for bsync.
//
// Owns the entire iroh gossip + blobs stack and exposes a clean
// `NetworkEvent` stream + `broadcast()` API. Shells never touch
// `GossipMessage`, `Hash`, or `EndpointId` parsing directly.

use anyhow::Context;
use bsync_core::{ClipboardContent, GossipMessage, MAX_MESSAGE_SIZE};
use iroh::{endpoint::presets, protocol::Router, Endpoint, EndpointId, SecretKey};
use iroh_blobs::{store::mem::MemStore, BlobsProtocol, Hash};
use iroh_gossip::{api::GossipSender, Gossip, TopicId};
use n0_future::task;
use tokio::sync::mpsc;

/// Topic derived from room name: blake3("bsync-clipboard-v1:" + room) → 32 bytes.
pub fn derive_topic(room: &str) -> TopicId {
    let hash = blake3::hash(format!("bsync-clipboard-v1:{room}").as_bytes());
    TopicId::from_bytes(*hash.as_bytes())
}

/// Parse an endpoint address string into an EndpointId.
pub fn parse_endpoint_addr(addr: &str) -> anyhow::Result<EndpointId> {
    addr.parse().context("invalid endpoint address in ticket")
}

/// Event from the network layer to the shell.
///
/// For images, the blob has already been downloaded — `content` holds
/// the full PNG bytes.
#[derive(Debug)]
pub enum NetworkEvent {
    MessageReceived { from: String, content: ClipboardContent },
    PeerConnected { id: String },
    PeerDisconnected { id: String },
    Lagged,
}

/// Handle to the network layer. Not `Clone` — owns the `Router` for shutdown.
/// Pass by reference to shell event-loop functions.
pub struct Network {
    endpoint: Endpoint,
    router: Router,
    gossip: Gossip,
    sender: GossipSender,
    blobs: BlobsHandle,
    topic_id: TopicId,
}

/// Container for blobs resources. Cloneable — all handles share the same
/// underlying store (MemStore is Arc-backed internally).
#[derive(Clone)]
struct BlobsHandle {
    store: MemStore,
    blobs: BlobsProtocol,
}

impl Default for BlobsHandle {
    fn default() -> Self {
        let store = MemStore::new();
        let blobs = BlobsProtocol::new(&store, None);
        Self { store, blobs }
    }
}

impl BlobsHandle {
    /// Add bytes to the blob store, returning the content hash.
    async fn add(&self, data: &[u8]) -> anyhow::Result<Hash> {
        let tag = self.store.blobs().add_slice(data).await?;
        Ok(tag.hash)
    }

    /// Download a blob from a remote peer and return its bytes.
    async fn download(
        &self,
        hash: Hash,
        from: EndpointId,
        endpoint: &Endpoint,
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

impl Network {
    /// Set up the iroh endpoint, gossip + blobs protocols, and subscribe to the topic.
    ///
    /// Spawns a background task that owns the gossip receiver, deserializes
    /// messages, downloads image blobs, and emits `NetworkEvent`s on the
    /// returned channel.
    pub async fn setup(
        room: &str,
        secret_key: &SecretKey,
        bootstrap: Vec<EndpointId>,
    ) -> anyhow::Result<(Self, mpsc::Receiver<NetworkEvent>)> {
        let endpoint = Endpoint::builder(presets::N0)
            .secret_key(secret_key.clone())
            .bind()
            .await
            .context("failed to bind iroh endpoint")?;

        let gossip = Gossip::builder().spawn(endpoint.clone());
        let blobs = BlobsHandle::default();

        let router = Router::builder(endpoint.clone())
            .accept(iroh_gossip::ALPN, gossip.clone())
            .accept(iroh_blobs::ALPN, blobs.blobs.clone())
            .spawn();

        let topic_id = derive_topic(room);
        let topic = gossip
            .subscribe(topic_id, bootstrap)
            .await
            .context("failed to subscribe to gossip topic")?;

        let (sender, receiver) = topic.split();

        let (event_tx, event_rx) = mpsc::channel::<NetworkEvent>(64);

        // Spawn the gossip event loop — owns the receiver, deserializes
        // messages, downloads image blobs, emits NetworkEvents.
        let event_endpoint = endpoint.clone();
        let event_blobs = blobs.clone();
        task::spawn(async move {
            use iroh_gossip::api::Event as GossipEvent;
            use n0_future::StreamExt;

            let mut receiver = receiver;
            while let Some(result) = receiver.next().await {
                match result {
                    Ok(GossipEvent::Received(msg)) => {
                        if let Ok(gm) = serde_json::from_slice::<GossipMessage>(&msg.content) {
                            let (origin, content) = match gm {
                                GossipMessage::ClipboardText { origin, content } => {
                                    (origin, ClipboardContent::Text(content))
                                }
                                GossipMessage::ClipboardImage { origin, hash } => {
                                    let hash = Hash::from_bytes(hash);
                                    let from: EndpointId = match origin.parse() {
                                        Ok(id) => id,
                                        Err(_) => continue,
                                    };
                                    match event_blobs.download(hash, from, &event_endpoint).await {
                                        Ok(png_data) => {
                                            (origin, ClipboardContent::Image(png_data))
                                        }
                                        Err(_) => continue,
                                    }
                                }
                            };
                            let _ = event_tx
                                .send(NetworkEvent::MessageReceived { from: origin, content })
                                .await;
                        }
                    }
                    Ok(GossipEvent::NeighborUp(id)) => {
                        let _ = event_tx
                            .send(NetworkEvent::PeerConnected { id: id.to_string() })
                            .await;
                    }
                    Ok(GossipEvent::NeighborDown(id)) => {
                        let _ = event_tx
                            .send(NetworkEvent::PeerDisconnected { id: id.to_string() })
                            .await;
                    }
                    Ok(GossipEvent::Lagged) => {
                        let _ = event_tx.send(NetworkEvent::Lagged).await;
                    }
                    Err(_) => break,
                }
            }
        });

        Ok((
            Network {
                endpoint,
                router,
                gossip,
                sender,
                blobs,
                topic_id,
            },
            event_rx,
        ))
    }

    /// Broadcast clipboard content to all peers.
    ///
    /// For text: serialize `GossipMessage::ClipboardText`, check size, broadcast.
    /// For images: upload blob to store, broadcast only the 32-byte hash.
    pub async fn broadcast(&self, content: &ClipboardContent, origin: &str) -> anyhow::Result<()> {
        let message = match content {
            ClipboardContent::Text(text) => GossipMessage::ClipboardText {
                origin: origin.to_string(),
                content: text.clone(),
            },
            ClipboardContent::Image(png_data) => {
                let hash = self.blobs.add(png_data).await?;
                GossipMessage::ClipboardImage {
                    origin: origin.to_string(),
                    hash: *hash.as_bytes(),
                }
            }
        };
        let payload = serde_json::to_vec(&message)?;
        if payload.len() <= MAX_MESSAGE_SIZE {
            self.sender.broadcast(payload.into()).await?;
        }
        Ok(())
    }

    /// Add a peer as bootstrap to the gossip mesh (for connecting to a new peer).
    pub async fn add_peer(&self, peer_id: EndpointId) -> anyhow::Result<()> {
        self.gossip.subscribe(self.topic_id, vec![peer_id]).await?;
        Ok(())
    }

    /// This node's endpoint ID as a string.
    pub fn endpoint_id(&self) -> String {
        self.endpoint.id().to_string()
    }

    /// Graceful shutdown.
    pub async fn shutdown(&self) -> anyhow::Result<()> {
        self.router.shutdown().await.context("shutdown router")?;
        Ok(())
    }
}
