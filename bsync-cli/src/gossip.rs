// iroh endpoint + gossip setup, topic derivation, and run loop helpers.

use anyhow::Context;
use iroh::{endpoint::presets, protocol::Router, Endpoint, EndpointId, SecretKey};
use iroh_gossip::{
    api::{GossipSender, GossipReceiver},
    Gossip, TopicId,
};

/// Topic derived from room name: blake3("bsync-clipboard-v1:" + room) → 32 bytes.
pub fn derive_topic(room: &str) -> TopicId {
    let hash = blake3::hash(format!("bsync-clipboard-v1:{room}").as_bytes());
    TopicId::from_bytes(*hash.as_bytes())
}

/// Container for iroh resources that must stay alive.
#[allow(dead_code)]
pub struct GossipHandle {
    pub endpoint: Endpoint,
    pub router: Router,
    pub gossip: Gossip,
    pub sender: GossipSender,
    pub receiver: GossipReceiver,
}

/// Set up the iroh endpoint, gossip protocol, and subscribe to the topic.
/// Returns a handle that owns all resources.
pub async fn setup(
    room: &str,
    secret_key: &SecretKey,
    bootstrap: Vec<EndpointId>,
) -> anyhow::Result<GossipHandle> {
    let endpoint = Endpoint::builder(presets::N0)
        .secret_key(secret_key.clone())
        .bind()
        .await
        .context("failed to bind iroh endpoint")?;

    let gossip = Gossip::builder().spawn(endpoint.clone());

    let router = Router::builder(endpoint.clone())
        .accept(iroh_gossip::ALPN, gossip.clone())
        .spawn();

    let topic_id = derive_topic(room);
    let topic = gossip
        .subscribe(topic_id, bootstrap)
        .await
        .context("failed to subscribe to gossip topic")?;

    let (sender, receiver) = topic.split();

    Ok(GossipHandle {
        endpoint,
        router,
        gossip,
        sender,
        receiver,
    })
}

/// Wait until at least one peer has joined the swarm.
#[allow(dead_code)]
pub async fn wait_for_peer(receiver: &mut GossipReceiver) -> anyhow::Result<()> {
    receiver.joined().await.context("timed out waiting for peer join")
}

/// Parse an endpoint address string into an EndpointId for bootstrapping.
pub fn parse_endpoint_addr(addr: &str) -> anyhow::Result<EndpointId> {
    addr.parse().context("invalid endpoint address in ticket")
}
