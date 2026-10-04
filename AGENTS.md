# Clipboard and PTY changes

Read [SECURITY.md](SECURITY.md) before changing OSC5522, native clipboard
ownership, terminal output serialization, or PTY cancellation. Preserve its
permission, generation, framing, and public-API boundaries and extend the
relevant regression tests.
