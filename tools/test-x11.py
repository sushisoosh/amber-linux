#!/usr/bin/env python3
"""Run native tests or an application smoke test on authenticated, disposable Xvfb+i3.

Requires the normal Linux build dependencies plus Python 3, Xvfb, xauth,
xdpyinfo, i3 and i3-msg. Native mode tests GTK window hints without WebKit,
a user bus or audio.
Run from any directory: python3 tools/test-x11.py
Use --app /absolute/path/to/amber to test a built application without running Cargo;
this mode also requires dbus-run-session and uses a private, activation-free bus.
"""

import argparse
import ctypes
import json
import os
from pathlib import Path
import resource
import select
import shutil
import signal
import subprocess
import sys
import tempfile
import time


def build_native_tests(repo):
    build_env = os.environ.copy()
    for key in ("DISPLAY", "WAYLAND_DISPLAY", "XAUTHORITY"):
        build_env.pop(key, None)
    build = subprocess.run(
        ["cargo", "test", "--locked", "-p", "amber-pc", "--lib", "--no-run", "--message-format=json"],
        cwd=repo, env=build_env, stdout=subprocess.PIPE, text=True,
    )
    messages = [json.loads(line) for line in build.stdout.splitlines()]
    for message in messages:
        if message.get("reason") == "compiler-message" and (build.returncode or message["message"]["level"] == "error"):
            print(message["message"]["rendered"], end="", flush=True)
    if build.returncode:
        raise SystemExit(build.returncode)
    binaries = [
        message["executable"]
        for message in messages
        if message.get("reason") == "compiler-artifact"
        and message.get("executable") and message["profile"]["test"]
        and message["target"]["name"] == "amber_pc_lib"
    ]
    if len(binaries) != 1:
        raise RuntimeError(f"Expected one desktop unit-test executable, got {binaries}")
    return binaries[0]


