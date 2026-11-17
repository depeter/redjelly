//! Signing in: saved tokens (auto-login, quick user switching), password
//! form, and Quick Connect.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use rj_jellyfin::{Client, ClientInfo, Error as JfError};
use slint::SharedString;

use crate::nav::Placement;
use crate::screens::Source;
use crate::{App, SCREEN_LOGIN_FORM, SCREEN_QUICK_CONNECT};

/// Bumped to cancel a running Quick Connect poll.
pub type QcGeneration = Arc<AtomicU64>;

pub(crate) fn normalise_url(url: &str) -> String {
    let url = url.trim().trim_end_matches('/');
    if url.contains("://") { url.to_owned() } else { format!("https://{url}") }
}

impl App {
    pub fn client_info(&self) -> ClientInfo {
        ClientInfo {
            client: "redjelly".into(),
            device: std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).unwrap_or_else(|_| "PC".into()),
            device_id: self.config.borrow().server.device_id.clone().unwrap_or_default(),
            version: env!("CARGO_PKG_VERSION").into(),
        }
    }

    /// First screen: auto-login with a saved token, else the user picker,
    /// else the server/login form.
    pub fn startup(&self) {
        let (url, user) = {
            let cfg = self.config.borrow();
            (cfg.server.url.clone(), cfg.server.last_user_id.clone())
        };
        let Some(url) = url else { return self.show_login_form(0) };
        self.ui().set_server_url(url.as_str().into());
        match user.and_then(|u| self.tokens.get(&url, &u).map(|t| (u, t))) {
            Some((user_id, token)) => self.resume_session(url, user_id, token),
            None => self.open(Source::Users, Placement::Reset),
        }
    }

    fn resume_session(&self, url: String, user_id: String, token: String) {
        let client = match Client::new(&url, self.client_info()) {
            Ok(c) => c.with_session(user_id.clone(), token),
            Err(e) => return self.toast(format!("Bad server URL: {e}")),
        };
        self.toast("Signing in…");
        self.spawn_ui(
            async move {
                let r = client.me().await;
                (client, r)
            },
            move |app, (client, result)| match result {
                Ok(_) => app.finish_login(client, url),
                Err(JfError::BadCredentials) => {
                    app.tokens.delete(&url, &user_id);
                    app.toast("Session expired, please sign in again");
                    app.open(Source::Users, Placement::Reset);
                }
                Err(e) => {
                    app.toast(format!("Can't reach the server: {e}"));
                    app.open(Source::Users, Placement::Reset);
                }
            },
        );
    }

    pub fn select_user(&self, id: String, name: String, has_password: bool) {
        let Some(url) = self.config.borrow().server.url.clone() else { return };
        if let Some(token) = self.tokens.get(&url, &id) {
            return self.resume_session(url, id, token);
        }
        if has_password {
            let ui = self.ui();
            ui.set_username(name.into());
            ui.set_password(SharedString::new());
            return self.show_login_form(2);
        }
        self.authenticate(url, name, String::new());
    }

    pub fn show_login_form(&self, first_field: i32) {
        let ui = self.ui();
        if let Some(url) = &self.config.borrow().server.url
            && ui.get_server_url().is_empty() {
                ui.set_server_url(url.as_str().into());
            }
        ui.set_status(SharedString::new());
        ui.set_login_first_field(first_field);
        // Re-entering the screen re-runs the form's `init`, which focuses
        // the requested field.
        ui.set_screen(0);
        ui.set_screen(SCREEN_LOGIN_FORM);
    }

    pub fn submit_login_form(&self) {
        let ui = self.ui();
        let url = normalise_url(&ui.get_server_url());
        ui.set_server_url(url.as_str().into());
        ui.set_status("Signing in…".into());
        self.authenticate(url, ui.get_username().to_string(), ui.get_password().to_string());
    }

    fn authenticate(&self, url: String, username: String, password: String) {
        let info = self.client_info();
        self.spawn_ui(
            async move {
                let mut c = Client::new(&url, info)?;
                c.authenticate(&username, &password).await?;
                anyhow::Ok((c, url))
            },
            |app, result| match result {
                Ok((client, url)) => app.finish_login(client, url),
                Err(e) => {
                    let msg = match e.downcast_ref::<JfError>() {
                        Some(JfError::BadCredentials) => "Wrong username or password".to_owned(),
                        _ => format!("{e:#}"),
                    };
                    if app.ui().get_screen() != SCREEN_LOGIN_FORM {
                        app.show_login_form(2);
                    }
                    app.ui().set_status(msg.into());
                }
            },
        );
    }

    pub fn start_quick_connect(&self) {
        let Some(url) = self.config.borrow().server.url.clone() else { return };
        let Ok(client) = Client::new(&url, self.client_info()) else { return };
        let generation = self.qc_gen.fetch_add(1, Ordering::SeqCst) + 1;
        let gen_now = self.qc_gen.clone();
        let ui = self.ui();
        ui.set_quick_connect_code(SharedString::new());
        ui.set_screen(SCREEN_QUICK_CONNECT);
        ui.invoke_focus_keys();

        self.spawn_ui(
            async move {
                let mut client = client;
                let qc = client.quick_connect_initiate().await?;
                let code = qc.code.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    crate::with_app(|app| app.ui().set_quick_connect_code(code.into()))
                });
                // Poll for up to 10 minutes, until approved or cancelled.
                for _ in 0..200 {
                    tokio::time::sleep(Duration::from_secs(3)).await;
                    if gen_now.load(Ordering::SeqCst) != generation {
                        return Ok(None);
                    }
                    if client.quick_connect_state(&qc.secret).await?.authenticated {
                        client.authenticate_quick_connect(&qc.secret).await?;
                        return Ok(Some((client, url)));
                    }
                }
                anyhow::bail!("Quick Connect timed out")
            },
            move |app, result: anyhow::Result<Option<(Client, String)>>| {
                if app.qc_gen.load(Ordering::SeqCst) != generation {
                    return;
                }
                match result {
                    Ok(Some((client, url))) => app.finish_login(client, url),
                    Ok(None) => {}
                    Err(e) => {
                        app.toast(format!("Quick Connect failed: {e:#}"));
                        app.login_escape();
                    }
                }
            },
        );
    }

    /// Esc on the login form / Quick Connect screen.
    pub fn login_escape(&self) {
        self.qc_gen.fetch_add(1, Ordering::SeqCst);
        if self.nav.borrow().is_empty() {
            if self.config.borrow().server.url.is_some() {
                self.open(Source::Users, Placement::Reset);
            }
        } else {
            self.show_top();
        }
    }

    fn finish_login(&self, client: Client, url: String) {
        let (Some(user_id), Some(token)) = (client.user_id().map(str::to_owned), client.token().map(str::to_owned)) else {
            return;
        };
        self.tokens.set(&url, &user_id, &token);
        {
            let mut cfg = self.config.borrow_mut();
            cfg.server.url = Some(url);
            cfg.server.last_user_id = Some(user_id);
            if let Err(e) = cfg.save(&self.config_path) {
                tracing::warn!("saving config: {e}");
            }
        }
        *self.client.borrow_mut() = Some(client);
        self.try_local_route(false);
        self.ui().set_password(SharedString::new());
        self.clear_toast();
        self.open(Source::Home, Placement::Reset);
    }

    /// The local address the signed-in client is using, if it is.
    pub fn local_route(&self) -> Option<String> {
        let public = self.config.borrow().server.url.clone()?;
        let client = self.client.borrow();
        let c = client.as_ref()?;
        let on_public = c.with_base(&public).is_ok_and(|p| p.base() == c.base());
        (!on_public).then(|| c.base().as_str().trim_end_matches('/').to_owned())
    }

    /// Switches the signed-in client to the server's address on the home
    /// network when there is one: the configured local address, or else
    /// whatever answers Jellyfin's network discovery with this server's id.
    /// An address only counts if it accepts our token for the same user;
    /// otherwise the client stays on the public address.
    /// `announce`: tell the user the outcome (they just changed the setting).
    pub fn try_local_route(&self, announce: bool) {
        let (public, local, known_id) = {
            let cfg = self.config.borrow();
            (cfg.server.url.clone(), cfg.server.local_url.clone(), cfg.server.id.clone())
        };
        let Some(public) = public else { return };
        // Start from the public address: the setting may have been cleared
        // or changed while a local one was in use.
        let Some(client) = self.client.borrow().as_ref().and_then(|c| c.with_base(&public).ok()) else { return };
        *self.client.borrow_mut() = Some(client.clone());
        let probe = client.clone();
        self.spawn_ui(
            async move {
                let mut server_id = None;
                let candidates = match local {
                    Some(local) => vec![local],
                    None => {
                        let found = tokio::task::spawn_blocking(|| {
                            rj_jellyfin::discovery::discover(std::time::Duration::from_millis(1500))
                        });
                        let (found, id) = tokio::join!(found, probe.server_id());
                        server_id = id.ok();
                        let want = server_id.clone().or(known_id);
                        let found = match found {
                            Ok(Ok(f)) => f,
                            Ok(Err(e)) => {
                                tracing::info!("server discovery: {e}");
                                Vec::new()
                            }
                            Err(_) => Vec::new(),
                        };
                        found
                            .into_iter()
                            .filter(|f| want.as_deref().is_some_and(|w| w.eq_ignore_ascii_case(&f.id)))
                            .flat_map(|f| f.urls)
                            .collect()
                    }
                };
                let mut winner = None;
                for url in candidates {
                    let Ok(candidate) = probe.with_base(&url) else { continue };
                    let me = tokio::time::timeout(std::time::Duration::from_secs(3), candidate.me()).await;
                    if matches!(&me, Ok(Ok(u)) if Some(u.id.as_str()) == candidate.user_id()) {
                        winner = Some((candidate, url));
                        break;
                    }
                    tracing::info!("server: local address {url} didn't answer");
                }
                (winner, server_id)
            },
            move |app, (winner, server_id)| {
                // Signed out or switched user while probing: not ours any more.
                if app.client.borrow().as_ref().map(|c| c.token().map(str::to_owned)) != Some(client.token().map(str::to_owned)) {
                    return;
                }
                if server_id.is_some() && app.config.borrow().server.id != server_id {
                    app.config.borrow_mut().server.id = server_id;
                    app.save_config();
                }
                match winner {
                    Some((candidate, url)) => {
                        tracing::info!("server: using local address {url}");
                        *app.client.borrow_mut() = Some(candidate);
                        if announce {
                            app.toast(format!("Using the local address {url}"));
                        }
                    }
                    None => {
                        tracing::info!("server: no local address; using {public}");
                        if announce {
                            app.toast("No local address answered; using the public one");
                        }
                    }
                }
                app.refresh_settings();
            },
        );
    }

    pub fn switch_user(&self) {
        if self.session.borrow().is_some() {
            return;
        }
        *self.client.borrow_mut() = None;
        self.open(Source::Users, Placement::Reset);
    }
}
