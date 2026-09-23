#!/usr/bin/env python3
"""Test the PS/2 -> shell -> VGA path in an isolated, headless QEMU.

Run cargo bootimage --locked, then python tests/smoke_console.py.
Uses only Python's standard library.
"""
import argparse
from http.server import BaseHTTPRequestHandler, HTTPServer
import json
from pathlib import Path
import socket
import subprocess
import tempfile
import threading
import time


class Console:
    def __init__(self, stream, directory):
        self.stream = stream
        self.directory = directory
        self.sequence = 0

    def command(self, name, arguments=None):
        self.sequence += 1
        request = {"execute": name, "id": self.sequence}
        if arguments is not None:
            request["arguments"] = arguments
        self.stream.write(json.dumps(request).encode() + b"\n")
        self.stream.flush()
        while True:
            response = json.loads(self.stream.readline())
            if response.get("id") == self.sequence:
                if "error" in response:
                    raise RuntimeError(response["error"])
                return response.get("return")

    def key(self, *keys):
        self.command("send-key", {
            "keys": [{"type": "qcode", "data": key} for key in keys],
            "hold-time": 1,
        })
        time.sleep(0.025)

    def type(self, text):
        punctuation = {
            " ": ("spc",), "\n": ("ret",), "\t": ("tab",),
            "-": ("minus",), "_": ("shift", "minus"),
            "'": ("apostrophe",), '"': ("shift", "apostrophe"),
            "\\": ("backslash",), "|": ("shift", "backslash"),
            "?": ("shift", "slash"), ".": ("dot",),
            ":": ("shift", "semicolon"), "/": ("slash",),
        }
        for character in text:
            if character.isascii() and character.isalnum():
                self.key(*(("shift", character.lower()) if character.isupper()
                           else (character,)))
            else:
                self.key(*punctuation[character])

    def screen(self):
        target = self.directory / "vga.bin"
        response = self.command("human-monitor-command", {
            "command-line": f'pmemsave 0xb8000 4000 "{target}"',
        })
        if response:
            raise RuntimeError(response)
        data = target.read_bytes()
        rows = [
            bytes(data[row * 160: (row + 1) * 160: 2]).decode("ascii", "replace")
            for row in range(25)
        ]
        content = []
        for y, row in enumerate(rows):
            for title in ("Terminal 1 - crabsh", "Terminal 2 - crabsh"):
                if title in row:
                    x = row.index(title) - 2
                    close = row.find("[x]", x)
                    if close == -1:
                        continue
                    right = close + 3
                    content.extend(line[x + 1:right].strip() for line in rows[y + 2:y + 18])
        return "\n".join(line.strip() for line in rows + content)

    def expect(self, text, timeout=5):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            screen = self.screen()
            if text in screen:
                return screen
            time.sleep(0.05)
        raise AssertionError(f"Missing {text!r} on VGA screen:\n{screen}")

    def move_to(self, x, y):
        for _ in range(160):
            self.screen()
            data = (self.directory / "vga.bin").read_bytes()
            positions = [i for i in range(2000) if data[2*i:2*i+2] == b"+\x1f"]
            assert len(positions) == 1, positions
            current_y, current_x = divmod(positions[0], 80)
            if (current_x, current_y) == (x, y):
                return
            dx = max(-8, min(8, (x - current_x) * 8))
            dy = max(-16, min(16, (y - current_y) * 16))
            self.command("human-monitor-command", {
                "command-line": f"mouse_move {dx} {dy}",
            })
            time.sleep(0.06)
        raise AssertionError(f"Pointer did not reach {(x, y)}: {(current_x, current_y)}")

    def click(self):
        for pressed in (1, 0):
            self.command("human-monitor-command", {
                "command-line": f"mouse_button {pressed}",
            })
            time.sleep(0.1)

    def clean(self):
        self.key("ctrl", "u")
        self.type("clear\n")


