# tutad

tutad is a daemon between [Tuta](https://tuta.com) and a regular mail server. Mail that arrives at Tuta is written into a Maildir; mail submitted through your SMTP server is sent through Tuta. Dovecot and Postfix do the rest, so any mail client sees an ordinary IMAP account.

It has no IMAP or SMTP server of its own and opens no port.

## Status

Skeleton: the command line and the settings are in place, `run` and `send` are not written yet.

## Usage

```sh
tutad run
tutad send < message.eml
```

The settings go in `~/.config/tutad/config.toml`:

```toml
email = "you@tutanota.com"
maildir = "/var/lib/tutad/Maildir"
```

See the [`man`](./tutad.1.scd) page (`man tutad` once installed).

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
