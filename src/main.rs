//! Daemon that turns a Tuta mailbox into a Maildir and Tuta submissions into mail.

mod cli;
mod config;
mod credentials;
mod cursor;
mod events;
mod fetch;
mod folders;
mod governor;
mod login;
mod maildir;
mod message;
mod outgoing;
mod render;
mod sdk;
mod send;
mod sent;
mod session;
mod state;
mod sync;
mod totp;
mod writeback;

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use cli::Cli;
use config::Config;
use cursor::Cursors;
use events::{Event, Stream};
use fetch::Fetcher;
use log::{info, warn, LevelFilter, Log, Metadata, Record};
use login::How;
use maildir::Maildir;
use state::State;
use sync::{Layout, Mirror};
use tokio::signal::unix::{signal, SignalKind};
use tokio::time::{sleep, sleep_until, Duration, Instant};
use totp::Totp;
use tutasdk::LoggedInSdk;

const SCAN: Duration = Duration::from_secs(5);
const SETTLE: Duration = Duration::from_secs(2);
const BACKOFF_MIN: Duration = Duration::from_secs(60);
const BACKOFF_MAX: Duration = Duration::from_secs(900);

struct Stderr;

impl Log for Stderr {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.target().starts_with("tutad")
    }

    fn log(&self, record: &Record) {
        if self.enabled(record.metadata()) {
            eprintln!("{}: {}", record.level(), record.args());
        }
    }

    fn flush(&self) {}
}

struct Connected {
    config: Config,
    state: PathBuf,
    sdk: Arc<LoggedInSdk>,
    _lock: File,
}

fn now() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}

fn lock(dir: &Path) -> Result<File> {
    fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let file = File::create(dir.join("lock")).context("cannot create the lock file")?;
    file.try_lock()
        .map_err(|_| anyhow!("another tutad is already running"))?;
    Ok(file)
}

async fn connect() -> Result<Connected> {
    let config = config::load(&config::default_path()?)?;
    let state = config::state_dir()?;
    let credentials = credentials::load(&credentials::directory()?)?;
    let totp = Totp::parse(&credentials.totp)?;
    let lock = lock(&state)?;
    let sdk = sdk::native(&config.api_url, state.clone())?;
    let (logged_in, how) = login::connect(
        &sdk,
        &config.email,
        &credentials.password,
        &totp,
        &state,
        now()?,
    )
    .await?;
    match how {
        How::Resumed => info!("resumed the saved session"),
        How::LoggedIn => info!("logged in and saved the session"),
    }
    Ok(Connected {
        config,
        state,
        sdk: logged_in,
        _lock: lock,
    })
}

async fn list_folders() -> Result<()> {
    let connected = connect().await?;
    for folder in folders::load(&connected.sdk).await? {
        let label = if folder.name.is_empty() {
            format!("{:?}", folder.kind)
        } else {
            folder.name
        };
        println!("{}{label}", "  ".repeat(folder.depth));
    }
    Ok(())
}

async fn fetch_folder(name: &str) -> Result<()> {
    let connected = connect().await?;
    let layout = Layout::new(&folders::load(&connected.sdk).await?).only(name)?;
    let state = State::open(&connected.state)?;
    let maildir = Maildir::open(&connected.config.maildir);
    Mirror::new(connected.sdk, maildir, state)
        .initial(&layout)
        .await?;
    Ok(())
}

