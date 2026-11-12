//! Secret storage with redacted values and typed failures.
//!
//! The platform implementation uses the native credential store through the
//! keyring crate's all-in-one interface. Automated tests use the in-memory
//! implementation, so no test reads or writes the real keychain.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Mutex, MutexGuard};

use keyring::v1::{Entry, Error as KeyringError};
use secrecy::{ExposeSecret, SecretString};

#[cfg(target_os = "macos")]
const KEYRING_SERVICE: &str = "com.oneget.app";
#[cfg(not(target_os = "macos"))]
const KEYRING_SERVICE: &str = "OneGet";

/// A typed secret-store failure with a stable machine-readable code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretError {
    /// Stable error code, for example `empty_secret` or `secret_unavailable`.
    pub code: &'static str,
    /// Human-readable, user-safe description that never contains a secret.
    pub message: String,
}

impl SecretError {
    /// Create an error with an explicit stable code.
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    fn unavailable(message: impl Into<String>) -> Self {
        Self::new("secret_unavailable", message)
    }
}

impl fmt::Display for SecretError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for SecretError {}

/// A secret value that is zeroized on drop and redacted when formatted.
#[derive(Clone)]
pub struct SecretValue {
    inner: SecretString,
}

impl SecretValue {
    /// Wrap a non-empty value.
    pub fn new(value: String) -> Result<Self, SecretError> {
        if value.is_empty() {
            return Err(SecretError::new(
                "empty_secret",
                "secret value must not be empty",
            ));
        }
        Ok(Self {
            inner: SecretString::from(value),
        })
    }

    /// Borrow the raw value for an explicit use such as storing it.
    pub fn expose_secret(&self) -> &str {
        self.inner.expose_secret()
    }
}

impl fmt::Debug for SecretValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretValue(***)")
    }
}

impl fmt::Display for SecretValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("***")
    }
}

/// Read, write, and delete named secrets.
pub trait SecretStore: Send + Sync {
    /// Fetch a secret, returning `None` when the key does not exist.
    fn get(&self, key: &str) -> Result<Option<SecretValue>, SecretError>;

    /// Store a secret, replacing any existing value for the key.
    fn set(&self, key: &str, value: &SecretValue) -> Result<(), SecretError>;

    /// Delete a secret. Deleting a missing key succeeds.
    fn delete(&self, key: &str) -> Result<(), SecretError>;
}

enum MemoryState {
    Available(HashMap<String, SecretValue>),
    Unavailable,
}

/// A process-local secret store used by automated tests and offline fallbacks.
pub struct MemorySecretStore {
    state: Mutex<MemoryState>,
}

impl MemorySecretStore {
    /// Create an empty, available in-memory store.
    pub fn new() -> Self {
        Self {
            state: Mutex::new(MemoryState::Available(HashMap::new())),
        }
    }

    /// Create a store whose operations all fail with `secret_unavailable`.
    pub fn unavailable() -> Self {
        Self {
            state: Mutex::new(MemoryState::Unavailable),
        }
    }

    fn lock(&self) -> Result<MutexGuard<'_, MemoryState>, SecretError> {
        self.state
            .lock()
            .map_err(|_| SecretError::new("secret_store_poisoned", "secret store lock is poisoned"))
    }
}

impl Default for MemorySecretStore {
    fn default() -> Self {
        Self::new()
    }
}

impl SecretStore for MemorySecretStore {
    fn get(&self, key: &str) -> Result<Option<SecretValue>, SecretError> {
        match &*self.lock()? {
            MemoryState::Unavailable => Err(SecretError::unavailable(
                "platform secret store is unavailable",
            )),
            MemoryState::Available(values) => Ok(values.get(key).cloned()),
        }
    }

    fn set(&self, key: &str, value: &SecretValue) -> Result<(), SecretError> {
        match &mut *self.lock()? {
            MemoryState::Unavailable => Err(SecretError::unavailable(
                "platform secret store is unavailable",
            )),
            MemoryState::Available(values) => {
                values.insert(key.to_owned(), value.clone());
                Ok(())
            }
        }
    }

    fn delete(&self, key: &str) -> Result<(), SecretError> {
        match &mut *self.lock()? {
            MemoryState::Unavailable => Err(SecretError::unavailable(
                "platform secret store is unavailable",
            )),
            MemoryState::Available(values) => {
                values.remove(key);
                Ok(())
            }
        }
    }
}

/// Secret storage backed by the platform-native credential store.
pub struct KeyringStore;

impl KeyringStore {
    /// Create a store bound to the platform credential service.
    pub fn new() -> Self {
        Self
    }

    fn entry(&self, key: &str) -> Result<Entry, SecretError> {
        Entry::new(KEYRING_SERVICE, key).map_err(map_keyring_error)
    }
}

impl Default for KeyringStore {
    fn default() -> Self {
        Self::new()
    }
}

impl SecretStore for KeyringStore {
    fn get(&self, key: &str) -> Result<Option<SecretValue>, SecretError> {
        match self.entry(key)?.get_password() {
            Ok(value) => SecretValue::new(value).map(Some),
            Err(KeyringError::NoEntry) => Ok(None),
            Err(error) => Err(map_keyring_error(error)),
        }
    }

    fn set(&self, key: &str, value: &SecretValue) -> Result<(), SecretError> {
        self.entry(key)?
            .set_password(value.expose_secret())
            .map_err(map_keyring_error)
    }

    fn delete(&self, key: &str) -> Result<(), SecretError> {
        match self.entry(key)?.delete_credential() {
            Ok(()) | Err(KeyringError::NoEntry) => Ok(()),
            Err(error) => Err(map_keyring_error(error)),
        }
    }
}

fn map_keyring_error(error: KeyringError) -> SecretError {
    SecretError::unavailable(format!("platform secret store is unavailable: {error}"))
}