def exercise(console, screenshot, nic=False, http_port=None, internet=False):
    console.expect("Click Terminal to get started", timeout=15)
    if screenshot:
        console.command("screendump", {"filename": str(screenshot.with_stem("desktop-empty").resolve())})
    console.move_to(7, 2)
    console.click()
    console.expect("Welcome to CrabOS")
    console.expect("shaolin@crabos:~$")
    console.expect("RAM (boot usable):")
    if nic:
        console.type("net\n")
        console.expect("QEMU e1000")
        console.expect("link up")
        console.type(f"browse http://10.0.2.2:{http_port}/\n")
        console.expect("CrabOS network OK", timeout=15)
        if internet:
            console.type("browse http://example.com/\n")
            console.expect("HTTP/", timeout=15)
    print("PASS: boot, native fastfetch and prompt", flush=True)

    console.clean()
    console.type("echo ac")
    console.key("left")
    console.type("b")
    console.key("end")
    console.type("\n")
    console.expect("\nabc\n")
    console.type("echo aXbc")
    console.key("home")
    for _ in range(6):
        console.key("right")
    console.key("backspace")
    console.key("delete")
    console.key("end")
    console.type("\n")
    console.expect("\nbc\n")
    print("PASS: insertion, Home/End, Backspace/Delete", flush=True)

    console.type('echo "two words" a\\ b \'x|y\'\n')
    console.expect("\ntwo words a b x|y\n")
    console.type('echo "unfinished\n')
    console.expect("crabsh: unclosed quote")
    console.type("echo hi | echo there\n")
    console.expect("pipes, redirection and command chaining are not available")
    console.type("missing-command\n")
    console.expect("command not found")
    print("PASS: quoted arguments and command errors", flush=True)

    console.clean()
    console.type("echo recalled\n")
    console.type("echo draft")
    console.key("up")
    console.expect("shaolin@crabos:~$ echo recalled")
    console.key("down")
    console.expect("shaolin@crabos:~$ echo draft")
    console.key("ctrl", "l")
    console.expect("shaolin@crabos:~$ echo draft")
    console.key("ctrl", "u")
    console.type("fast\t\n")
    console.expect("OS: CrabOS")
    console.type("un\t-a\n")
    console.expect("CrabOS crabos")
    console.type("h\t")
    console.expect("help  history  hostname")
    console.key("ctrl", "u")
    print("PASS: history draft, Ctrl+L/C and completion", flush=True)

    console.clean()
    console.type("echo " + "x" * 260)
    console.key("ctrl", "a")
    console.key("ctrl", "k")
    console.type("echo recovered\n")
    screen = console.expect("\nrecovered\n")
    assert "xxxx" not in screen, screen
    console.type("echo " + "x" * 260 + "\n")
    console.expect("input exceeds 256 bytes; command discarded")
    console.type("echo erase this")
    console.key("ctrl", "w")
    console.type("word\n")
    console.expect("\nerase word\n")
    console.type("discard")
    console.key("ctrl", "u")
    console.type("echo kept\n")
    console.expect("\nkept\n")
    print("PASS: wrapped input, overflow rejection, erase-line and erase-word", flush=True)

    console.clean()
    console.type("echo SCROLLMARK\n")
    for _ in range(6):
        console.type("fastfetch\n")
    screen = console.screen()
    assert "SCROLLMARK" not in screen, screen
    for _ in range(8):
        console.key("pgup")
    console.expect("SCROLLMARK")
    console.key("pgdn")
    console.type("echo live\n")
    console.expect("\nlive\n")
    print("PASS: scrollback and return to live input", flush=True)

    console.clean()
    console.type("uptime\nmem\nhistory\n")
    console.expect("Boot-usable physical RAM:")
    console.expect("Live allocation payload:")
    console.expect("uptime")
    console.clean()
    console.type("echo saved-history\n")
    console.move_to(74, 4)
    console.click()
    console.expect("Click Terminal to get started")
    assert "Terminal 1 - crabsh" not in console.screen()
    console.move_to(7, 2)
    console.click()
    console.key("up")
    console.expect("echo saved-history")
    console.key("ctrl", "u")
    console.click()
    console.expect("Terminal 2 - crabsh")
    console.type("echo second\n")
    console.expect("second")
    console.move_to(45, 4)
    console.command("human-monitor-command", {"command-line": "mouse_button 1"})
    time.sleep(0.1)
    console.move_to(35, 3)
    console.command("human-monitor-command", {"command-line": "mouse_button 0"})
    time.sleep(0.1)
    console.move_to(66, 3)
    console.click()
    assert "Terminal 2 - crabsh" not in console.screen()
    console.type("echo first\n")
    console.expect("first")
    console.key("ctrl", "c")
    console.expect("Click Terminal to get started")
    console.key("ctrl", "q")
    console.expect("Terminal 1 - crabsh")
    console.move_to(30, 1)
    print("PASS: mouse launcher, close buttons, dragging, two shells, reopen history and shortcuts", flush=True)
    console.clean()
    console.type("fastfetch\n")
    screen = console.expect("Terminal: VGA 80x25 / PS/2")
    if screenshot:
        console.command("screendump", {"filename": str(screenshot.resolve())})
    print("PASS: system commands\n\n" + screen, flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image", type=Path,
                        default=Path("target/x86_64-crab_os/debug/bootimage-crab_os.bin"))
    parser.add_argument("--screenshot", type=Path, help="Optional PPM screenshot output")
    parser.add_argument("--nic", action="store_true")
    parser.add_argument("--internet", action="store_true")
    args = parser.parse_args()
    if not args.image.is_file():
        parser.error("boot image missing; run cargo bootimage --locked first")
    with tempfile.TemporaryDirectory(prefix="crabos-console-") as directory:
        directory = Path(directory)
        server = None
        if args.nic:
            class Handler(BaseHTTPRequestHandler):
                def do_GET(self):
                    data = b"<h1>CrabOS network OK</h1>"
                    self.send_response(200)
                    self.send_header("Content-Type", "text/html")
                    self.send_header("Content-Length", str(len(data)))
                    self.end_headers()
                    self.wfile.write(data)

                def log_message(self, *_):
                    pass

            server = HTTPServer(("127.0.0.1", 0), Handler)
            threading.Thread(target=server.serve_forever, daemon=True).start()
        endpoint = directory / "qmp.sock"
        process = subprocess.Popen([
            "qemu-system-x86_64", "-m", "128M", "-display", "none",
            "-drive", f"format=raw,file={args.image.resolve()},snapshot=on",
            "-no-reboot", "-no-shutdown", "-nic", "user,model=e1000" if args.nic else "none",
            "-serial", f"file:{directory / 'serial.txt'}",
            "-qmp", f"unix:{endpoint},server=on,wait=off",
        ], stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
        try:
            deadline = time.monotonic() + 5
            while not endpoint.exists():
                if process.poll() is not None:
                    raise RuntimeError(process.stderr.read().decode())
                if time.monotonic() > deadline:
                    raise TimeoutError("QEMU did not create its control socket")
                time.sleep(0.05)
            with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
                connection.settimeout(5)
                while True:
                    try:
                        connection.connect(str(endpoint))
                        break
                    except ConnectionRefusedError:
                        if process.poll() is not None:
                            raise RuntimeError(process.stderr.read().decode())
                        if time.monotonic() > deadline:
                            raise TimeoutError("QEMU control socket did not accept connections")
                        time.sleep(0.05)
                with connection.makefile("rwb") as stream:
                    greeting = json.loads(stream.readline())
                    assert "QMP" in greeting, greeting
                    console = Console(stream, directory)
                    console.command("qmp_capabilities")
                    exercise(console, args.screenshot, args.nic,
                             server.server_port if server else None, args.internet)
        finally:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)
            if server:
                server.shutdown()
                server.server_close()


if __name__ == "__main__":
    main()
