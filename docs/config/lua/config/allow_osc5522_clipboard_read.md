# `allow_osc5522_clipboard_read`

Defaults to `false`. When enabled, foreground applications in an attached native
Wayland pane may read clipboard content without a user paste grant. MIME-name
listing is always prompt-free. Actual paste grants work with this option off.

This trusts applications with clipboard contents; enable it only deliberately.
See [MIME clipboard access](../../../clipboard.md) for scope and resource limits.
