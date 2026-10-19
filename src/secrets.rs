//! API key persistence in the desktop Secret Service.
//!
//! Backends, chosen automatically by [`oo7`]:
//!
//! - **Host:** the `org.freedesktop.secrets` D-Bus service (GNOME Keyring,
//!   `KWallet` ≥ 5.97, `KeePassXC`), using its default collection.
//! - **Flatpak:** the XDG Secret portal, which gives the app its own encrypted
//!   keyring file without D-Bus access to other applications' secrets.
//!
//! The key is never written anywhere else: not in a config file, not in
//! `GSettings`, not in an environment variable. Items are identified by the
//! attributes `application = <APP_ID>` and `kind = api-key`, so the
//! application never reads or overwrites unrelated items.
//!
//! All methods are `async` and must run on the worker runtime (see
//! [`crate::worker`]). Unlocking a locked collection may show a system
//! prompt.

use secrecy::SecretString;
use thiserror::Error;

use crate::api::{ApiError, ApiKey};
use crate::config;

/// Human-readable label shown in keyring managers such as Seahorse.
const ITEM_LABEL: &str = "Filester API key";
const ATTR_APPLICATION: &str = "application";
const ATTR_KIND: &str = "kind";
const KIND_API_KEY: &str = "api-key";

/// Errors from the secret store.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum SecretStoreError {
    /// The Secret Service or portal is unavailable, locked, or refused the
    /// operation. This usually means no keyring daemon is running.
    #[error("system keyring unavailable: {0}")]
    Backend(#[source] Box<oo7::Error>),

    /// A stored item exists but does not hold a valid key. Call
    /// [`TokenStore::clear`] and ask the user to sign in again.
    #[error("the stored API key is invalid; please sign in again")]
    Corrupt,
}

impl From<oo7::Error> for SecretStoreError {
    fn from(err: oo7::Error) -> Self {
        Self::Backend(Box::new(err))
    }
}

/// Handle to the API key's keyring item.
#[derive(Debug)]
pub struct TokenStore {
    keyring: oo7::Keyring,
}

impl TokenStore {
    /// Connect to the Secret Service, or to the portal inside a sandbox.
    ///
    /// # Errors
    ///
    /// [`SecretStoreError::Backend`] if no backend is reachable.
    pub async fn open() -> Result<Self, SecretStoreError> {
        Ok(Self {
            keyring: oo7::Keyring::new().await?,
        })
    }

    /// Load the stored key, if any.
    ///
    /// # Errors
    ///
    /// [`SecretStoreError::Backend`] if the keyring cannot be unlocked or
    /// read, and [`SecretStoreError::Corrupt`] if the stored value is not a
    /// valid key.
    pub async fn load(&self) -> Result<Option<ApiKey>, SecretStoreError> {
        self.keyring.unlock().await?;
        let items = self.keyring.search_items(&attributes()).await?;
        let Some(item) = items.first() else {
            return Ok(None);
        };
        // `oo7::Secret` zeroizes on drop; the copy moves straight into a
        // `SecretString`, which does the same.
        let secret = item.secret().await?;
        let text = std::str::from_utf8(secret.as_bytes()).map_err(|_| SecretStoreError::Corrupt)?;
        ApiKey::new(SecretString::from(text))
            .map(Some)
            .map_err(|_: ApiError| SecretStoreError::Corrupt)
    }

    /// Store `key`, replacing any previous key for this application.
    ///
    /// # Errors
    ///
    /// [`SecretStoreError::Backend`] if the keyring cannot be unlocked or
    /// written.
    pub async fn save(&self, key: &ApiKey) -> Result<(), SecretStoreError> {
        self.keyring.unlock().await?;
        self.keyring
            .create_item(
                ITEM_LABEL,
                &attributes(),
                oo7::Secret::text(key.expose_for_storage()),
                true,
            )
            .await?;
        Ok(())
    }

    /// Delete the stored key (sign-out). Succeeds if nothing was stored.
    ///
    /// # Errors
    ///
    /// [`SecretStoreError::Backend`] if the keyring cannot be unlocked or
    /// modified.
    pub async fn clear(&self) -> Result<(), SecretStoreError> {
        self.keyring.unlock().await?;
        self.keyring.delete(&attributes()).await?;
        Ok(())
    }
}

/// Attributes that identify this application's key, and nothing else.
fn attributes() -> [(&'static str, &'static str); 2] {
    [
        (ATTR_APPLICATION, config::APP_ID),
        (ATTR_KIND, KIND_API_KEY),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attributes_are_namespaced_by_app_id() {
        let attrs = attributes();
        assert!(attrs.contains(&("application", "io.github.scroguard.Filester")));
        assert!(attrs.contains(&("kind", "api-key")));
    }
}
