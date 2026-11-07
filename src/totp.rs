//! One-time codes from an `otpauth://` URI, as printed by `pass otp uri`.

use data_encoding::BASE32_NOPAD;
use hmac::{Hmac, KeyInit, Mac};
use sha1::Sha1;
use thiserror::Error;

/// An `otpauth://` URI that cannot be used.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum Error {
    /// The URI is not an `otpauth://totp/` one.
    #[error("not an otpauth://totp URI")]
    NotTotp,
    /// The URI carries no `secret` parameter.
    #[error("the URI has no secret")]
    NoSecret,
    /// The secret is not valid base32.
    #[error("the secret is not valid base32")]
    BadSecret,
    /// Only SHA1 is supported, as in every common authenticator.
    #[error("unsupported algorithm: {0}")]
    Algorithm(String),
    /// A numeric parameter is out of range.
    #[error("invalid {0}")]
    Number(&'static str),
}

/// The parameters of a time-based one-time password.
#[derive(Debug, PartialEq, Eq)]
pub struct Totp {
    key: Vec<u8>,
    digits: u32,
    period: u64,
}

impl Totp {
    /// Reads the secret, digits and period from a URI.
    ///
    /// # Errors
    ///
    /// Fails when the URI is not a usable `otpauth://totp/` one.
    pub fn parse(uri: &str) -> Result<Self, Error> {
        let (_, query) = uri
            .trim()
            .strip_prefix("otpauth://totp/")
            .ok_or(Error::NotTotp)?
            .split_once('?')
            .ok_or(Error::NoSecret)?;
        let mut secret = None;
        let mut digits = 6;
        let mut period = 30;
        for (name, value) in query.split('&').filter_map(|pair| pair.split_once('=')) {
            match name {
                "secret" => secret = Some(value),
                "digits" => digits = value.parse().map_err(|_| Error::Number("digits"))?,
                "period" => period = value.parse().map_err(|_| Error::Number("period"))?,
                "algorithm" if !value.eq_ignore_ascii_case("SHA1") => {
                    return Err(Error::Algorithm(value.to_owned()));
                }
                _ => {}
            }
        }
        if !(1..=9).contains(&digits) {
            return Err(Error::Number("digits"));
        }
        if period == 0 {
            return Err(Error::Number("period"));
        }
        let secret = secret.ok_or(Error::NoSecret)?;
        let key = BASE32_NOPAD
            .decode(secret.to_ascii_uppercase().trim_end_matches('=').as_bytes())
            .map_err(|_| Error::BadSecret)?;
        Ok(Self {
            key,
            digits,
            period,
        })
    }

    /// The code valid at a time given in seconds since the Unix epoch.
    ///
    /// # Errors
    ///
    /// Fails when the secret cannot key the HMAC.
    #[cfg_attr(not(test), expect(dead_code))]
    pub fn code(&self, unix_seconds: u64) -> Result<String, Error> {
        let mut mac = Hmac::<Sha1>::new_from_slice(&self.key).map_err(|_| Error::BadSecret)?;
        mac.update(&(unix_seconds / self.period).to_be_bytes());
        let digest = mac.finalize().into_bytes();
        let offset = usize::from(digest[19] & 0x0f);
        let truncated = u32::from_be_bytes([
            digest[offset],
            digest[offset + 1],
            digest[offset + 2],
            digest[offset + 3],
        ]) & 0x7fff_ffff;
        let code = truncated % 10u32.pow(self.digits);
        Ok(format!("{code:0width$}", width = self.digits as usize))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RFC_URI: &str =
        "otpauth://totp/x?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ&digits=8&period=30";

    #[test]
    fn rfc_6238_vectors_match() {
        let totp = Totp::parse(RFC_URI).unwrap();
        for (time, expected) in [
            (59, "94287082"),
            (1_111_111_109, "07081804"),
            (1_111_111_111, "14050471"),
            (1_234_567_890, "89005924"),
            (2_000_000_000, "69279037"),
            (20_000_000_000, "65353130"),
        ] {
            assert_eq!(totp.code(time).unwrap(), expected);
        }
    }

    #[test]
    fn defaults_are_six_digits_and_thirty_seconds() {
        let totp =
            Totp::parse("otpauth://totp/a:b?secret=gezdgnbvgy3tqojqgezdgnbvgy3tqojq&issuer=a")
                .unwrap();
        assert_eq!(totp.code(59).unwrap(), "287082");
    }

    #[test]
    fn a_trailing_newline_is_ignored() {
        assert!(Totp::parse(&format!("{RFC_URI}\n")).is_ok());
    }

    #[test]
    fn bad_uris_are_refused() {
        assert_eq!(Totp::parse("https://x"), Err(Error::NotTotp));
        assert_eq!(Totp::parse("otpauth://totp/x"), Err(Error::NoSecret));
        assert_eq!(
            Totp::parse("otpauth://totp/x?issuer=a"),
            Err(Error::NoSecret)
        );
        assert_eq!(
            Totp::parse("otpauth://totp/x?secret=!!!"),
            Err(Error::BadSecret)
        );
        assert_eq!(
            Totp::parse("otpauth://totp/x?secret=GEZA&algorithm=SHA256"),
            Err(Error::Algorithm("SHA256".to_owned()))
        );
        assert_eq!(
            Totp::parse("otpauth://totp/x?secret=GEZA&period=0"),
            Err(Error::Number("period"))
        );
        assert_eq!(
            Totp::parse("otpauth://totp/x?secret=GEZA&digits=12"),
            Err(Error::Number("digits"))
        );
    }
}
