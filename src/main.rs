//! Daemon that turns a Tuta mailbox into a Maildir and Tuta submissions into mail.

mod cli;
mod config;
mod credentials;
mod folders;
mod governor;
mod login;
#[cfg_attr(not(test), expect(dead_code))]
mod maildir;
#[cfg_attr(not(test), expect(dead_code))]
mod message;
#[cfg_attr(not(test), expect(dead_code))]
mod render;
mod sdk;
mod session;
#[cfg_attr(not(test), expect(dead_code))]
mod state;
mod totp;

use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Result};
use cli::Cli;
use login::How;
use totp::Totp;

async fn list_folders() -> Result<()> {
    let config = config::load(&config::default_path()?)?;
    let state = config::state_dir()?;
    let credentials = credentials::load(&credentials::directory()?)?;
    let totp = Totp::parse(&credentials.totp)?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let sdk = sdk::native(&config.api_url, state.clone())?;
    let (logged_in, how) = login::connect(
        &sdk,
        &config.email,
        &credentials.password,
        &totp,
        &state,
        now,
    )
    .await?;
    match how {
        How::Resumed => eprintln!("resumed the saved session"),
        How::LoggedIn => eprintln!("logged in and saved the session"),
    }
    for folder in folders::load(&logged_in).await? {
        let label = if folder.name.is_empty() {
            format!("{:?}", folder.kind)
        } else {
            folder.name
        };
        println!("{}{label}", "  ".repeat(folder.depth));
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<ExitCode> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match cli::parse(&args) {
        Ok(Cli::Help) => println!("{}", cli::USAGE),
        Ok(Cli::Version) => println!("tutad {}", env!("CARGO_PKG_VERSION")),
        Ok(Cli::Run) => {
            let config = config::load(&config::default_path()?)?;
            let state = config::state_dir()?;
            let credentials = credentials::load(&credentials::directory()?)?;
            Totp::parse(&credentials.totp)?;
            bail!(
                "run is not implemented yet: {} into {}, state in {}",
                config.email,
                config.maildir.display(),
                state.display()
            );
        }
        Ok(Cli::Folders) => list_folders().await?,
        Ok(Cli::Send) => {
            config::load(&config::default_path()?)?;
            bail!("send is not implemented yet");
        }
        Err(error) => {
            eprintln!("tutad: {error}\ntry tutad -h");
            return Ok(ExitCode::from(2));
        }
    }
    Ok(ExitCode::SUCCESS)
}
