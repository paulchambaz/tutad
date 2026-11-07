//! The Tuta event stream: one websocket that delivers batches of changes as they happen.

use std::time::Duration;

use anyhow::{bail, Context, Result};
use futures::{SinkExt, StreamExt};
use log::{info, warn};
use serde_json::Value;
use tokio::net::TcpStream;
use tokio::time::{sleep, sleep_until, timeout, Instant};
use tokio_tungstenite::tungstenite::{Error, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::cursor::Cursors;

const CLIENT_NAME: &str = "tutad";
const HEARTBEAT: Duration = Duration::from_secs(60);
const IDLE: Duration = Duration::from_secs(90);
const SMALL: (u64, u64) = (5, 10);
const MEDIUM: (u64, u64) = (20, 40);
const LARGE: (u64, u64) = (60, 120);
const SYS_MODELS: &str =
    include_str!("../sdk/generated/tuta-sdk/rust/sdk/src/type_models/sys.json");
const TUTANOTA_MODELS: &str =
    include_str!("../sdk/generated/tuta-sdk/rust/sdk/src/type_models/tutanota.json");

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// What happened to an entity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    Create,
    Update,
    Delete,
}

/// One changed entity, named by its application, type and ids.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Update {
    pub application: String,
    pub type_id: i64,
    pub list_id: String,
    pub id: String,
    pub operation: Operation,
}

/// The changes Tuta made to one group in one step, to be applied together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Batch {
    pub group: String,
    pub id: String,
    pub updates: Vec<Update>,
}

/// A message from the stream that matters to the sync.
#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    /// Changes to apply.
    Batch(Batch),
    /// Tuta has replayed everything missed since the saved cursors.
    CaughtUp,
}

enum Close {
    Stop(u16),
    Pause((u64, u64)),
    Retry,
}

enum Step {
    Event(Event),
    Skip,
    Lost(Close),
}

/// A websocket to Tuta that reconnects by itself and never reconnects when Tuta refuses.
pub struct Stream {
    base_url: String,
    user_id: String,
    access_token: String,
    versions: (u32, u32),
    socket: Option<Socket>,
    failures: u32,
    next_ping: Instant,
}

impl Stream {
    /// A stream for a session, not yet connected.
    ///
    /// # Errors
    ///
    /// Fails when the bundled type models carry no version.
    pub fn new(base_url: &str, user_id: &str, access_token: &str) -> Result<Self> {
        drop(rustls::crypto::ring::default_provider().install_default());
        Ok(Self {
            base_url: base_url.to_owned(),
            user_id: user_id.to_owned(),
            access_token: access_token.to_owned(),
            versions: (version(SYS_MODELS)?, version(TUTANOTA_MODELS)?),
            socket: None,
            failures: 0,
            next_ping: Instant::now(),
        })
    }

    /// Waits for the next message that matters, connecting or reconnecting as needed.
    ///
    /// A reconnect asks Tuta to replay everything after the given cursors. Dropping
    /// the future is safe.
    ///
    /// # Errors
    ///
    /// Fails when Tuta refuses the session, so that the caller stops for good.
    pub async fn next(&mut self, cursors: &Cursors) -> Result<Event> {
        loop {
            if self.socket.is_none() {
                self.connect(cursors).await?;
                continue;
            }
            match self.read().await {
                Step::Event(event) => return Ok(event),
                Step::Skip => {}
                Step::Lost(close) => self.lost(close).await?,
            }
        }
    }

    async fn connect(&mut self, cursors: &Cursors) -> Result<()> {
        let url = build_url(
            &self.base_url,
            self.versions,
            &self.user_id,
            &self.access_token,
            cursors,
        );
        match tokio_tungstenite::connect_async(url).await {
            Ok((socket, _)) => {
                info!("connected to the event stream");
                self.socket = Some(socket);
                self.failures = 0;
                self.next_ping = Instant::now() + HEARTBEAT;
                Ok(())
            }
            Err(Error::Url(_)) => bail!("the event stream address is unusable"),
            Err(Error::Http(response)) => {
                let status = response.status().as_u16();
                match close_action(4000 + status) {
                    Close::Stop(_) => bail!("Tuta refused the event stream ({status})"),
                    close => self.lost(close).await,
                }
            }
            Err(error) => {
                warn!("cannot connect to the event stream: {error}");
                self.lost(Close::Retry).await
            }
        }
    }

