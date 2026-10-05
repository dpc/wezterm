# MIME clipboard access (OSC5522)

This fork supports the [Kitty clipboard extension](https://sw.kovidgoyal.net/kitty/clipboard/)
and [mode 5522 paste events](https://rockorager.dev/misc/bracketed-paste-mime/)
inside the existing WezTerm process. Native Wayland clipboard APIs supply arbitrary
MIME data, including screenshots, text and binary formats. There is no clipboard
daemon, helper executable, path injection or automatic file upload.

Support requires a native Wayland GUI and a local Linux PTY. An ordinary `ssh`
process and a protocol-aware tmux can forward the protocol. Other GUI backends,
serial PTYs and WezTerm remote mux/proxy panes report unsupported and keep ordinary
text paste behavior. Closing/detaching the GUI removes capability from surviving
panes.

## Permissions and configuration

```lua
-- Defaults: do not grant background applications clipboard content access.
config.allow_osc5522_clipboard_read = false
config.allow_osc5522_clipboard_write = false
config.osc5522_clipboard_read_limit = 64 * 1024 * 1024
-- Opt in to regular-file bodies on ordinary native paste gestures only.
config.enable_osc5522_file_paste = false
```

With these defaults, an application can list MIME names but must receive a
one-use grant from an actual paste gesture to read content. A grant lasts 30
seconds and applies only to the original pane/window and clipboard location.
Invalid/missing passwords or names use the ordinary configured policy. Enabling
either permission trusts foreground applications in the attached pane to perform
that operation without a prompt. Do not enable these globally for untrusted
applications.

The read limit is an aggregate decoded-byte resource policy. Reads exceeding it
are denied before a success response begins. Writes have a separate 64MiB limit
and support multiple formats and aliases; they replace the native selection only
after the entire write validates. There is no write success backend exemption:
writes work when explicitly allowed, and are denied by default.

Mode 5522 takes precedence over bracketed paste 2004. An enabled application must
fetch plaintext through OSC5522 too; WezTerm does not send a duplicate bracketed
text paste. Existing paste key bindings remain unchanged. Busy gestures are
rejected before admission, not partially delivered. The terminal offers every
available native MIME name, not only image/text types.

Gesture reads retain the original Wayland offer rather than eagerly copying every
format. Owner changes cancel it; a later clipboard owner's contents are never
substituted. This is not an immutable byte snapshot. `text/uri-list` and file
metadata are just bytes by default: WezTerm does not open referenced files
unless `enable_osc5522_file_paste` is enabled for ordinary native paste gestures.

## Copied files, including across SSH

With `enable_osc5522_file_paste = true`, copying local files in a file manager
and using the normal terminal paste action captures their bodies on the
workstation. WezTerm advertises an additional private
`application/vnd.tau.files-v1` format to that gesture's application. An ordinary
SSH connection and protocol-aware, correctly fenced tmux carry actual file
bytes to Tau; the remote host does not reinterpret workstation paths. Ambient
read permission and MIME listing never create this file-body format.
An ordinary URI-list containing only non-file URLs keeps its native clipboard
formats unchanged, so copying a browser URL still follows normal text paste.

Local regular files of any type are supported, without an extension/MIME
whitelist. Symlinks follow normal local resolution, but the opened object must
be regular. Directories, devices, FIFOs and network URLs are refused; a copied
`cut` entry is read, never deleted. Malformed paths or one failing file reject
the whole paste. The current decoded bundle limit is 16MiB **including
filenames and framing**, further constrained by `osc5522_clipboard_read_limit`.
There is no separate file-count limit. Tau's artifact limit is a distinct
per-file 16MiB policy. These limits are not limitations of SSH or OSC5522.

The retained bundle contains the bytes acquired while handling the gesture, not
an atomic filesystem snapshot. Files can change during acquisition. The native
owner and lifecycle checks still revoke the offer; an authorized read never
opens the files again. Filesystem checks run on bounded workers off the GUI
thread, but a blocking filesystem syscall cannot be forcibly interrupted.
See [SECURITY.md](../SECURITY.md) for memory ownership and cancellation boundaries.

The versioned private body format uses unsigned big-endian lengths:

```text
"TAUFILES\0" (9 bytes), u32 file_count
repeat file_count times:
    u16 basename_length, u16 mime_length, u64 body_length
    UTF-8 basename, optional ASCII MIME hint, opaque body bytes
EOF (no trailing bytes)
```

At least one file is required. Names are basenames only, excluding empty names,
`.`/`..`, separators and controls. Duplicates preserve order. The producer leaves
MIME hints empty rather than guessing media types. Names/MIME hints never confer
authority, execution or rendering permission. Consumers must validate complete
framing and correlated DONE before publishing results.

Native reads prebuffer before OK. An unsupported location returns ENOSYS, policy
denial EPERM and temporary failure EBUSY. Permanent native read I/O failure has no
standard status and yields a local diagnostic plus client timeout. Native source
ownership is provisional until the compositor confirms it; reads during that
interval can return EBUSY. Consumers must discard incomplete or stale-ID data.

## Operational limitations

Mode-off/reset/focus changes revoke pending authority, not bytes already written.
An already-started OSC frame finishes before subsequent output when the PTY makes
progress. A slow consumer may stall it indefinitely; there is no live stall
timeout that silently kills normal input. Actual pane close interrupts native I/O.
Hard disconnect can truncate transport. A synchronous Lua or mux write can still
wait behind that frame; GUI key/SendString paths use an explicit ordered queue.

Protocol queue saturation is bounded and can cause a request to time out.
A silent response is never success or proof that older bytes have drained.
See [SECURITY.md](../SECURITY.md) for precise bounds, cancellation and locking rules.

## Manual acceptance checks (not performed by unit tests)

In a separately started test build, with the intended Wayland compositor:

1. Probe 5522 in a local pane, then through ordinary SSH and a matching tmux build.
   Check supported/reset initially; check unsupported on X11 and remote mux panes.
2. Paste a screenshot, then Unicode/multiline plaintext. Fetch PNG/text bytes and
   verify exact bytes/hash, one completed paste and no bracketed duplicate.
3. Copy only PNG, clear the clipboard, replace its owner, and repeat after an
   admitted read. Confirm no stale owner's bytes or partial completed artifacts.
4. Test primary selection where supported, and ENOSYS where unavailable.
5. Confirm default write denial. Explicitly enable writes in the test config;
   transfer 64MiB, multiple MIME types, empty data and aliases. Read them using an
   independent native client. Replacing a source before its old cancellation
   callback must preserve the new clipboard.
6. Switch panes/windows, disable/re-enable the mode, reset, and close the pane
   while native acquisition/output is stalled. Confirm grant replay fails,
   stale replies cannot complete a later request and close remains bounded.

Do not activate/restart an existing desktop session merely to run these checks.
