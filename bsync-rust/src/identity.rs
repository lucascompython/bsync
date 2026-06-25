// Identity persistence: load or create an ed25519 SecretKey.
// Key stored at ~/.config/bsync/identity.key with 0o600 permissions.

use anyhow::Context;
use iroh_base::SecretKey;
use std::path::PathBuf;

/// Load existing key from disk, or generate + persist a new one.
/// Returns the SecretKey and its human-readable EndpointId.
pub async fn load_or_create_key() -> anyhow::Result<(SecretKey, String)> {
    let config_dir = config_dir()?;
    let key_path = config_dir.join("identity.key");

    if key_path.exists() {
        load_key(&key_path).await
    } else {
        create_key(&config_dir, &key_path).await
    }
}

async fn load_key(key_path: &PathBuf) -> anyhow::Result<(SecretKey, String)> {
    let key_bytes = tokio::fs::read(key_path)
        .await
        .context("failed to read identity key")?;
    let bytes: [u8; 32] = key_bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("identity key file is corrupt: expected 32 bytes"))?;
    let secret_key = SecretKey::from_bytes(&bytes);
    let peer_id = iroh_base::EndpointId::from(secret_key.public()).to_string();
    Ok((secret_key, peer_id))
}

async fn create_key(
    config_dir: &PathBuf,
    key_path: &PathBuf,
) -> anyhow::Result<(SecretKey, String)> {
    let secret_key = SecretKey::generate();

    tokio::fs::create_dir_all(config_dir)
        .await
        .context("failed to create config directory")?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(config_dir, std::fs::Permissions::from_mode(0o700))
            .await
            .context("failed to set config directory permissions")?;
    }

    let key_bytes = secret_key.to_bytes();
    tokio::fs::write(key_path, &key_bytes)
        .await
        .context("failed to write identity key")?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(key_path, std::fs::Permissions::from_mode(0o600))
            .await
            .context("failed to set key file permissions")?;
    }

    let peer_id = iroh_base::EndpointId::from(secret_key.public()).to_string();
    Ok((secret_key, peer_id))
}

fn config_dir() -> anyhow::Result<PathBuf> {
    dirs::config_dir()
        .map(|p| p.join("bsync"))
        .context("cannot determine config directory")
}
