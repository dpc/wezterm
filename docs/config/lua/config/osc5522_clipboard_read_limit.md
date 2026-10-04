# `osc5522_clipboard_read_limit`

Defaults to `64 * 1024 * 1024` bytes. This is an aggregate decoded-byte resource
policy for one OSC5522 read, checked before sending OK. Exceeding it returns EPERM.
It is separate from the protocol's64MiB write capacity and any application limit.
MIME-name listing has an independent64KiB metadata bound.

See [MIME clipboard access](../../../clipboard.md) for queue and native I/O bounds.
