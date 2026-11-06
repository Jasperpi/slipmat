//! Signing in to Spotify and keeping one librespot session alive.
//!
//! Slipmat is not a Spotify Connect device: it only needs a session to read
//! the library and to decode songs onto the decks, so nothing here touches
//! whatever the account is playing elsewhere.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Result, anyhow};
use librespot_core::authentication::Credentials;
use librespot_core::cache::Cache;
use librespot_core::{Session, SessionConfig};
use serde::Serialize;
use tokio::sync::watch;

use crate::paths;
use crate::spotify::auth;

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Login {
    /// No saved login yet. `riff` says a riff login exists on this machine that can be reused.
    SignedOut { riff: bool },
    Connecting,
    /// Waiting for the user to finish signing in in the browser tab we opened.
    Browser { url: String },
    Online { user: String },
    Reconnecting,
    Failed { message: String },
}

pub struct Spotify {
    live: watch::Sender<Option<Session>>,
    login: watch::Sender<Login>,
    busy: Mutex<bool>,
}

fn session_dir() -> PathBuf {
    paths::cache().join("session")
}

fn riff_credentials() -> Option<PathBuf> {
    let p = dirs::cache_dir()?.join("riff").join("session").join("credentials.json");
    p.exists().then_some(p)
}

fn keep_private(dir: &Path) {
    let _ = dir; // Windows: the user's profile folder is private already
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        let _ = std::fs::set_permissions(dir.join("credentials.json"), std::fs::Permissions::from_mode(0o600));
    }
}

fn cache() -> Result<Cache> {
    let dir = session_dir();
    std::fs::create_dir_all(&dir)?;
    // Audio files are cached too, so a song played before loads again in a moment.
    let audio = paths::cache().join("audio");
    let cache = Cache::new(Some(dir.clone()), Some(dir.clone()), Some(audio), Some(3 * 1024 * 1024 * 1024))
        .map_err(|e| anyhow!("can't open the cache: {e}"))?;
    keep_private(&dir);
    Ok(cache)
}

fn session_config() -> SessionConfig {
    SessionConfig { autoplay: Some(false), ..Default::default() }
}

impl Spotify {
    pub fn new() -> Arc<Self> {
        let (live, _) = watch::channel(None);
        let (login, _) = watch::channel(Login::SignedOut { riff: riff_credentials().is_some() });
        Arc::new(Self { live, login, busy: Mutex::new(false) })
    }

    /// Sign in with the saved login, if there is one.
    pub fn resume(self: &Arc<Self>) {
        if let Ok(cache) = cache() {
            if let Some(creds) = cache.credentials() {
                self.clone().spawn(creds);
            }
        }
    }

    pub fn login(&self) -> Login {
        self.login.borrow().clone()
    }

    pub fn watch_login(&self) -> watch::Receiver<Login> {
        self.login.subscribe()
    }

    /// The live session, waiting a little if one is on its way.
    pub async fn session(&self) -> Result<Session> {
        let mut rx = self.live.subscribe();
        let wait = async {
            loop {
                if let Some(s) = rx.borrow_and_update().as_ref() {
                    return Ok(s.clone());
                }
                if rx.changed().await.is_err() {
                    return Err(anyhow!("shutting down"));
                }
            }
        };
        tokio::time::timeout(Duration::from_secs(20), wait)
            .await
            .map_err(|_| anyhow!("not signed in to Spotify"))?
    }

    /// Reuse the login riff saved on this machine.
    pub fn use_riff(self: &Arc<Self>) -> Result<()> {
        let path = riff_credentials().ok_or_else(|| anyhow!("no riff login on this machine"))?;
        let creds: Credentials = serde_json::from_slice(&std::fs::read(path)?)?;
        self.clone().spawn(creds);
        Ok(())
    }

    /// Open Spotify's sign-in page; finishes on its own once the user approves.
    pub fn browser_login(self: &Arc<Self>) {
        let this = self.clone();
        tokio::spawn(async move {
            let shown = this.clone();
            let tokens = auth::login(auth::KEYMASTER_CLIENT_ID, auth::REDIRECT_KEYMASTER, move |url| {
                shown.login.send_replace(Login::Browser { url: url.to_string() });
            })
            .await;
            match tokens {
                Ok(t) => this.spawn(Credentials::with_access_token(t.access_token)),
                Err(e) => {
                    this.login.send_replace(Login::Failed { message: e.to_string() });
                }
            }
        });
    }

    pub fn logout(&self) {
        let _ = std::fs::remove_file(session_dir().join("credentials.json"));
        if let Some(s) = self.live.send_replace(None) {
            s.shutdown();
        }
        self.login.send_replace(Login::SignedOut { riff: riff_credentials().is_some() });
    }

    fn spawn(self: Arc<Self>, creds: Credentials) {
        {
            let mut busy = self.busy.lock().unwrap();
            if *busy {
                return;
            }
            *busy = true;
        }
        self.login.send_replace(Login::Connecting);
        tokio::spawn(async move {
            self.supervise(creds).await;
            *self.busy.lock().unwrap() = false;
        });
    }

    /// Keep a session up, reconnecting with backoff, until the login is refused.
    async fn supervise(&self, mut creds: Credentials) {
        let mut backoff = 1u64;
        let mut first = true;
        loop {
            self.login.send_replace(if first { Login::Connecting } else { Login::Reconnecting });
            let cache = match cache() {
                Ok(c) => c,
                Err(e) => {
                    self.login.send_replace(Login::Failed { message: e.to_string() });
                    return;
                }
            };
            let session = Session::new(session_config(), Some(cache.clone()));
            match session.connect(creds.clone(), true).await {
                Ok(()) => {
                    first = false;
                    backoff = 1;
                    // A one-time token can't sign in twice; reconnect with what was stored.
                    if let Some(c) = cache.credentials() {
                        creds = c;
                    }
                    keep_private(&session_dir());
                    let user = session.username();
                    log::info!("signed in to Spotify as {user}");
                    self.login.send_replace(Login::Online { user });
                    self.live.send_replace(Some(session.clone()));
                    while !session.is_invalid() {
                        tokio::time::sleep(Duration::from_millis(1500)).await;
                        if self.live.borrow().is_none() {
                            return; // signed out
                        }
                    }
                    log::warn!("Spotify connection dropped");
                    self.live.send_replace(None);
                }
                Err(e) if e.kind == librespot_core::error::ErrorKind::PermissionDenied => {
                    log::error!("login refused: {e}");
                    let _ = std::fs::remove_file(session_dir().join("credentials.json"));
                    self.login.send_replace(Login::Failed { message: "Spotify refused the saved login. Sign in again.".into() });
                    return;
                }
                Err(e) => {
                    log::warn!("connect failed: {e}");
                    if first {
                        self.login.send_replace(Login::Failed { message: format!("Couldn't reach Spotify: {e}") });
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(backoff)).await;
            backoff = (backoff * 2).min(20);
        }
    }
}
