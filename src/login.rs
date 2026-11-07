//! Getting a logged-in SDK with as few logins as possible, and never a retry.

use std::fs;
use std::io::ErrorKind;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use tokio::time::sleep;
use tutasdk::login::LoginError;
use tutasdk::rest_error::HttpError;
use tutasdk::tutanota_constants::SecondFactorType;
use tutasdk::{ApiCallError, LoggedInSdk, Sdk};

use crate::session;
use crate::totp::Totp;

const CLIENT_NAME: &str = "tutad";
const ATTEMPT_FILE: &str = "login-attempt";
const WAIT_BETWEEN_LOGINS: u64 = 300;
const CODE_MARGIN: u64 = 3;
const POLLS: u32 = 10;

/// How a logged-in SDK came about.
#[derive(Debug, PartialEq, Eq)]
pub enum How {
    /// The saved session was still good.
    Resumed,
    /// A new session was created with the password and a one-time code.
    LoggedIn,
}

#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    SessionInvalid,
    Blocked,
    Outdated,
    Other,
}

fn classify(error: &LoginError) -> Verdict {
    match error {
        LoginError::InvalidAccessToken { .. }
        | LoginError::InvalidPassphrase { .. }
        | LoginError::InvalidKey { .. } => Verdict::SessionInvalid,
        LoginError::ApiCall {
            source: ApiCallError::ServerResponseError { source },
        } => match source {
            HttpError::NotAuthenticatedError
            | HttpError::SessionExpiredError
            | HttpError::AccessExpiredError => Verdict::SessionInvalid,
            HttpError::NotAuthorizedError
            | HttpError::LockedError
            | HttpError::AccessDeactivatedError
            | HttpError::AccessBlockedError
            | HttpError::LimitReachedError => Verdict::Blocked,
            HttpError::InvalidSoftwareVersionError => Verdict::Outdated,
            _ => Verdict::Other,
        },
        LoginError::ApiCall { .. } => Verdict::Other,
    }
}

fn refuse(error: &LoginError, what: &str) -> anyhow::Error {
    match classify(error) {
        Verdict::Blocked => anyhow!(
            "Tuta has blocked, locked or limited the account ({error}); do not log in again"
        ),
        Verdict::Outdated => {
            anyhow!("Tuta refuses this version of tutad ({error}); it needs a new SDK release")
        }
        Verdict::SessionInvalid | Verdict::Other => anyhow!("{what}: {error}"),
    }
}

fn check_attempt(dir: &Path, now: u64) -> Result<()> {
    let text = match fs::read_to_string(dir.join(ATTEMPT_FILE)) {
        Ok(text) => text,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).context("cannot read the last login attempt"),
    };
    let Ok(then) = text.trim().parse::<u64>() else {
        return Ok(());
    };
    let age = now.saturating_sub(then);
    if age < WAIT_BETWEEN_LOGINS {
        bail!(
            "a login was tried {age} s ago; wait {} s before trying again",
            WAIT_BETWEEN_LOGINS - age
        );
    }
    Ok(())
}

fn record_attempt(dir: &Path, now: u64) -> Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    fs::write(dir.join(ATTEMPT_FILE), now.to_string()).context("cannot record the login attempt")
}

fn clear_attempt(dir: &Path) {
    drop(fs::remove_file(dir.join(ATTEMPT_FILE)));
}

async fn log_in(
    sdk: &Sdk,
    email: &str,
    password: &str,
    totp: &Totp,
    dir: &Path,
    mut now: u64,
) -> Result<Arc<LoggedInSdk>> {
    check_attempt(dir, now)?;
    record_attempt(dir, now)?;
    let session = sdk
        .initiate_session(email, password, CLIENT_NAME)
        .await
        .map_err(|error| refuse(&error, "the login was refused"))?;
    let credentials = session.credentials;
    if !session.challenges.is_empty() {
        let totp_type = i64::from(SecondFactorType::Totp);
        if !session.challenges.iter().any(|c| c.r#type == totp_type) {
            bail!("the account asks for a second factor other than TOTP, which tutad cannot give");
        }
        let left = totp.seconds_left(now);
        if left <= CODE_MARGIN {
            sleep(Duration::from_secs(left)).await;
            now += left;
        }
        let code: u32 = totp
            .code(now)?
            .parse()
            .context("the one-time code must have at most six digits")?;
        sdk.authenticate_with_second_factor_totp(&credentials.access_token, code)
            .await
            .map_err(|error| refuse(&error, "the one-time code was refused"))?;
        wait_for_second_factor(sdk, &credentials.access_token).await?;
    }
    let logged_in = sdk
        .login(credentials.clone())
        .await
        .map_err(|error| refuse(&error, "the new session was refused"))?;
    session::save(dir, &credentials)?;
    clear_attempt(dir);
    Ok(logged_in)
}

async fn wait_for_second_factor(sdk: &Sdk, access_token: &str) -> Result<()> {
    for _ in 0..POLLS {
        sleep(Duration::from_secs(1)).await;
        let pending = sdk
            .is_second_factor_pending(access_token)
            .await
            .map_err(|error| refuse(&error, "the second factor check failed"))?;
        if !pending {
            return Ok(());
        }
    }
    bail!("the second factor was still pending after {POLLS} checks")
}