    async fn read(&mut self) -> Step {
        let Some(socket) = self.socket.as_mut() else {
            return Step::Lost(Close::Retry);
        };
        tokio::select! {
            () = sleep_until(self.next_ping) => {
                self.next_ping = Instant::now() + HEARTBEAT;
                match socket.send(Message::Ping(Vec::new())).await {
                    Ok(()) => Step::Skip,
                    Err(_) => Step::Lost(Close::Retry),
                }
            }
            frame = timeout(IDLE, socket.next()) => match frame {
                Ok(Some(Ok(Message::Text(text)))) => match parse(&text) {
                    Ok(Some(event)) => Step::Event(event),
                    Ok(None) => Step::Skip,
                    Err(error) => {
                        warn!("ignoring an unreadable message: {error}");
                        Step::Skip
                    }
                },
                Ok(Some(Ok(Message::Ping(payload)))) => {
                    drop(socket.send(Message::Pong(payload)).await);
                    Step::Skip
                }
                Ok(Some(Ok(Message::Close(frame)))) => Step::Lost(
                    frame.map_or(Close::Retry, |frame| close_action(u16::from(frame.code))),
                ),
                Ok(Some(Ok(_))) => Step::Skip,
                Ok(Some(Err(_)) | None) | Err(_) => Step::Lost(Close::Retry),
            },
        }
    }

    async fn lost(&mut self, close: Close) -> Result<()> {
        self.socket = None;
        self.failures = self.failures.saturating_add(1);
        let (low, high) = match close {
            Close::Stop(code) => {
                bail!("Tuta closed the event stream with code {code}; not reconnecting")
            }
            Close::Pause(range) => range,
            Close::Retry => match self.failures {
                0 | 1 => SMALL,
                2 => MEDIUM,
                _ => LARGE,
            },
        };
        let seconds = jitter(low, high);
        info!("event stream lost; reconnecting in {seconds} s");
        sleep(Duration::from_secs(seconds)).await;
        Ok(())
    }
}

fn version(models: &str) -> Result<u32> {
    let models: Value = serde_json::from_str(models).context("malformed type models")?;
    models
        .as_object()
        .and_then(|models| models.values().next())
        .and_then(|model| model.get("version"))
        .and_then(Value::as_u64)
        .and_then(|version| u32::try_from(version).ok())
        .context("the type models carry no version")
}

fn build_url(
    base_url: &str,
    versions: (u32, u32),
    user_id: &str,
    access_token: &str,
    cursors: &Cursors,
) -> String {
    let encode = |text: &str| form_urlencoded::byte_serialize(text.as_bytes()).collect::<String>();
    let base = base_url
        .trim_end_matches('/')
        .replacen("https://", "wss://", 1)
        .replacen("http://", "ws://", 1);
    let mut url = format!(
        "{base}/event?modelVersions={}.{}&clientVersion={}&userId={}&accessToken={}&clientName={CLIENT_NAME}",
        versions.0,
        versions.1,
        encode(tutasdk::CLIENT_VERSION),
        encode(user_id),
        encode(access_token),
    );
    if !cursors.is_empty() {
        url.push_str("&groupsToLastEventBatchIds=");
        for (group, batch) in cursors.all() {
            url.push_str(&format!("{group}={batch};"));
        }
    }
    url
}

fn close_action(code: u16) -> Close {
    match code.checked_sub(4000) {
        Some(401 | 403 | 440 | 470 | 472) => Close::Stop(code),
        Some(1 | 429) => Close::Pause(LARGE),
        _ => Close::Retry,
    }
}

fn jitter(low: u64, high: u64) -> u64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| u64::from(elapsed.subsec_nanos()));
    low + nanos % (high - low + 1)
}

fn parse(text: &str) -> Result<Option<Event>> {
    let (kind, payload) = text.split_once(';').unwrap_or((text, ""));
    match kind {
        "entityUpdate" => Ok(Some(Event::Batch(batch(payload)?))),
        "initialSyncDone" => Ok(Some(Event::CaughtUp)),
        _ => Ok(None),
    }
}

fn text(value: &Value, key: &str) -> Result<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .with_context(|| format!("missing field {key}"))
}

fn number(value: &Value, key: &str) -> Result<i64> {
    match value.get(key) {
        Some(Value::String(text)) => text.parse().with_context(|| format!("bad field {key}")),
        Some(Value::Number(number)) => number.as_i64().with_context(|| format!("bad field {key}")),
        _ => bail!("missing field {key}"),
    }
}

