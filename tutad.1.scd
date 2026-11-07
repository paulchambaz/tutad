TUTAD(1) "github.com/paulchambaz/tutad" "General Commands Manual"

# NAME

*tutad* - daemon between Tuta and a regular mail server

# SYNOPSIS

*tutad* [*-hV*]++
*tutad* *run*++
*tutad* *send*

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
	Keep the Maildir and the Tuta mailbox in step until stopped.

*send*
	Read one message on standard input and send it through Tuta. This is
	what the mail server calls for a submitted message.

# FILES

_~/.config/tutad/config.toml_
	The settings. *email* is the Tuta address and *maildir* the directory
	of the Maildir. Unknown keys are refused.

# SEE ALSO

*dovecot*(1), *postfix*(1)

# AUTHORS

Paul Chambaz <paul.chambaz@tutanota.com>
