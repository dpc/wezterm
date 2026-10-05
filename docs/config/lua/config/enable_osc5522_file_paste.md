# `enable_osc5522_file_paste`

Defaults to `false`. When enabled, an ordinary native Wayland paste gesture
captures workstation-local regular files named by the retained clipboard offer
and adds the private `application/vnd.tau.files-v1` body format. This allows
protocol-aware applications such as Tau to receive actual copied-file bytes
through SSH and tmux instead of just receiving URI metadata.

This is separate from ambient OSC5522 read/write permissions. Background reads
and MIME listings do not resolve copied-file paths. The gesture bundle is capped
at 16MiB including metadata/framing and is subject to the configured native-read
limit. Symlinks follow normal local resolution; directories, special files and
network URLs are not read.

See [MIME clipboard access](../../../clipboard.md) and `SECURITY.md` for the
complete authority, retained-memory and filesystem cancellation boundaries.
