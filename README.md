# tutad

tutad is a daemon between [Tuta](https://tuta.com) and a regular mail server. Mail that arrives at Tuta is written into a Maildir; mail submitted through your SMTP server is sent through Tuta. Dovecot and Postfix do the rest, so any mail client sees an ordinary IMAP account.

It has no IMAP or SMTP server of its own and opens no port.

## Status

Built and tested offline: sync into a Maildir, send, write-back of read flags, moves and deletes, and the removal of the client's duplicate Sent copy. Tried live on a real account: login, the initial fetch of 2,902 mails, reconnecting the event stream, and one plain-text send. Not yet tried live: new, read, move and delete events, attachments, HTML and Bcc on send, write-back, and Sent dedupe. See [`PLAN.md`](./PLAN.md).

## Usage

```sh
tutad run
tutad send RECIPIENT... < message.eml
tutad folders
tutad fetch FOLDER
```

`run` keeps the Maildir and the Tuta mailbox in step until stopped. `send` reads one message on standard input and sends it through Tuta to the recipients; it never logs in and exits 0 when sent, 65 when the message must bounce and 75 when the mail server should try again. `folders` prints the Tuta folders. `fetch` writes the new mail of one folder and stops.

The settings go in `~/.config/tutad/config.toml`:

```toml
email = "you@tutanota.com"
maildir = "/var/lib/tutad/Maildir"
```

The password and the `otpauth://` URI of the second factor are read from the files `password` and `totp` in `$CREDENTIALS_DIRECTORY`, which systemd's `LoadCredential` provides. The session, the record of written mail and the event cursors live in `$STATE_DIRECTORY`.

See the [`man`](./tutad.1.scd) page (`man tutad` once installed), [`CONTRACT.md`](./CONTRACT.md) for the design and [`docs/RUNBOOK.md`](./docs/RUNBOOK.md) for what to do when something fails.

## Building

```sh
nix build
```

Or with cargo:

```sh
cargo build --release
```

`just install` puts `tutad` and its man page in `~/.local`.

## License

GPL-3.0-only.
