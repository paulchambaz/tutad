# tutad

tutad is a daemon between [Tuta](https://tuta.com) and a regular mail server. Mail that arrives at Tuta is written into a Maildir; mail submitted through your SMTP server is sent through Tuta. Dovecot and Postfix do the rest, so any mail client sees an ordinary IMAP account.

It has no IMAP or SMTP server of its own and opens no port. Reading, moving and deleting in your client are written back to Tuta, and the copy of a sent mail that your client files in Sent is dropped in favour of the one Tuta keeps.

## Installation

### Nix

```sh
nix profile install github:paulchambaz/tutad
```

Or add `github:paulchambaz/tutad` as an input of your flake and install its `packages.default`. The man page is installed with it.

### Manual

See the [**Building**](#building) section.

## Usage

tutad is meant to run as a service, next to Dovecot and Postfix:

```
Tuta <-> tutad run <-> Maildir <-> Dovecot <-> your client
                                                    |
Tuta <-- tutad send <-- Postfix <-------------------+
```

`tutad run` keeps the Maildir and the Tuta mailbox in step until stopped. It fetches what is missing, follows Tuta's changes as they happen and, every few seconds, sends the reading, moving and deleting done in the Maildir back to Tuta. A deleted file goes to Tuta's Trash, never beyond. Point Dovecot at the Maildir.

Postfix hands each message from your Tuta address to `tutad send` through a `pipe` service. The service must start as root (`unpriv` is `n` in `master.cf`) and drop to the user that owns the state directory:

```
tutad unix - n n - 1 pipe flags=Rq user=virtualMail argv=/usr/bin/tutad send ${recipient}
```

`send` reads one message on standard input and sends it through Tuta to the recipients, Bcc included. It never logs in: it uses the session that `run` saved. It exits 0 when sent, 65 when the message must bounce and 75 when Postfix should try again.

Two more commands help when setting up: `tutad folders` prints the Tuta folders, and `tutad fetch FOLDER` writes the new mail of one folder and stops. Neither changes anything in the account.

Run `tutad run` once by hand before relying on `send`: the first run logs in and saves the session.

## Configuration

The settings go in `~/.config/tutad/config.toml`:

```toml
email = "you@tutanota.com"
maildir = "/var/lib/tutad/Maildir"
```

- `email` is the Tuta address.
- `maildir` is the directory of the Maildir that Dovecot serves.

The password and the `otpauth://` URI of the second factor are read from the files `password` and `totp` in `$CREDENTIALS_DIRECTORY`, which systemd's `LoadCredential` provides. The session, the record of written mail and the event cursors live in `$STATE_DIRECTORY`. Keep that directory and the Maildir together: empty both or neither.

A unit that fits:

```ini
[Service]
ExecStart=/usr/bin/tutad run
StateDirectory=tutad
LoadCredential=password:/etc/tutad/password
LoadCredential=totp:/etc/tutad/totp
Restart=on-failure
RestartSec=300
UMask=0077
```

`RestartSec=300` matters: tutad never logs in twice within five minutes, so a failure shows as one restart every five minutes and never as a loop of requests.

Everything is detailed in the [`man`](./tutad.1.scd) page (`man tutad` once installed), including what to do when something fails.

## Limits

- Tuta has no flags besides read and unread, so tutad sets and reads only the Maildir flag `S`.
- After more than 44 days without running, Tuta's changes can no longer be replayed. tutad then drops the mail that is gone and moves the mail filed elsewhere, but does not compare read flags.
- tutad talks to Tuta's own API, which changes about every two weeks. A release that Tuta refuses needs a new build; see UPDATING in the man page.

## Building

### Nix

```sh
git clone https://github.com/paulchambaz/tutad.git
cd tutad
nix develop # a shell with every dev tool
nix build
just --list # the dev commands
```

### Manual

You need `cargo`, `scdoc` for the man page and `just` for the dev commands.

```sh
just build
just test
just lint
just install # binary and man page in ~/.local
```

## Contribution

Contributions are welcome: feature suggestions, bug reports or code, through issues and pull requests on GitHub.

## License

<a href="https://www.gnu.org/licenses/gpl-3.0.en.html">
  <img src="https://www.gnu.org/graphics/gplv3-127x51.png" alt="GNU GPLv3" />
</a>

tutad is free software. You can use, study, modify and distribute it under the
terms of the [GNU General Public License](https://www.gnu.org/licenses/gpl-3.0.en.html),
version 3.
