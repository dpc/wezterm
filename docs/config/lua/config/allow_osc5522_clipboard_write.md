# `allow_osc5522_clipboard_write`

Defaults to `false`. When enabled, foreground applications in an attached native
Wayland pane may replace the clipboard using OSC5522, including arbitrary MIME
formats and aliases. Writes validate and buffer atomically, with a64MiB limit.

See [MIME clipboard access](../../../clipboard.md) for permissions and scope.