/// Resumes the saved session, or logs in once when there is none.
///
/// A refused saved session is removed and reported, never replaced in the same run.
/// A new login is refused within five minutes of the last one.
///
/// # Errors
///
/// Fails on the first problem, whether from Tuta, the network or the state directory.
pub async fn connect(
    sdk: &Sdk,
    email: &str,
    password: &str,
    totp: &Totp,
    dir: &Path,
    now: u64,
) -> Result<(Arc<LoggedInSdk>, How)> {
    let Some(credentials) = session::load(dir)? else {
        let logged_in = log_in(sdk, email, password, totp, dir, now).await?;
        return Ok((logged_in, How::LoggedIn));
    };
    match sdk.login(credentials).await {
        Ok(logged_in) => Ok((logged_in, How::Resumed)),
        Err(error) if classify(&error) == Verdict::SessionInvalid => {
            session::forget(dir)?;
            bail!("the saved session was refused ({error}) and removed; run again to log in")
        }
        Err(error) => Err(refuse(&error, "the saved session could not be resumed")),
    }
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use tutasdk::bindings::rest_client::{
        HttpMethod, RestClient, RestClientError, RestClientOptions, RestResponse,
    };
    use tutasdk::bindings::test_file_client::TestFileClient;

    use super::*;
    use crate::sdk::{self, fixture};

    const URI: &str = "otpauth://totp/x?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ";

    fn offline_sdk() -> Sdk {
        sdk::new(
            fixture::BASE.to_owned(),
            Arc::new(fixture::rest_client()),
            Arc::new(TestFileClient::default()),
        )
    }

    struct Down;

    #[async_trait]
    impl RestClient for Down {
        async fn request_binary(
            &self,
            _url: String,
            _method: HttpMethod,
            _options: RestClientOptions,
        ) -> Result<RestResponse, RestClientError> {
            Err(RestClientError::NetworkError)
        }
    }

    fn http(error: HttpError) -> LoginError {
        LoginError::ApiCall {
            source: ApiCallError::ServerResponseError { source: error },
        }
    }

    #[test]
    fn errors_are_sorted_by_what_they_say_about_the_session() {
        let invalid = LoginError::InvalidAccessToken {
            error_message: String::new(),
        };
        assert_eq!(classify(&invalid), Verdict::SessionInvalid);
        for error in [
            HttpError::NotAuthenticatedError,
            HttpError::SessionExpiredError,
            HttpError::AccessExpiredError,
        ] {
            assert_eq!(classify(&http(error)), Verdict::SessionInvalid);
        }
        for error in [
            HttpError::AccessDeactivatedError,
            HttpError::AccessBlockedError,
            HttpError::LockedError,
            HttpError::LimitReachedError,
            HttpError::NotAuthorizedError,
        ] {
            assert_eq!(classify(&http(error)), Verdict::Blocked);
        }
        assert_eq!(
            classify(&http(HttpError::InvalidSoftwareVersionError)),
            Verdict::Outdated
        );
        assert_eq!(
            classify(&http(HttpError::InternalServerError)),
            Verdict::Other
        );
        assert_eq!(
            classify(&http(HttpError::TooManyRequestsError {
                suspension_time_sec: None
            })),
            Verdict::Other
        );
    }

    #[test]
    fn a_login_is_refused_for_five_minutes_after_an_attempt() {
        let dir = tempfile::tempdir().unwrap();
        check_attempt(dir.path(), 1000).unwrap();
        record_attempt(dir.path(), 1000).unwrap();
        let message = check_attempt(dir.path(), 1100).unwrap_err().to_string();
        assert!(message.contains("wait 200 s"), "{message}");
        check_attempt(dir.path(), 1300).unwrap();
        clear_attempt(dir.path());
        check_attempt(dir.path(), 1001).unwrap();
    }

    #[tokio::test]
    async fn a_saved_session_is_resumed_without_any_login() {
        let dir = tempfile::tempdir().unwrap();
        session::save(dir.path(), &fixture::credentials()).unwrap();
        let before = fs::read(dir.path().join("session.json")).unwrap();
        let totp = Totp::parse(URI).unwrap();
        let (_, how) = connect(&offline_sdk(), "x@tuta.com", "pw", &totp, dir.path(), 5000)
            .await
            .unwrap();
        assert_eq!(how, How::Resumed);
        assert_eq!(fs::read(dir.path().join("session.json")).unwrap(), before);
        assert!(!dir.path().join(ATTEMPT_FILE).exists());
    }

    #[tokio::test]
    async fn a_second_login_too_soon_makes_no_request() {
        let dir = tempfile::tempdir().unwrap();
        record_attempt(dir.path(), 5000).unwrap();
        let totp = Totp::parse(URI).unwrap();
        let error = connect(&offline_sdk(), "x@tuta.com", "pw", &totp, dir.path(), 5010)
            .await
            .err()
            .unwrap();
        assert!(error.to_string().contains("wait"), "{error}");
    }

    #[tokio::test]
    async fn the_attempt_is_recorded_before_the_request_and_kept_on_failure() {
        let dir = tempfile::tempdir().unwrap();
        let totp = Totp::parse(URI).unwrap();
        let down = sdk::new(
            fixture::BASE.to_owned(),
            Arc::new(Down),
            Arc::new(TestFileClient::default()),
        );
        let result = connect(&down, "x@tuta.com", "pw", &totp, dir.path(), 9000).await;
        assert!(result.is_err());
        let stamp = fs::read_to_string(dir.path().join(ATTEMPT_FILE)).unwrap();
        assert_eq!(stamp, "9000");
        assert!(session::load(dir.path()).unwrap().is_none());
    }
}
