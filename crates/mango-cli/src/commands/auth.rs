//! Account management: Microsoft sign-in, listing, and removal.

use clap::Subcommand;
use mango_core::auth;
use mango_core::config::Config;
use mango_core::net::http;
use mango_core::paths::DataPaths;

#[derive(Debug, Subcommand)]
pub enum AuthCommand {
    /// Sign in with a Microsoft account (opens the browser).
    Login,
    /// Sign in to a custom Yggdrasil server (authlib-injector).
    LoginYggdrasil {
        /// Auth server base URL, e.g. https://auth.example.com/api/yggdrasil
        #[arg(long)]
        server: String,
        /// Account name or email.
        #[arg(long)]
        username: String,
        /// Password. Prompted for when omitted.
        #[arg(long)]
        password: Option<String>,
    },
    /// List stored accounts.
    List,
    /// Remove an account and its stored credentials.
    Logout {
        /// Account uuid. Defaults to the selected account.
        uuid: Option<String>,
    },
    /// Refresh the selected account's token now.
    Refresh,
    /// Show whether a client ID is configured.
    Status,
}

pub async fn run(
    command: AuthCommand,
    data: &DataPaths,
    config: &mut Config,
) -> mango_core::Result<()> {
    match command {
        AuthCommand::Login => login(data, config).await,
        AuthCommand::LoginYggdrasil { server, username, password } => {
            login_yggdrasil(data, config, &server, &username, password).await
        }
        AuthCommand::List => list(config),
        AuthCommand::Logout { uuid } => logout(data, config, uuid),
        AuthCommand::Refresh => refresh(data, config).await,
        AuthCommand::Status => status(data),
    }
}

async fn login_yggdrasil(
    data: &DataPaths,
    config: &mut Config,
    server: &str,
    username: &str,
    password: Option<String>,
) -> mango_core::Result<()> {
    let password = match password {
        Some(password) => password,
        None => {
            // Read without echoing, so the password does not land in shell history
            // or the scrollback buffer.
            eprint!("password: ");
            rpassword_like()?
        }
    };

    let client = http::build_client()?;
    match auth::sign_in_yggdrasil(&client, data, config, server, username, &password).await {
        Ok(account) => {
            println!();
            println!("signed in   {}", account.username);
            println!("uuid        {}", account.uuid);
            println!("server      {}", account.yggdrasil_base.as_deref().unwrap_or("-"));
            println!();
            println!("launch with an authlib agent attached:");
            println!("  mango launch --online …");
            Ok(())
        }
        Err(err) => {
            println!();
            println!("sign-in failed: {err}");
            Err(err)
        }
    }
}

/// Read a password from the terminal without echoing it.
fn rpassword_like() -> mango_core::Result<String> {
    #[cfg(windows)]
    {
        // `std::io::stdin` cannot disable echo; a tiny PowerShell call can.
        let output = std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                "$p = Read-Host -AsSecureString; \
                   [Runtime.InteropServices.Marshal]::PtrToStringAuto(\
                   [Runtime.InteropServices.Marshal]::SecureStringToBSTR($p))",
            ])
            .output()
            .map_err(|err| {
                mango_core::MangoError::Other(format!("could not read the password: {err}"))
            })?;
        if !output.status.success() {
            return Err(mango_core::MangoError::Other("password entry was cancelled".to_owned()));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim_end().to_owned())
    }

    #[cfg(not(windows))]
    {
        use std::io::BufRead;
        // `stty -echo` is available on every unix with a terminal.
        let _ = std::process::Command::new("stty").arg("-echo").status();
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line).map_err(|err| {
            mango_core::MangoError::Other(format!("could not read the password: {err}"))
        })?;
        let _ = std::process::Command::new("stty").arg("echo").status();
        eprintln!();
        Ok(line.trim_end().to_owned())
    }
}

async fn login(data: &DataPaths, config: &mut Config) -> mango_core::Result<()> {
    if !auth::is_configured(data) {
        println!("no Microsoft client ID configured");
        println!("set MANGO_MSA_CLIENT_ID or create {}", data.root().join("msa.json").display());
        return Ok(());
    }

    println!("opening the browser to sign in…");
    let client = http::build_client()?;

    match auth::sign_in(&client, data, config).await {
        Ok(account) => {
            println!();
            println!("signed in   {}", account.username);
            println!("uuid        {}", account.uuid);
            if let Some(expires) = account.expires_at {
                println!("token until {}", format_time(expires));
            }
            Ok(())
        }
        Err(err) => {
            // Auth failures are actionable, so print them without the usual
            // "error:" prefix noise.
            println!();
            println!("sign-in failed: {err}");
            Err(err)
        }
    }
}

fn list(config: &Config) -> mango_core::Result<()> {
    if config.accounts.is_empty() {
        println!("no accounts; sign in with `mango auth login`");
        return Ok(());
    }

    println!("{:<38} {:<18} {:<10} TOKEN", "UUID", "NAME", "TYPE");
    for (uuid, account) in &config.accounts {
        let selected = config.selected_account.as_deref() == Some(uuid.as_str());
        let marker = if selected { " *" } else { "" };
        let token = match account.expires_at {
            Some(expires) if expires > now() => format!("valid until {}", format_time(expires)),
            Some(_) => "expired".to_owned(),
            None => "—".to_owned(),
        };
        println!(
            "{:<38} {:<18} {:<10} {}{}",
            account.uuid,
            account.username,
            format!("{:?}", account.kind).to_lowercase(),
            token,
            marker
        );
    }
    println!();
    println!("* selected account");
    Ok(())
}

fn logout(data: &DataPaths, config: &mut Config, uuid: Option<String>) -> mango_core::Result<()> {
    let target = uuid.or_else(|| config.selected_account.clone());
    let Some(target) = target else {
        println!("no account selected");
        return Ok(());
    };

    let name = config.accounts.get(&target).map(|account| account.username.clone());
    auth::remove_account(data, config, &target)?;
    println!("removed {}", name.unwrap_or(target));
    Ok(())
}

async fn refresh(data: &DataPaths, config: &mut Config) -> mango_core::Result<()> {
    let Some(uuid) = config.selected_account.clone() else {
        println!("no account selected");
        return Ok(());
    };

    let client = http::build_client()?;
    match auth::ensure_fresh(&client, data, config, &uuid).await {
        Ok(_) => {
            let expires = config.accounts.get(&uuid).and_then(|account| account.expires_at);
            println!("token refreshed");
            if let Some(expires) = expires {
                println!("valid until {}", format_time(expires));
            }
            Ok(())
        }
        Err(err) => {
            println!("refresh failed: {err}");
            Err(err)
        }
    }
}

fn status(data: &DataPaths) -> mango_core::Result<()> {
    match auth::configured_client_id(data.root()) {
        Some(id) => println!("client id  {id}"),
        None => println!("client id  not configured"),
    }
    println!("config     {}", data.root().join("msa.json").display());
    Ok(())
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

/// Render a unix timestamp as local-ish ISO-8601 for readability.
fn format_time(unix_seconds: i64) -> String {
    if unix_seconds <= 0 {
        return "—".to_owned();
    }
    let days = unix_seconds / 86_400;
    let seconds_in_day = unix_seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}Z",
        seconds_in_day / 3600,
        (seconds_in_day % 3600) / 60
    )
}

/// Howard Hinnant's days-to-civil algorithm.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m as u32, d as u32)
}
