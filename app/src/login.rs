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
        self.ui().set_password(SharedString::new());
        self.clear_toast();
        self.open(Source::Home, Placement::Reset);
    }

    pub fn switch_user(&self) {
        if self.session.borrow().is_some() {
            return;
        }
        *self.client.borrow_mut() = None;
        self.open(Source::Users, Placement::Reset);
    }
}
