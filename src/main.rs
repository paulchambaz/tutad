//! Daemon that turns a Tuta mailbox into a Maildir and Tuta submissions into mail.

mod cli;
mod config;

use std::process::ExitCode;

use anyhow::{bail, Result};
use cli::Cli;

fn main() -> Result<ExitCode> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match cli::parse(&args) {
        Ok(Cli::Help) => println!("{}", cli::USAGE),
        Ok(Cli::Version) => println!("tutad {}", env!("CARGO_PKG_VERSION")),
        Ok(Cli::Run) => {
            let config = config::load(&config::default_path()?)?;
            bail!(
                "run is not implemented yet: {} into {}",
                config.email,
                config.maildir.display()
            );
        }
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
