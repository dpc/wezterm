# Experimental cell borders

This fork implements the complete version 1 cell-border draft pinned to
`joshka/ghostty` commit `62f25299908b4fc432dca76f330bf43da7185a02`,
file `specs/cell-border-protocol.md`. The file's SHA-256 is
`76cd38dd5fd3565ec5ea9989f3fba45bd66b8cedb9da7b90a01138fd684a45be`.
This is an experimental, unregistered protocol, not a terminal standard.

Borders belong to physical terminal cells, not text characters or overlay
objects. They do not consume character positions and are not copied into
selected text. All twelve half-edge and centre-arm segments are supported,
with independent RGB, palette, or default-foreground colours, widths, inside
or centred placement, and below-text or above-text layers.

The implementation also supports solid, double, dashed and dotted strokes;
priority-based shared-edge arbitration; rounded corners, forks and fillets;
explicit radii; and outside-corner fills. Palette and default colours resolve
at draw time. Double-width and double-height *lines* remain undefined by the
draft; ordinary wide characters are supported.

## Detect support

Send the border query followed by Primary Device Attributes:

```text
ESC P 1 + b cell-border;q ESC \ ESC [ c
```

The response, before the DA1 reply, is:

```text
ESC P 1 + b cell-border;ok;1;pen,rect,half,center,palette,style,round,priority,outside,radius ESC \
```

The spaces separate notation; do not transmit them. In Python, the query
bytes are `b"\x1bP1+bcell-border;q\x1b\\\x1b[c"`.
Do not infer support from `TERM` or `TERM_PROGRAM`. An ordinary multiplexer can
discard the query and answer DA1 without a border reply. The corresponding
tmux fork negotiates support with its attached terminals and serializes native
cell-border commands; passthrough is not required.

## Pen and rectangles

For example, `cell-border;pen;255;16;p2;0;0` sets all outer edges in the pen
to palette colour 2, inside the cell, below text. Wrap this payload in
`ESC P 1 + b` and `ESC \`. Printed and newly blanked cells take the pen.
An ordinary wide character omits the internal left/right edges between its
two cells.

`cell-border;set;3;16;80c5b7;1;1;2;20` sets the two top half-edges of **every**
cell in a two-row, twenty-column rectangle starting at the cursor. It does
not draw only the rectangle's perimeter. Rectangles clip to the screen,
ignore scrolling margins and leave the pen unchanged. They can address the
two cells of a wide glyph independently.

SGR 0 clears the pen, including joint flags, radius and outside colour.
Other SGR parameters preserve it. Saved cursors save the pen; alternate
screens retain their own cells; scrolling and reflow carry borders with
cells, including bordered trailing blanks. Erasures take the pen, while
selective erasures preserve protected cells. DECSTR clears the pen without
clearing existing borders. RIS removes borders from both screens and
scrollback. DECCOLM clears screen borders without changing the pen.

Malformed commands do not apply partially. Payload storage is capped at 256
bytes; oversized or canceled commands are discarded. Unknown extension keys
and unknown values are ignored. For repeated keys, the last *recognized*
value wins, matching the pinned reference implementation.

## Demo and verification

Generate a demonstration stream without touching a terminal:

```sh
python3 assets/cell-borders-demo.py --output /tmp/cell-borders-demo.bin
```

To display it, send the file to a terminal after detecting support.
The demo clears the visible screen and hides the cursor. Restore the cursor
with `printf '\033[?25h\033[0m'` afterwards.

Local verification uses the repository's Nix development shell:

```sh
nix develop ./nix --command cargo test -p wezterm-term --no-default-features
nix develop ./nix --command cargo test -p wezterm-escape-parser --no-default-features --features std
nix develop ./nix --command cargo test -p wezterm-gui --no-default-features --features wayland
```

The terminal tests exercise real escape streams and lifecycle operations.
Renderer tests check pixel coverage, shared edges, double-line junctions,
rounded forks, radius continuation and outside fills. Geometry derives from
the pinned Ghostty reference, with its MIT notice retained in the source.
Border geometry is recomputed for each visible pane, outside cached text rows,
so neighboring cells and palette changes cannot leave stale border paint.
Antialiased curves emit more quads than straight borders; very dense rounded
grids have higher rendering and memory costs.

## Native WezTerm multiplexer compatibility

Cell attributes now carry border data, so the native WezTerm mux codec
version increases from 45 to 46. Upgrade native WezTerm mux servers and
clients together. This does not change the tmux protocol or require tmux
passthrough. Building or publishing these branches does not activate or
restart any running terminal or multiplexer.
