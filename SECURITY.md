# OSC5522 security and runtime boundaries

This fork implements OSC5522 inside WezTerm, not in a separate service.
Read [the user-facing clipboard documentation](docs/clipboard.md) for configuration
and [the protocol](https://sw.kovidgoyal.net/kitty/clipboard/) for wire semantics.

## Authority

Only a native Wayland GUI attached to a cancellation-capable local Linux PTY
advertises mode 5522. Ordinary `ssh` running inside that PTY is supported.
X11/macOS/Windows, serial/remote WezTerm mux domains, headless panes, and detached
GUI endpoints must not advertise this capability. Linux PTYs created while
`enable_wayland` is configured prepare cancellable I/O **before their first
reader syscall**. An X11 fallback or headless pane under that configuration also
uses poll-backed blocking I/O, but does not gain clipboard capability.

Ambient content reads and writes are denied by default. Listing MIME names is
prompt-free and never issues/renews a content grant. A real paste gesture retains
the exact native offer and all MIME names. Its random UTF-8 password is scoped
to the GUI attachment, pane, selection location and a 30-second lifetime.
The first admitted authorized read of an available format consumes it.
Missing/invalid credentials fall back to configured permissions, not to a
different grant. Ambient reads never resolve files named by clipboard bytes.

`enable_osc5522_file_paste` is default-off and separate from ambient permissions.
When enabled, an ordinary native paste gesture may resolve the exact retained
offer's `text/uri-list` (or GNOME copied-file metadata) on the workstation and
snapshot local regular-file bodies. Only `file:` URIs with empty/localhost
authority are accepted; queries, fragments, malformed escapes and NUL paths are
rejected. Ordinary symlinks are followed. Linux O_PATH pins the resolved object;
fstat must identify a regular file before procfs opens that same descriptor for
reading. Directories, devices, FIFOs and network URLs are not read, and cut never
deletes. File formats/extensions are not permission filters.
Pure non-file URI-lists preserve the native offer without file acquisition;
mixed file/non-file lists and non-file GNOME copied-file entries are rejected.

The private `application/vnd.tau.files-v1` offer carries exact opaque bodies and
basename-only metadata, never source paths or remote filesystem authority. SSH
and tmux carry those bytes normally. The aggregate bundle cap is currently
16MiB including framing, additionally constrained by the configured native read
limit; it matches Tau's current clipboard acquisition policy, not an OSC5522
protocol limit. No discretionary file-count cap is added. Any invalid file fails
the gesture as a whole, before an offer/grant is emitted.

These are bytes read during gesture acquisition, not an atomic filesystem
snapshot: files may change during a read. Native ownership/lifecycle revocation
still invalidates the retained offer. No ambient request or prompt-free MIME
listing creates this synthetic file-body offer. Subsequent authorized reads use
the captured body, not a later filesystem read.

Retaining a native offer is **not an immutable snapshot**: an owner can alter
its bytes. Never substitute a newer native offer for an admitted gesture read.
Owner replacement/clear revokes retained identity. Local publication cannot bind
the old object to a new generation while awaiting compositor confirmation.
Pinned SCTK0.19.2 omits its ordinary-selection NULL callback; our narrow dispatch
shim forwards that event after the toolkit updates its cache.

Focus/pane/overlay changes, mode-off, resets, replacement, destruction and
disconnect revoke authority. Soft reset preserves the mode but revokes its work.
Capture the request/gesture epoch before queuing GUI work: stale work must never
adopt a new epoch. Native commit and revocation share a lock; release it and the
session lock before taking the terminal/output lock. Do not promise observation
of remote shell foreground-process changes; applications must disable before
handoff.

## Bounds and errors

Writes assemble atomically, accept 64MiB, decode concatenated base64 per format,
share alias allocations, and drain failed transactions until a fresh `write`.
Read chunks independently encode at most 4096 bytes. Reads prebuffer before OK;
the configurable 64MiB aggregate default is a permission/resource policy, not a
protocol limit or Tau's separate 16MiB artifact limit. Policy denial is EPERM,
unavailable location ENOSYS, temporary acquisition failure EBUSY. Other native
read I/O failures have no defined wire status: no OK or invented error is sent;
the consumer times out. Diagnostics contain no clipboard data or credentials.

Per attachment: at most 64 queued GUI protocol requests of at most 128KiB, 64
response slots, and a configured read-limit payload budget including the
actively writing response. MIME/list metadata is capped at 64KiB and 1024 names.
Control responses/listings are separately bounded by response count. GUI request
overflow revokes the affected generation rather than dropping a write chunk and
committing a prefix. Response saturation can leave requests unanswered; it never
proves successful completion. Reserve an offer slot **before** accepting a
gesture or replacing its grant. No unsolicited EBUSY is synthesized for a gesture.

Native and gesture-file acquisition share at most 4 workers globally, each with its own configured
aggregate cap and a 10-second total deadline; these prebuffers are **additional**
to queued/active response memory. File acquisition uses the same existing deadline
across metadata and bodies, with checks between filesystem operations. A blocking
filesystem syscall (including a slow mounted filesystem) cannot be forcibly
interrupted: neither the deadline nor generation checks promise hard filesystem
cancellation.

Each attachment can retain one gesture file snapshot of at most 16MiB, **in
addition** to queued/active response memory. A replacement may temporarily hold
the old snapshot plus the one currently being built, until admission. Snapshot
inventory validity guards retain only the native identity, not bodies; queued
MIME listings cannot hide 64 full snapshots outside the payload budget. Revocation
of the attachment drops the session snapshot immediately; one weak expiry task
per attachment sleeps to the exact grant deadline, reaping expired snapshots
even when no request arrives; it stops when no grant remains. Native owner
changes invalidate authority immediately and release snapshots on reap.
Grant admission moves the snapshot into its read. Body response allocations
share the snapshot and remain subject to the existing response payload budget.
Retained bundle backing capacity equals its accounted length. During acquisition,
growth requests never exceed the bundle cap, but reallocation/compaction can
temporarily hold old and new backing allocations (up to twice that cap), in
addition to bounded metadata bytes/path objects and any previous session snapshot.

Native source transfer uses one poll-multiplexed
worker with 128 total queued/active pipes, a conservative 256MiB retained-payload
reference budget, and 10-second inactivity deadlines. Overload refuses the newest
native pipe with a diagnostic, **without clearing clipboard ownership**. Wayland
pipes have no error status: refused/failed transfers have an unavoidable
EOF/empty-data ambiguity for cooperative native consumers. Do not claim lossless
native overload handling. Own reads request formats sequentially, and all
admitted sender pipes progress independently. Normal input retains its pre-existing queue;
these bounds are not a claim that all terminal memory is bounded.

## Output and teardown

One writer orders protocol responses and terminal probes. Hold the shared native
writer mutex for each complete frame, including short writes. Generation checks
suppress not-yet-started frames. A started frame may finish after revocation;
accepted bytes cannot be retracted. Incomplete transactions have no DONE and must
not be published. An old asynchronous producer may enqueue later, but its captured
epoch prevents any bytes being emitted after a newer generation's response.

Live PTY backpressure has **no fatal timeout**. A blocked frame can remain pending
until the consumer resumes or the pane is actually closed. Kill/drop cancellation
must reach nonblocking poll loops independently of writer locks, including reader
clones. Hard transport teardown may truncate a frame; never append cleanup/input
to a failed partial stream. Real I/O failure latches all writer clones and emits
a fixed persistent pane notice; cancellation does not append EOT.

Explicit GUI `enqueue_input` acknowledges queueing, like ordinary terminal keys.
Public `Pane::writer`, mux `WriteToPane`, and Lua `pane:send_text` retain synchronous
write/flush/error semantics. Such synchronous calls can still block, including
Lua running on a GUI thread. Do not claim universal GUI nonblocking behavior.

## Verification

Tests cover pure protocol state, shared synthetic v2 wire frames, malformed and
fragmented parsing, real Unix PTY cancellation/reader parity, native byte pipes,
offer generations, and short-write/FIFO/budget behavior. The shared fixture file
is synthetic input, not a captured end-to-end execution. These tests do not
replace actual compositor clipboard ownership tests or a live
Wayland→SSH→tmux→application smoke test. No desktop activation is part of the
automated checks. See `docs/clipboard.md` for the manual checks.
