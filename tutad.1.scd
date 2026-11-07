TUTAD(1) "github.com/paulchambaz/tutad" "General Commands Manual"

# NAME

*tutad* - daemon between Tuta and a regular mail server

# SYNOPSIS

*tutad* [*-hV*]++
*tutad* *run*++
*tutad* *send* _RECIPIENT_...++
*tutad* *folders*++
*tutad* *fetch* _FOLDER_

# DESCRIPTION

*tutad* connects to a Tuta mailbox and does two things. Mail that arrives at
Tuta is written into a Maildir, which a regular IMAP server such as Dovecot
serves to any mail client. Mail that the client submits through a regular SMTP
server such as Postfix is handed to *tutad*, which sends it through Tuta.

*tutad* has no IMAP or SMTP server of its own and opens no port.

# OPTIONS

*-h*, *--help*
	Print a short help and exit.

*-V*, *--version*
	Print the version and exit.

# COMMANDS

*run*
	Keep the Maildir and the Tuta mailbox in step until stopped. It
	fetches the mail that is missing, follows Tuta's changes as they
	happen, and sends the reading, moving and deleting done in the Maildir
	back to Tuta. A deleted file goes to Tuta's Trash, never beyond.

*send* _RECIPIENT_...
	Read one message on standard input and send it through Tuta to the
	recipients, which are the envelope recipients and so include Bcc. This
	is what the mail server calls for a submitted message. It never logs
	in: it uses the session that *run* saved.

*folders*
	Resume the saved session or log in, print the Tuta folders and stop.
	Nothing is changed in the account.

*fetch* _FOLDER_
	Write the new mail of one folder, named as in the Maildir, and stop.

# EXIT STATUS

*send* exits 0 when Tuta accepted the mail, 65 when the message can never be
sent and the mail server should bounce it, and 75 when it may work later and
the mail server should try again. Every other command exits 0 on success, 2
on a mistake in the command line and 1 on any other failure.

# FILES

_~/.config/tutad/config.toml_
	The settings. *email* is the Tuta address and *maildir* the directory
	of the Maildir. Unknown keys are refused.

_$CREDENTIALS_DIRECTORY/password_, _$CREDENTIALS_DIRECTORY/totp_
	The Tuta password and the *otpauth://* URI of the second factor, as
	systemd's *LoadCredential* provides them.

_$STATE_DIRECTORY_
	Where tutad keeps what it must remember: _session.json_ (the saved
	session), _login-attempt_ (when a login was last tried), _mail.tsv_
	(which file holds which mail), _events.tsv_ (how far the changes in
	Tuta were read), _sent.tsv_ (mails Tuta accepted, to drop the client's
	own copy in Sent) and _lock_ (so only one *run* is alive).

# SETUP

Run *tutad run* as a service and point Dovecot at the Maildir. Give the unit
the credentials with *LoadCredential*, a *StateDirectory*, *UMask=0077* and
*Restart=on-failure* with *RestartSec=300*: tutad never logs in twice within
five minutes, so a failure is one restart every five minutes and never a loop
of requests.

Have Postfix hand the mail of the Tuta address to *send* through a *pipe*
service in _master.cf_. It must start as root, so its unprivileged column is
*n*, and it drops to the user that owns the state directory:

```
tutad unix - n n - 1 pipe flags=Rq user=virtualMail argv=/usr/bin/tutad send ${recipient}
```

Run *tutad run* once before relying on *send*: the first run logs in and saves
the session.

# SAFETY

*run* asks Tuta to slow down when it answers 429 or 503 and waits. It never
deletes beyond Tuta's Trash. It refuses, and logs a warning, to delete in Tuta
more than 100 mails that vanished from the Maildir between two scans, and to
drop more than 100 local mails that Tuta no longer lists. If Tuta blocks,
locks or limits the account, or refuses this version of tutad, *run* exits
instead of retrying.

# DIAGNOSTICS

*tutad* logs to standard error, which systemd keeps in the journal.

_a login was tried N s ago; wait M s before trying again_
	A fresh login is never tried twice within five minutes. Wait.

_the saved session was refused (...) and removed; run again to log in_
	Tuta ended the session. Start *run* once to log in again.

_Tuta has blocked, locked or limited the account (...); do not log in again_
	Stop the service and look at the account in the Tuta app. Do not retry.

_Tuta refuses this version of tutad (...); it needs a new SDK release_
	See UPDATING.

_another tutad is already running_
	A second *run*, *folders* or *fetch* found the lock. *send* takes none.

_N mails vanished at once; not deleting them in Tuta_
	Over 100 files left the Maildir between two scans, likely a restore or a
	wrong mount. Check the Maildir. Tuta was not touched.

_Tuta no longer lists N mails; not removing them_
	After a long stop, over 100 local mails are gone from Tuta. If that is
	right, empty the state directory and the Maildir, then start *run*.

Mail submitted but not sent is held by Postfix when *send* exits 75: no
saved session, *run* is down, or the network failed. Start *run*; the mail
goes out on the next retry.

# STARTING OVER

The Maildir and the state directory describe each other: empty both, or
neither. To fetch everything again, stop *run*, empty the Maildir, delete
_mail.tsv_ and _events.tsv_ from the state directory and start *run*. Keep
_session.json_ so no login is needed.

# UPDATING

Tuta changes its API about every two weeks. To follow a new release, change
*commit=* and *tag=* in _sdk/BASE_, copy TutaBridge's patches for that
release into _sdk/patches/_, run *just sdk*, then *cargo test --all-targets*.
A patch that Tuta merged upstream fails to apply and is removed.

# LIMITS

After more than 44 days without running, tutad cannot replay Tuta's changes.
It then drops the mail that is gone and moves the mail filed elsewhere, but
does not compare read flags.

Tuta has no flags besides read and unread, so tutad sets and reads only the
*S* flag.

# SEE ALSO

*dovecot*(1), *postfix*(1)

# AUTHORS

Paul Chambaz <paul.chambaz@tutanota.com>
