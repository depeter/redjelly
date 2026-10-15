//! Secrets: Jellyfin access tokens (one per server+user, so household
//! members can switch without retyping passwords) and provider passwords.
//! Windows: Credential Manager. Elsewhere (dev only): a 0600 JSON file.

use std::path::PathBuf;

const SERVICE: &str = "redjelly";

fn account(server: &str, user_id: &str) -> String {
    format!("{}|{user_id}", server.trim_end_matches('/'))
}

/// Account name for a subtitle provider login.
pub fn provider_account(provider: &str, username: &str) -> String {
    format!("provider:{provider}|{username}")
}

pub struct TokenStore {
    #[cfg_attr(windows, allow(dead_code))]
    file: PathBuf,
}

impl TokenStore {
    pub fn new(data_dir: PathBuf) -> Self {
        #[cfg(windows)]
        match windows_native_keyring_store::Store::new() {
            Ok(store) => keyring_core::set_default_store(store),
            Err(e) => tracing::warn!("Windows credential store unavailable: {e}"),
        }
        Self { file: data_dir.join("tokens.json") }
    }

    pub fn get(&self, server: &str, user_id: &str) -> Option<String> {
        self.get_secret(&account(server, user_id))
    }

    pub fn set(&self, server: &str, user_id: &str, token: &str) {
        self.set_secret(&account(server, user_id), token)
    }

    pub fn delete(&self, server: &str, user_id: &str) {
        self.delete_secret(&account(server, user_id))
    }

    #[cfg(windows)]
    pub fn get_secret(&self, account: &str) -> Option<String> {
        keyring_core::Entry::new(SERVICE, account).ok()?.get_password().ok()
    }

    #[cfg(windows)]
    pub fn set_secret(&self, account: &str, value: &str) {
        let r = keyring_core::Entry::new(SERVICE, account).and_then(|e| e.set_password(value));
        if let Err(e) = r {
            tracing::warn!("saving secret: {e}");
        }
    }

    #[cfg(windows)]
    pub fn delete_secret(&self, account: &str) {
        if let Ok(e) = keyring_core::Entry::new(SERVICE, account) {
            let _ = e.delete_credential();
        }
    }

    #[cfg(not(windows))]
    fn read_all(&self) -> std::collections::HashMap<String, String> {
        std::fs::read_to_string(&self.file)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    #[cfg(not(windows))]
    fn write_all(&self, map: &std::collections::HashMap<String, String>) {
        use std::os::unix::fs::OpenOptionsExt;
        let _ = SERVICE;
        let write = || -> std::io::Result<()> {
            if let Some(dir) = self.file.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&self.file)?;
            std::io::Write::write_all(&mut f, &serde_json::to_vec(map).expect("map serialises"))
        };
        if let Err(e) = write() {
            tracing::warn!("saving token: {e}");
        }
    }

    #[cfg(not(windows))]
    pub fn get_secret(&self, account: &str) -> Option<String> {
        self.read_all().remove(account)
    }

    #[cfg(not(windows))]
    pub fn set_secret(&self, account: &str, value: &str) {
        let mut map = self.read_all();
        map.insert(account.into(), value.into());
        self.write_all(&map);
    }

    #[cfg(not(windows))]
    pub fn delete_secret(&self, account: &str) {
        let mut map = self.read_all();
        map.remove(account);
        self.write_all(&map);
    }
}
