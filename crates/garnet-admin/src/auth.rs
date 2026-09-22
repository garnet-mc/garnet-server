//! Panel accounts, sessions and one-time login tokens.
//!
//! Accounts live in `panel-users.json` next to the server config with
//! argon2id password hashes. Sessions are random tokens kept in memory, so a
//! restart logs everyone out (fine for an admin panel). One-time tokens
//! cover two flows: the first-run setup link printed in the console, and
//! `/panel` typed by an opped player in game.

use anyhow::{bail, Context, Result};
use argon2::password_hash::{phc::PasswordHash, PasswordHasher, PasswordVerifier};
use argon2::Argon2;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// From least to most powerful. Every role includes the ones below it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Viewer,
    Moderator,
    Admin,
    Owner,
}

impl Role {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().as_str() {
            "viewer" => Role::Viewer,
            "moderator" | "mod" => Role::Moderator,
            "admin" => Role::Admin,
            "owner" => Role::Owner,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Role::Viewer => "viewer",
            Role::Moderator => "moderator",
            Role::Admin => "admin",
            Role::Owner => "owner",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct User {
    pub username: String,
    pub password_hash: String,
    pub role: Role,
    /// Minecraft UUID this account is tied to, if it was created by `/panel`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minecraft_uuid: Option<uuid::Uuid>,
}

#[derive(Clone, Debug)]
pub struct Session {
    pub username: String,
    pub role: Role,
    pub expires: Instant,
}

pub struct Auth {
    users_path: PathBuf,
    users: Mutex<Vec<User>>,
    sessions: Mutex<HashMap<String, Session>>,
    /// token -> (username to log in as, role, expiry). `username` may not
    /// exist yet for the setup token.
    one_time: Mutex<HashMap<String, (String, Role, Instant)>>,
    /// Failed logins per IP for throttling.
    failures: Mutex<HashMap<String, (u32, Instant)>>,
}

const SESSION_TTL: Duration = Duration::from_secs(12 * 60 * 60);
const TOKEN_TTL: Duration = Duration::from_secs(15 * 60);

impl Auth {
    pub fn load(users_path: &Path) -> Result<Self> {
        let users = if users_path.exists() {
            let text = std::fs::read_to_string(users_path)?;
            serde_json::from_str(&text).with_context(|| format!("parsing {}", users_path.display()))?
        } else {
            Vec::new()
        };
        Ok(Self {
            users_path: users_path.to_owned(),
            users: Mutex::new(users),
            sessions: Mutex::new(HashMap::new()),
            one_time: Mutex::new(HashMap::new()),
            failures: Mutex::new(HashMap::new()),
        })
    }

    pub fn has_users(&self) -> bool {
        !self.users.lock().unwrap().is_empty()
    }

    fn save(&self, users: &[User]) -> Result<()> {
        let tmp = self.users_path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(users)?)?;
        std::fs::rename(&tmp, &self.users_path)?;
        Ok(())
    }

    pub fn list_users(&self) -> Vec<(String, Role)> {
        self.users.lock().unwrap().iter().map(|u| (u.username.clone(), u.role)).collect()
    }

    pub fn create_user(&self, username: &str, password: &str, role: Role) -> Result<()> {
        let username = username.trim();
        if username.is_empty() || username.len() > 32 || !username.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-') {
            bail!("username must be 1-32 letters, digits, _ or -");
        }
        if password.len() < 8 {
            bail!("password must be at least 8 characters");
        }
        let mut users = self.users.lock().unwrap();
        if users.iter().any(|u| u.username.eq_ignore_ascii_case(username)) {
            bail!("user already exists");
        }
        let hash = Argon2::default()
            .hash_password(password.as_bytes())
            .map_err(|e| anyhow::anyhow!("hashing failed: {e}"))?
            .to_string();
        users.push(User {
            username: username.to_owned(),
            password_hash: hash,
            role,
            minecraft_uuid: None,
        });
        self.save(&users)
    }

    pub fn delete_user(&self, username: &str) -> Result<()> {
        let mut users = self.users.lock().unwrap();
        let before = users.len();
        users.retain(|u| !u.username.eq_ignore_ascii_case(username));
        if users.len() == before {
            bail!("no such user");
        }
        if !users.iter().any(|u| u.role == Role::Owner) {
            bail!("cannot delete the last owner");
        }
        self.save(&users)?;
        self.sessions.lock().unwrap().retain(|_, s| !s.username.eq_ignore_ascii_case(username));
        Ok(())
    }

