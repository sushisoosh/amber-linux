#!/usr/bin/env python3
"""Run opt-in native tests on a new authenticated Xvfb+i3, never the caller's display.

Requires the normal Linux build dependencies plus Python 3, Xvfb, xauth,
xdpyinfo, i3 and i3-msg. No GTK initialization, WebKit, user bus or audio.
Run from any directory: python3 tools/test-x11.py
"""

import json
import os
from pathlib import Path
import select
import shutil
import subprocess
import tempfile
import time


def main():
    for tool in ("cargo", "Xvfb", "xauth", "xdpyinfo", "i3", "i3-msg"):
        if shutil.which(tool) is None:
            raise RuntimeError(f"Required executable is missing: {tool}")
    repo = Path(__file__).resolve().parent.parent
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
        return build.returncode
    binaries = [
        message["executable"]
        for message in messages
        if message.get("reason") == "compiler-artifact"
        and message.get("executable") and message["profile"]["test"]
        and message["target"]["name"] == "amber_pc_lib"
    ]
    if len(binaries) != 1:
        raise RuntimeError(f"Expected one desktop unit-test executable, got {binaries}")

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
        subprocess.run(["xauth", "-f", str(auth), "add", ":0", ".", cookie], env=env, check=True)
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
                    env=env, pass_fds=(write_fd,), stdout=xlog, stderr=subprocess.STDOUT,
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
                subprocess.run(["xauth", "-f", str(auth), "add", display, ".", cookie], env=env, check=True)
                subprocess.run(["xdpyinfo"], env=env, stdout=subprocess.DEVNULL, check=True, timeout=10)
                unauthenticated = subprocess.run(
                    ["xdpyinfo"], env={**env, "XAUTHORITY": str(root / "absent-auth")},
                    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=10,
                )
                if unauthenticated.returncode == 0:
                    raise RuntimeError("Xvfb unexpectedly accepted an unauthenticated connection")
                wm = subprocess.Popen(["i3", "-c", str(config), "--shmlog-size", "0"], env=env, stdout=ilog, stderr=subprocess.STDOUT)
                processes.append(wm)
                deadline = time.monotonic() + 15
                while not ipc.exists():
                    if wm.poll() is not None or time.monotonic() >= deadline:
                        ilog.seek(0)
                        raise RuntimeError(f"Isolated i3 failed to start: {ilog.read()}")
                    time.sleep(0.05)
                subprocess.run(["i3-msg", "-s", str(ipc), "-t", "get_version"], env=env, check=True, timeout=10)
                print(f"Native X11 tests on disposable {display}; no real desktop connection.", flush=True)
                result = subprocess.run(
                    [binaries[0], "linux_desktop::x11::tests::", "--ignored", "--test-threads=1", "--nocapture"],
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
                if process.poll() is None:
                    process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()


if __name__ == "__main__":
    raise SystemExit(main())