async fn run() -> Result<()> {
    let connected = connect().await?;
    let session = session::load(&connected.state)?.context("no saved session after login")?;
    let mut layout = Layout::new(&folders::load(&connected.sdk).await?);
    let mut cursors = Cursors::open(&connected.state, now()?)?;
    let away = cursors.is_empty();
    if away {
        let latest = Fetcher::new(Arc::clone(&connected.sdk))
            .latest_batch()
            .await?;
        if let Some((group, batch)) = latest {
            cursors.set(&group, &batch, now()?)?;
        }
    }
    let state = State::open(&connected.state)?;
    let maildir = Maildir::open(&connected.config.maildir);
    let mut mirror = Mirror::new(connected.sdk, maildir, state);
    sent::compact(&connected.state, now()?)?;
    let mut heads = HashMap::new();
    for pass in 0..2 {
        if pass > 0 {
            sleep(SETTLE).await;
        }
        if let Err(error) = mirror.write_back(&layout).await {
            warn!("not sending local changes to Tuta: {error:#}");
            break;
        }
    }
    drop_copies(&mirror, &connected.state, &mut heads);
    let listing = mirror.initial(&layout).await?;
    if away {
        if let Err(error) = mirror.reconcile(&layout, &listing) {
            warn!("not catching up with Tuta's changes: {error:#}");
        }
    }
    let mut stream = Stream::new(
        &connected.config.api_url,
        &session.user_id.0,
        &session.access_token,
    )?;
    let mut terminate = signal(SignalKind::terminate())?;
    let mut wait = SCAN;
    let mut next_scan = Instant::now() + wait;
    loop {
        let event = {
            let next = stream.next(&cursors);
            tokio::pin!(next);
            loop {
                tokio::select! {
                    event = &mut next => break event?,
                    () = sleep_until(next_scan) => {
                        wait = match mirror.write_back(&layout).await {
                            Ok(()) => SCAN,
                            Err(error) => {
                                warn!("not sending local changes to Tuta: {error:#}");
                                (wait * 2).clamp(BACKOFF_MIN, BACKOFF_MAX)
                            }
                        };
                        drop_copies(&mirror, &connected.state, &mut heads);
                        next_scan = Instant::now() + wait;
                    }
                    _ = tokio::signal::ctrl_c() => return Ok(()),
                    _ = terminate.recv() => return Ok(()),
                }
            }
        };
        match event {
            Event::Batch(batch) => {
                mirror.apply(&mut layout, &batch).await?;
                cursors.set(&batch.group, &batch.id, now()?)?;
            }
            Event::CaughtUp => info!("caught up with the changes made while away"),
        }
    }
}

fn drop_copies(mirror: &Mirror, state: &Path, heads: &mut HashMap<String, Option<String>>) {
    let dropped = now()
        .and_then(|now| sent::load(state, now))
        .and_then(|ids| sent::drop_copies(&mirror.maildir, &mirror.state, heads, &ids));
    match dropped {
        Ok(0) => {}
        Ok(count) => info!("dropped {count} client copies from Sent"),
        Err(error) => warn!("not checking Sent for client copies: {error:#}"),
    }
}

async fn send_mail(recipients: &[String]) -> Result<()> {
    let mut raw = Vec::new();
    std::io::stdin()
        .read_to_end(&mut raw)
        .context("cannot read the message")?;
    let outgoing = outgoing::parse(&raw, recipients)?;
    let config = config::load(&config::default_path()?)?;
    let state = config::state_dir()?;
    let sdk = sdk::native(&config.api_url, state.clone())?;
    let logged_in = login::resume(&sdk, &state).await?;
    send::send(&logged_in, &outgoing).await?;
    if let Some(id) = &outgoing.message_id {
        if let Err(error) = now().and_then(|now| sent::record(&state, id, now)) {
            warn!("sent, but could not note the Message-ID: {error:#}");
        }
    }
    Ok(())
}

const EX_DATAERR: u8 = 65;
const EX_TEMPFAIL: u8 = 75;

fn send_code(error: &anyhow::Error) -> u8 {
    let permanent = error.downcast_ref::<outgoing::Error>().is_some()
        || error.downcast_ref::<send::Refused>().is_some();
    if permanent {
        EX_DATAERR
    } else {
        EX_TEMPFAIL
    }
}

#[tokio::main]
async fn main() -> Result<ExitCode> {
    drop(log::set_logger(&Stderr));
    log::set_max_level(LevelFilter::Info);
    let args: Vec<String> = std::env::args().skip(1).collect();
    match cli::parse(&args) {
        Ok(Cli::Help) => println!("{}", cli::USAGE),
        Ok(Cli::Version) => println!("tutad {}", env!("CARGO_PKG_VERSION")),
        Ok(Cli::Run) => run().await?,
        Ok(Cli::Folders) => list_folders().await?,
        Ok(Cli::Fetch(name)) => fetch_folder(&name).await?,
        Ok(Cli::Send(recipients)) => {
            if let Err(error) = send_mail(&recipients).await {
                eprintln!("tutad: {error:#}");
                return Ok(ExitCode::from(send_code(&error)));
            }
        }
        Err(error) => {
            eprintln!("tutad: {error}\ntry tutad -h");
            return Ok(ExitCode::from(2));
        }
    }
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bad_message_bounces_and_anything_else_is_tried_again() {
        let bad = anyhow::Error::from(outgoing::Error::NoSender);
        assert_eq!(send_code(&bad), EX_DATAERR);
        assert_eq!(send_code(&send::Refused.into()), EX_DATAERR);
        assert_eq!(send_code(&anyhow!("network down")), EX_TEMPFAIL);
        assert_eq!(send_code(&bad.context("while parsing")), EX_DATAERR);
    }
}