def smoke_app(app, root, env, ipc, processes):
    settings = Path(env["XDG_CONFIG_HOME"]) / "app.amber.pc"
    settings.mkdir(mode=0o700)
    (settings / "settings.json").write_text(json.dumps({
        "onboarded": True, "autostart": False, "autoUpdate": False,
        "local": {"keepLoaded": False}, "cleanup": {"smart": False}, "profiles": [],
        "phone": {"enabled": False, "tunnel": False}, "sounds": False,
        "hotkey": {"dictate": "", "toggleBubble": ""},
        "terminal": {"enabled": False, "hotkey": ""},
    }), encoding="ascii")
    # No standard session config: it could activate host desktop/audio services.
    bus_config = root / "dbus.conf"
    bus_config.write_text(
        '<busconfig><type>session</type>'
        f'<listen>unix:path={root}/run/bus</listen><auth>EXTERNAL</auth>'
        '<policy context="default"><allow send_destination="*"/>'
        '<allow receive_sender="*"/><allow own="*"/></policy></busconfig>',
        encoding="ascii",
    )
    env = {**env, "DBUS_SESSION_BUS_ADDRESS": f"unix:path={root}/run/bus",
           "NO_AT_BRIDGE": "1", "PULSE_SERVER": f"unix:{root}/no-pulse"}
    with (root / "app.log").open("w+") as log:
        process = subprocess.Popen(
            ["dbus-run-session", "--config-file", str(bus_config), "--", str(app)],
            env=env, cwd=root, stdout=log, stderr=subprocess.STDOUT, start_new_session=True,
        )
        processes.append(process)

        def wait_for_settings(present, timeout):
            deadline = time.monotonic() + timeout
            stable_since = None
            while time.monotonic() < deadline:
                if process.poll() is not None:
                    raise RuntimeError(f"Amber exited during application smoke test (status {process.returncode})")
                tree = subprocess.run(
                    ["i3-msg", "-s", str(ipc), "-t", "get_tree"], env=env,
                    capture_output=True, text=True, check=True, timeout=3,
                )
                nodes = [json.loads(tree.stdout)]
                window = None
                while nodes:
                    node = nodes.pop()
                    nodes.extend(node.get("nodes", []) + node.get("floating_nodes", []))
                    if node.get("window") and node.get("window_properties", {}).get("title") == "Amber":
                        window = node["window"]
                if bool(window) == present:
                    if stable_since is None:
                        stable_since = time.monotonic()
                    # Keep pumping the real Tao event loop, not merely observing a launch.
                    if time.monotonic() - stable_since >= (3 if present else 0.3):
                        if process.poll() is not None:
                            raise RuntimeError(f"Amber exited during application smoke test (status {process.returncode})")
                        return window
                else:
                    stable_since = None
                time.sleep(0.1)
            raise RuntimeError(f"Amber settings window did not become {'visible' if present else 'closed'} within {timeout}s")

        try:
            window = wait_for_settings(True, 30)
            print("Amber survived startup; settings window remained visible for 3 seconds.", flush=True)
            # i3 sends WM_DELETE_WINDOW; Amber should keep running after settings closes.
            closed = subprocess.run(
                ["i3-msg", "-s", str(ipc), f"[id={window}] kill"], env=env,
                capture_output=True, text=True, check=True, timeout=3,
            )
            if not all(reply.get("success") for reply in json.loads(closed.stdout)):
                raise RuntimeError(f"Could not close Amber settings: {closed.stdout}")
            wait_for_settings(False, 5)
            reopened = subprocess.Popen(
                [str(app)], env=env, cwd=root, stdout=log, stderr=subprocess.STDOUT, start_new_session=True,
            )
            processes.append(reopened)
            if reopened.wait(timeout=10) != 0:
                raise RuntimeError(f"Second Amber launch failed (status {reopened.returncode})")
            wait_for_settings(True, 15)
            print("PASS: Amber survived startup, settings close, and single-instance reopen (3s stable each).", flush=True)
            return 0
        except Exception:
            log.seek(0)
            print(log.read(), flush=True)
            raise


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--app", type=Path, help="smoke-test this existing binary; do not build or run native tests")
    args = parser.parse_args()
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(143))
    resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
    # Adopt/reap WebKit or D-Bus descendants, including ones that leave their group.
    libc = ctypes.CDLL(None, use_errno=True)
    if libc.prctl(36, 1, 0, 0, 0) != 0:  # PR_SET_CHILD_SUBREAPER
        raise OSError(ctypes.get_errno(), "Could not become a child subreaper")
    for tool in ("Xvfb", "xauth", "xdpyinfo", "i3", "i3-msg", "dbus-run-session" if args.app else "cargo"):
        if shutil.which(tool) is None:
            raise RuntimeError(f"Required executable is missing: {tool}")
    repo = Path(__file__).resolve().parent.parent
    if args.app:
        args.app = args.app.resolve(strict=True)
        if not args.app.is_file() or not os.access(args.app, os.X_OK):
            parser.error("--app must name an executable file")
    binary = None if args.app else build_native_tests(repo)

    # Keep build caches outside the isolated HOME; the test binary needs no Cargo env.
    with tempfile.TemporaryDirectory(prefix="amber-x11-test-") as directory:
        root = Path(directory)
        auth = root / "Xauthority"
        config = root / "i3.config"
        ipc = root / "i3.sock"
        env = {key: os.environ[key] for key in ("PATH", "LD_LIBRARY_PATH", "FONTCONFIG_FILE", "FONTCONFIG_PATH") if key in os.environ}
        for key, child in (("HOME", "home"), ("XDG_CONFIG_HOME", "config"), ("XDG_DATA_HOME", "data"), ("XDG_CACHE_HOME", "cache"), ("XDG_RUNTIME_DIR", "run")):
            (root / child).mkdir(mode=0o700)
            env[key] = str(root / child)
        env.update({
            "LANG": "C.UTF-8", "XAUTHORITY": str(auth), "XDG_SESSION_TYPE": "x11",
            "GDK_BACKEND": "x11", "GSETTINGS_BACKEND": "memory",
            "RUST_BACKTRACE": "1",
            "DBUS_SESSION_BUS_ADDRESS": f"unix:path={root}/no-session-bus",
            "DBUS_SYSTEM_BUS_ADDRESS": f"unix:path={root}/no-system-bus",
            "AMBER_X11_TEST_ROOT": str(root),
        })
        cookie = os.urandom(16).hex()
        auth.touch(mode=0o600)
        subprocess.run(["xauth", "-f", str(auth), "add", ":0", ".", cookie], env=env, check=True, timeout=10)
        config.write_text(
            "# i3 config file (v4)\nfont pango:monospace 8\nfocus_follows_mouse no\n"
            "mouse_warping none\nworkspace_auto_back_and_forth no\n"
            f"ipc-socket {ipc}\n", encoding="ascii",
        )
        processes = []
        read_fd, write_fd = os.pipe()
        try:
            with (root / "Xvfb.log").open("w+") as xlog, (root / "i3.log").open("w+") as ilog:
                server = subprocess.Popen(
                    ["Xvfb", "-displayfd", str(write_fd), "-screen", "0", "1024x768x24", "-nolisten", "tcp", "-auth", str(auth), "-noreset"],
                    env=env, pass_fds=(write_fd,), stdout=xlog, stderr=subprocess.STDOUT, start_new_session=True,
                )
                processes.append(server)
                os.close(write_fd)
                write_fd = -1
                if not select.select([read_fd], [], [], 15)[0]:
                    raise RuntimeError("Xvfb did not publish a fresh display")
                number = os.read(read_fd, 64).decode("ascii").strip()
                if not number.isdecimal():
                    raise RuntimeError("Invalid Xvfb display number")
                display = f":{number}"
                if display == os.environ.get("DISPLAY", "").split(".")[0]:
                    raise RuntimeError("Refusing to reuse the caller's display")
                env.update({"DISPLAY": display, "AMBER_X11_TEST_DISPLAY": display, "AMBER_X11_TEST_SERVER_PID": str(server.pid)})
                subprocess.run(["xauth", "-f", str(auth), "add", display, ".", cookie], env=env, check=True, timeout=10)
                subprocess.run(["xdpyinfo"], env=env, stdout=subprocess.DEVNULL, check=True, timeout=10)
                unauthenticated = subprocess.run(
                    ["xdpyinfo"], env={**env, "XAUTHORITY": str(root / "absent-auth")},
                    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=10,
                )
                if unauthenticated.returncode == 0:
                    raise RuntimeError("Xvfb unexpectedly accepted an unauthenticated connection")
                wm = subprocess.Popen(["i3", "-c", str(config), "--shmlog-size", "0"], env=env, stdout=ilog, stderr=subprocess.STDOUT, start_new_session=True)
                processes.append(wm)
                deadline = time.monotonic() + 15
                while not ipc.exists():
                    if wm.poll() is not None or time.monotonic() >= deadline:
                        ilog.seek(0)
                        raise RuntimeError(f"Isolated i3 failed to start: {ilog.read()}")
                    time.sleep(0.05)
                subprocess.run(["i3-msg", "-s", str(ipc), "-t", "get_version"], env=env, check=True, timeout=10)
                if args.app:
                    print(f"Application smoke test on disposable {display}: {args.app}; private D-Bus, no real config.", flush=True)
                    return smoke_app(args.app, root, env, ipc, processes)
                guard = subprocess.run(
                    [binary, "linux_desktop::x11::tests::clipboard_targets_utf8_limits_and_replacement", "--ignored", "--exact"],
                    env={key: value for key, value in env.items() if not key.startswith("AMBER_X11_TEST_")},
                    cwd=repo, capture_output=True, text=True, timeout=10,
                )
                if guard.returncode == 0 or "Run python3 tools/test-x11.py" not in guard.stdout + guard.stderr:
                    raise RuntimeError("Native test did not refuse an unguarded DISPLAY")
                print(f"Native X11 tests on disposable {display}; no real desktop connection.", flush=True)
                result = subprocess.run(
                    [binary, "linux_desktop::x11::tests::", "--ignored", "--test-threads=1", "--nocapture"],
                    env=env, cwd=repo, timeout=90,
                )
                if result.returncode:
                    ilog.seek(0)
                    print(ilog.read())
                return result.returncode
        finally:
            os.close(read_fd)
            if write_fd != -1:
                os.close(write_fd)
            for process in reversed(processes):
                try:
                    os.killpg(process.pid, signal.SIGTERM)
                except ProcessLookupError:
                    pass
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    pass
                # Also kill descendants when the leader has already exited (e.g. WebKit).
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                process.wait(timeout=5)
            deadline = time.monotonic() + 5
            while True:
                try:
                    child, _ = os.waitpid(-1, os.WNOHANG)
                except ChildProcessError:
                    break
                if child:
                    continue
                for child in Path(f"/proc/self/task/{os.getpid()}/children").read_text().split():
                    try:
                        os.kill(int(child), signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                if time.monotonic() >= deadline:
                    raise RuntimeError("Timed out cleaning up adopted subprocesses")
                time.sleep(0.05)


if __name__ == "__main__":
    raise SystemExit(main())