fn batch(payload: &str) -> Result<Batch> {
    let value: Value = serde_json::from_str(payload).context("malformed entityUpdate")?;
    let updates = value
        .get("1487")
        .and_then(Value::as_array)
        .context("missing field 1487")?
        .iter()
        .map(update)
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect();
    Ok(Batch {
        group: text(&value, "1486")?,
        id: text(&value, "1485")?,
        updates,
    })
}

fn update(value: &Value) -> Result<Option<Update>> {
    let operation = match number(value, "624")? {
        0 => Operation::Create,
        1 => Operation::Update,
        2 => Operation::Delete,
        _ => return Ok(None),
    };
    Ok(Some(Update {
        application: text(value, "464")?,
        type_id: number(value, "2556")?,
        list_id: value
            .get("466")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        id: text(value, "467")?,
        operation,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cursors(pairs: &[(&str, &str)]) -> Cursors {
        let dir = tempfile::tempdir().unwrap();
        let mut cursors = Cursors::open(dir.path(), 0).unwrap();
        for (group, batch) in pairs {
            cursors.set(group, batch, 0).unwrap();
        }
        cursors
    }

    const BATCH: &str = r#"entityUpdate;{"1485":"b1","1486":"g1","1487":[
        {"464":"tutanota","2556":"97","466":"L1","467":"M1","624":"0"},
        {"464":"tutanota","2556":1450,"466":"L2","467":"E1","624":"2"},
        {"464":"sys","2556":"5","467":"X","624":"9"}]}"#;

    #[test]
    fn a_batch_of_updates_is_read() {
        let Some(Event::Batch(batch)) = parse(BATCH).unwrap() else {
            panic!("not a batch");
        };
        assert_eq!((batch.group.as_str(), batch.id.as_str()), ("g1", "b1"));
        assert_eq!(batch.updates.len(), 2);
        assert_eq!(
            batch.updates[0],
            Update {
                application: "tutanota".to_owned(),
                type_id: 97,
                list_id: "L1".to_owned(),
                id: "M1".to_owned(),
                operation: Operation::Create,
            }
        );
        assert_eq!(batch.updates[1].operation, Operation::Delete);
        assert_eq!(batch.updates[1].type_id, 1450);
    }

    #[test]
    fn other_messages_are_ignored_and_broken_ones_are_errors() {
        assert_eq!(parse("initialSyncDone").unwrap(), Some(Event::CaughtUp));
        assert_eq!(parse("leaderStatus;{}").unwrap(), None);
        assert_eq!(parse("somethingNew;x").unwrap(), None);
        assert!(parse("entityUpdate;{").is_err());
        assert!(parse(r#"entityUpdate;{"1485":"b","1486":"g"}"#).is_err());
    }

    #[test]
    fn tuta_closing_with_a_refusal_stops_for_good() {
        for code in [4401, 4403, 4440, 4470, 4472] {
            assert!(
                matches!(close_action(code), Close::Stop(c) if c == code),
                "{code}"
            );
        }
        assert!(matches!(close_action(4429), Close::Pause(LARGE)));
        assert!(matches!(close_action(4001), Close::Pause(LARGE)));
        assert!(matches!(close_action(4500), Close::Retry));
        assert!(matches!(close_action(1006), Close::Retry));
        assert!(matches!(close_action(401), Close::Retry));
    }

    #[test]
    fn the_address_carries_the_cursors_and_hides_nothing_unencoded() {
        let url = build_url(
            "https://app.tuta.com/",
            (1, 2),
            "u1",
            "tok/en+1",
            &cursors(&[("g1", "b1"), ("g2", "b2")]),
        );
        assert!(url.starts_with("wss://app.tuta.com/event?modelVersions=1.2&clientVersion="));
        assert!(url.contains("&userId=u1&accessToken=tok%2Fen%2B1&clientName=tutad"));
        assert!(url.ends_with("&groupsToLastEventBatchIds=g1=b1;g2=b2;"));
        let bare = build_url("http://localhost:9000", (1, 2), "u", "t", &cursors(&[]));
        assert!(bare.starts_with("ws://localhost:9000/event?"));
        assert!(!bare.contains("groupsToLastEventBatchIds"));
    }

    #[test]
    fn the_model_versions_are_read_from_the_bundled_models() {
        assert!(version(SYS_MODELS).unwrap() > 0);
        assert!(version(TUTANOTA_MODELS).unwrap() > 0);
        assert!(version("{}").is_err());
    }

    #[test]
    fn the_wait_stays_in_its_range() {
        for _ in 0..50 {
            let seconds = jitter(5, 10);
            assert!((5..=10).contains(&seconds));
        }
        assert_eq!(jitter(7, 7), 7);
    }
}