    pub fn set_password(&self, username: &str, password: &str) -> Result<()> {
        if password.len() < 8 {
            bail!("password must be at least 8 characters");
        }
        let mut users = self.users.lock().unwrap();
        let user = users
            .iter_mut()
            .find(|u| u.username.eq_ignore_ascii_case(username))
            .context("no such user")?;
        user.password_hash = Argon2::default()
            .hash_password(password.as_bytes())
            .map_err(|e| anyhow::anyhow!("hashing failed: {e}"))?
            .to_string();
        self.save(&users)
    }

    /// Password login with a small per-IP delay after repeated failures.
    pub fn login(&self, username: &str, password: &str, ip: &str) -> Result<String> {
        {
            let failures = self.failures.lock().unwrap();
            if let Some((count, last)) = failures.get(ip) {
                if *count >= 5 && last.elapsed() < Duration::from_secs(60) {
                    bail!("too many failed attempts, try again in a minute");
                }
            }
        }
        let found = {
            let users = self.users.lock().unwrap();
            users.iter().find(|u| u.username.eq_ignore_ascii_case(username)).cloned()
        };
        let ok = match &found {
            Some(user) => match PasswordHash::new(&user.password_hash) {
                Ok(parsed) => Argon2::default().verify_password(password.as_bytes(), &parsed).is_ok(),
                Err(_) => false,
            },
            None => {
                // Hash anyway so timing does not reveal whether the user exists.
                let _ = Argon2::default().hash_password(password.as_bytes());
                false
            }
        };
        if !ok {
            let mut failures = self.failures.lock().unwrap();
            let entry = failures.entry(ip.to_owned()).or_insert((0, Instant::now()));
            if entry.1.elapsed() > Duration::from_secs(60) {
                entry.0 = 0;
            }
            entry.0 += 1;
            entry.1 = Instant::now();
            bail!("wrong username or password");
        }
        self.failures.lock().unwrap().remove(ip);
        let user = found.unwrap();
        Ok(self.new_session(&user.username, user.role))
    }

    fn new_session(&self, username: &str, role: Role) -> String {
        let token = random_token();
        self.sessions.lock().unwrap().insert(
            token.clone(),
            Session {
                username: username.to_owned(),
                role,
                expires: Instant::now() + SESSION_TTL,
            },
        );
        token
    }

    pub fn session(&self, token: &str) -> Option<Session> {
        let mut sessions = self.sessions.lock().unwrap();
        sessions.retain(|_, s| s.expires > Instant::now());
        sessions.get(token).cloned()
    }

    pub fn logout(&self, token: &str) {
        self.sessions.lock().unwrap().remove(token);
    }

    /// A link that logs in as `username` with `role` once, within 15 minutes.
    /// Used for the setup wizard and for `/panel` in game.
    pub fn issue_one_time(&self, username: &str, role: Role) -> String {
        let token = random_token();
        let mut tokens = self.one_time.lock().unwrap();
        tokens.retain(|_, (_, _, exp)| *exp > Instant::now());
        tokens.insert(token.clone(), (username.to_owned(), role, Instant::now() + TOKEN_TTL));
        token
    }

    /// Redeems a one-time token for a session. For the setup token the
    /// username is `*setup*` and no session is created; the caller shows the
    /// account-creation form instead.
    pub fn redeem(&self, token: &str) -> Option<(String, Role, Option<String>)> {
        let mut tokens = self.one_time.lock().unwrap();
        let (username, role, expires) = tokens.remove(token)?;
        if expires < Instant::now() {
            return None;
        }
        if username == "*setup*" {
            return Some((username, role, None));
        }
        drop(tokens);
        let session = self.new_session(&username, role);
        Some((username, role, Some(session)))
    }

    /// Whether a one-time token is valid without consuming it (setup page load).
    pub fn peek(&self, token: &str) -> bool {
        let tokens = self.one_time.lock().unwrap();
        tokens.get(token).map(|(_, _, exp)| *exp > Instant::now()).unwrap_or(false)
    }
}

fn random_token() -> String {
    let bytes: [u8; 32] = rand::random();
    hex::encode(bytes)
}
