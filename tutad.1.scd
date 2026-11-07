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
