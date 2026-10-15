#!/usr/bin/env python3
"""Paste a synthetic file export in a disposable headless Wayland desktop.

Pass tool paths as needed. This starts a private D-Bus daemon without service
activation and never connects to the caller's compositor or clipboard. Logs and the pasted file are retained
in the printed private temporary directory. This tests the terminal protocol,
not Tau's artifact store/UI or SSH unless --ssh-command is supplied.
"""
import argparse
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import tempfile
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for tool in ("wezterm", "sway", "swaymsg", "nautilus", "wtype", "wl-paste", "dbus-daemon"):
        parser.add_argument("--" + tool, default=shutil.which(tool))
    parser.add_argument("--ssh-command", help="SSH argv prefix; remote Python must see the test directory")
    parser.add_argument("--tmux", help="Upgraded tmux binary (private test sockets only)")
    parser.add_argument("--tmux-layers", type=int, choices=(1, 2), default=1)
    args = parser.parse_args()
    for tool in ("wezterm", "sway", "swaymsg", "nautilus", "wtype", "wl_paste", "dbus_daemon"):
        if not getattr(args, tool):
            parser.error("provide --" + tool.replace("_", "-"))
    root = Path(tempfile.mkdtemp(prefix="wezterm-file-smoke-", dir="/tmp"))
    print(root, flush=True)
    runtime = root / "runtime"
    runtime.mkdir(mode=0o700)
    destination = root / "destination"
    destination.mkdir()
    home = root / "home"
    home.mkdir()
    env = dict(os.environ)
    for key in ("DISPLAY", "WAYLAND_DISPLAY", "SWAYSOCK", "WEZTERM_UNIX_SOCKET", "WEZTERM_PANE",
                "DBUS_SESSION_BUS_ADDRESS", "DBUS_SESSION_BUS_PID", "SSH_AUTH_SOCK"):
        env.pop(key, None)
    env.update(
        XDG_RUNTIME_DIR=str(runtime), HOME=str(home),
        XDG_CONFIG_HOME=str(home / ".config"), XDG_CACHE_HOME=str(home / ".cache"),
        XDG_DATA_HOME=str(home / ".local/share"),
        WLR_BACKENDS="headless", WLR_HEADLESS_OUTPUTS="1", WLR_RENDERER="pixman",
        WLR_LIBINPUT_NO_DEVICES="1", GDK_BACKEND="wayland",
    )
    (root / "sway.conf").write_text(
        "xwayland disable\noutput * resolution 1280x800\nseat seat0 fallback true\n"
    )
    (root / "bus.conf").write_text(
        '<busconfig><type>session</type><listen>unix:tmpdir=' + str(runtime) +
        '</listen><auth>EXTERNAL</auth><policy context="default">'
        '<allow send_destination="*"/><allow receive_sender="*"/><allow own="*"/>'
        '</policy></busconfig>'
    )
    (root / "wezterm.lua").write_text(
        "return {enable_wayland=true, front_end='Software', "
        "allow_osc5522_clipboard_write=true, check_for_updates=false, "
        "enable_tab_bar=false, window_decorations='NONE'}\n"
    )
    filename = "opaque snow \u2603 #%?.bin"
    body = bytes(range(256)) * 257
    (root / "expected").write_bytes(body)
    (root / "exporter.py").write_text(
        """import base64, os, pathlib, select, struct, sys, termios, time, tty
root = pathlib.Path(sys.argv[1])
name = sys.argv[2].encode()
body = (root / 'expected').read_bytes()
bundle = b'TAUFILES\\0' + struct.pack('>IHHQ', 1, len(name), 0, len(body)) + name + body
old = termios.tcgetattr(0)
tty.setraw(0)
def emit(meta, payload=None):
    frame = b'\\x1b]5522;' + meta.encode()
    if payload is not None: frame += b';' + payload
    os.write(1, frame + b'\\x1b\\\\')
try:
    os.write(1, b'READY')
    (root / 'ready').touch()
    os.read(0, 1)
    # Nested tmux attachments negotiate support asynchronously. Never start a
    # write until the exact probe confirms the endpoint is ready.
    for attempt in range(30):
        os.write(1, b'\\x1b[?5522$p')
        probe = b''
        deadline = time.monotonic() + .5
        while time.monotonic() < deadline and b'$y' not in probe:
            if select.select([0], [], [], .1)[0]: probe += os.read(0, 4096)
        with (root / 'probes').open('ab') as log: log.write(probe)
        if b'\\x1b[?5522;1$y' in probe or b'\\x1b[?5522;2$y' in probe:
            break
        time.sleep(.1)
    else:
        raise RuntimeError('OSC5522 endpoint never became ready')
    os.write(1, b'\\x1b[?5522h')
    emit('type=write-file:id=smoke')
    mime = base64.b64encode(b'application/vnd.tau.file-export-v1').decode()
    for pos in range(0, len(bundle), 4095):
        emit('type=write-file-data:id=smoke:mime=' + mime, base64.b64encode(bundle[pos:pos+4095]))
    emit('type=write-file-data:id=smoke')
    reply = b''
    deadline = time.monotonic() + 20
    while time.monotonic() < deadline and b'\\x1b\\\\' not in reply:
        if select.select([0], [], [], 1)[0]: reply += os.read(0, 4096)
    (root / 'reply').write_bytes(reply)
    while True: os.read(0, 4096)
except Exception as error:
    (root / 'exporter.error').write_text(repr(error))
    raise
finally:
    termios.tcsetattr(0, termios.TCSANOW, old)
"""
    )
    (root / "gate.py").write_text(
        """import os, select, sys, termios, time, tty
# Each tmux attachment probes its parent only once. Gate *before* creating
# each attachment, not merely after the entire nested stack already exists.
old = termios.tcgetattr(0)
tty.setraw(0)
try:
    for attempt in range(60):
        os.write(1, b'\\x1b[?5522$p')
        response = b''
        deadline = time.monotonic() + .5
        while time.monotonic() < deadline and b'$y' not in response:
            if select.select([0], [], [], .1)[0]: response += os.read(0, 4096)
        if b'\\x1b[?5522;1$y' in response or b'\\x1b[?5522;2$y' in response:
            break
        time.sleep(.1)
    else:
        raise RuntimeError('parent OSC5522 endpoint never became ready')
finally:
    termios.tcsetattr(0, termios.TCSANOW, old)
os.execvp(sys.argv[1], sys.argv[1:])
"""
    )
    children = []
    tmux_sockets = []

    def start(argv, label):
        log = open(root / (label + ".log"), "wb")
        child = subprocess.Popen(argv, env=env, stdout=log, stderr=log)
        children.append(child)
        return child

    def run(argv):
        return subprocess.check_output(argv, env=env, stderr=subprocess.STDOUT, timeout=15)

    def wait_for(predicate, label, timeout=30):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            value = predicate()
            if value:
                return value
            time.sleep(0.1)
        raise RuntimeError("timed out: " + label)

    def tree_has(app):
        tree = json.loads(run([args.swaymsg, "-t", "get_tree"]))
        def visit(node):
            return node.get("app_id") == app or any(
                visit(n) for n in node.get("nodes", []) + node.get("floating_nodes", [])
            )
        return visit(tree)

    try:
        start([args.dbus_daemon, "--nofork", "--print-address=1",
               "--config-file=" + str(root / "bus.conf")], "bus")
        def bus_address():
            return next((line for line in (root / "bus.log").read_text().splitlines()
                         if line.startswith("unix:")), None)
        env["DBUS_SESSION_BUS_ADDRESS"] = wait_for(bus_address, "private bus")
        start([args.sway, "-c", str(root / "sway.conf")], "sway")
        display = wait_for(
            lambda: [p for p in runtime.glob("wayland-*") if p.is_socket()],
            "compositor socket",
        )[0]
        env["WAYLAND_DISPLAY"] = display.name
        env["SWAYSOCK"] = str(wait_for(lambda: list(runtime.glob("sway-ipc.*.sock")), "IPC")[0])
        command = [sys.executable, str(root / "exporter.py"), str(root), filename]
        if args.tmux:
            config = root / "tmux.conf"
            config.write_text("set -s native-clipboard on\nset -g status off\n")
            for layer in range(args.tmux_layers):
                socket = root / f"tmux-{layer}.sock"
                tmux_sockets.append(socket)
                command = [sys.executable, str(root / "gate.py"), "env", "-u", "TMUX", args.tmux, "-S", str(socket),
                           "-f", str(config), "new-session", shlex.join(command)]
        if args.ssh_command:
            command = shlex.split(args.ssh_command) + [shlex.join(command)]
        start([args.wezterm, "--config-file", str(root / "wezterm.lua"),
               "start", "--always-new-process", "--"] + command, "wezterm")
        wait_for(lambda: (root / "ready").exists(), "exporter ready")
        wait_for(lambda: tree_has("org.wezfurlong.wezterm"), "terminal window")
        run([args.swaymsg, '[app_id="org.wezfurlong.wezterm"] focus'])
        (root / "tree.json").write_bytes(run([args.swaymsg, "-t", "get_tree"]))
        # Keep a keyboard on the headless seat: destroying its last keyboard
        # revokes focus, correctly denying an asynchronous clipboard request.
        start([args.wtype, "-s", "1000", "-k", "a", "-s", "60000"], "keyboard")
        wait_for(lambda: (root / "reply").exists() or (root / "exporter.error").exists(), "export reply")
        assert not (root / "exporter.error").exists(), (root / "exporter.error").read_text()
        reply = (root / "reply").read_bytes()
        assert reply.startswith(b"\x1b]5522;") and reply.endswith(b"\x1b\\"), repr(reply)
        fields = reply[7:-2].decode().split(":")
        assert len(fields) == 3, repr(reply)
        assert dict(field.split("=", 1) for field in fields) == {
            "type": "write-file", "id": "smoke", "status": "DONE"
        }, repr(reply)
        for mime in ("text/uri-list", "x-special/gnome-copied-files", "application/x-kde-cutselection"):
            value = run([args.wl_paste, "--no-newline", "--type", mime])
            (root / (mime.replace("/", "_") + ".bin")).write_bytes(value)
        start([args.nautilus, "--new-window", str(destination)], "nautilus")
        wait_for(lambda: tree_has("org.gnome.Nautilus"), "file-manager window")
        run([args.swaymsg, '[app_id="org.gnome.Nautilus"] focus'])
        time.sleep(1)
        run([args.wtype, "-M", "ctrl", "-k", "v", "-m", "ctrl"])
        pasted = destination / filename
        wait_for(pasted.exists, "actual file-manager paste")
        assert pasted.read_bytes() == body
        print("PASS: native formats, actual Nautilus paste, exact opaque bytes and basename", flush=True)
    finally:
        for socket in tmux_sockets:
            try:
                subprocess.run([args.tmux, "-S", str(socket), "kill-server"], env=env,
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=5)
            except (OSError, subprocess.TimeoutExpired) as error:
                print(f"Could not stop private tmux socket {socket}: {error}", file=sys.stderr)
        for child in reversed(children):
            child.terminate()
        for child in reversed(children):
            try:
                child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait()


if __name__ == "__main__":
    main()
