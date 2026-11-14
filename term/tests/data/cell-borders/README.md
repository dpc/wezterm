# Native tmux cell-border captures

These synthetic byte streams come from a private tmux server attached
to a mock terminal. They contain no user-terminal sessions and stop
before client teardown.

- tmux source: `05be67b9755a44fee3715d25c8acee35f1990204`
- reviewed tmux tree: `da9196321a3fa957d90717d63aa02f78eb220d6c`
- tmux binary SHA-256:
  `cf1cfc6f3b813a711faa636bf2303f15f1fdb543a5c3c64bdfa77e61a230ba48`
- fixture generator SHA-256:
  `59f62e74b961ae26e790bd426ed52ec68cf010612098d1bd6b2a042f5f350daf`
- pinned draft SHA-256:
  `76cd38dd5fd3565ec5ea9989f3fba45bd66b8cedb9da7b90a01138fd684a45be`

Each scenario contains the application output (`apps.bin`), tmux's outer
terminal output (`outer.bin`), replies received by the application
(`app-input.bin`), and capture provenance and pane dimensions (`metadata.json`).
The outer terminal is 80 columns by 24 rows, with the tmux status line disabled.

`border_tmux_native_redraw_matches_direct_application` replays both streams
through the real WezTerm terminal parser and compares every physical cell's
border set, accounting for pane offsets and clipping. For the palette scenario,
it compares resolved colours: tmux realizes pane-local OSC palette overrides
as RGB on the outer terminal. For VS16, both WezTerm decoders explicitly select
Unicode 14 to match tmux's emoji-variation width policy. WezTerm's historical
Unicode 9 default does not widen that sequence; the border protocol does not
change character-width policy.

The reflow scenario ends at its original width. The replay checks the final
state; the live tmux fixture separately checks the intermediate resize.
The screen-reset scenario ends after RIS, so its final comparison is an empty
border screen. Independent WezTerm lifecycle tests cover the intermediate
alternate-screen and saved-pen behavior.

These wire/state interoperability tests are not screenshots or GPU-rendering
tests. Renderer pixel tests and a separately captured private-display demo
provide different evidence.

Run the committed fixtures with the ordinary terminal test suite. To replay a
fresh capture directory with the same layout:

```sh
WEZTERM_CELL_BORDER_FIXTURES=/path/to/captures \
  nix develop ./nix --command \
  cargo test -p wezterm-term --no-default-features border_tmux_native
```
