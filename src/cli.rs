//! The command line: what the user asked for, read with no I/O.

use thiserror::Error;

/// The usage shown by `-h` and after a mistake.
pub const USAGE: &str = "Usage: tutad [-hV]
       tutad run
       tutad send
       tutad folders

A daemon between Tuta and a regular mail server. `run` keeps a Maildir in
step with the Tuta mailbox; `send` reads one message on standard input and
sends it through Tuta, for Postfix to call; `folders` logs in or resumes the
saved session, prints the Tuta folders and changes nothing in the account.

  -h    print this help and exit
  -V    print the version and exit

See man tutad for the rest.";

/// What the user asked for.
#[derive(Debug, PartialEq, Eq)]
pub enum Cli {
    /// Keep the Maildir and the Tuta mailbox in step.
    Run,
    /// Send the message on standard input through Tuta.
    Send,
    /// Log in or resume, print the Tuta folders and stop.
    Folders,
    /// Print the usage.
    Help,
    /// Print the version.
    Version,
}

/// A command line that cannot be understood.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum Error {
    /// The first word is not a command or a flag.
    #[error("unknown argument: {0}")]
    Unknown(String),
    /// A command takes no further argument.
    #[error("unexpected argument: {0}")]
    Extra(String),
    /// Nothing was asked for.
    #[error("missing command")]
    Missing,
}

/// Reads the arguments after the program name.
///
/// # Errors
///
/// Fails when the arguments are not a command or a flag.
pub fn parse(args: &[String]) -> Result<Cli, Error> {
    let (first, rest) = args.split_first().ok_or(Error::Missing)?;
    if let Some(extra) = rest.first() {
        return Err(Error::Extra(extra.clone()));
    }
    match first.as_str() {
        "run" => Ok(Cli::Run),
        "send" => Ok(Cli::Send),
        "folders" => Ok(Cli::Folders),
        "-h" | "--help" => Ok(Cli::Help),
        "-V" | "--version" => Ok(Cli::Version),
        other => Err(Error::Unknown(other.to_owned())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| (*word).to_owned()).collect()
    }

    #[test]
    fn commands_and_flags_are_read() {
        assert_eq!(parse(&args(&["run"])), Ok(Cli::Run));
        assert_eq!(parse(&args(&["send"])), Ok(Cli::Send));
        assert_eq!(parse(&args(&["folders"])), Ok(Cli::Folders));
        assert_eq!(parse(&args(&["-h"])), Ok(Cli::Help));
        assert_eq!(parse(&args(&["--version"])), Ok(Cli::Version));
    }

    #[test]
    fn mistakes_are_refused() {
        assert_eq!(parse(&[]), Err(Error::Missing));
        assert_eq!(
            parse(&args(&["fly"])),
            Err(Error::Unknown("fly".to_owned()))
        );
        assert_eq!(
            parse(&args(&["run", "now"])),
            Err(Error::Extra("now".to_owned()))
        );
    }
}
