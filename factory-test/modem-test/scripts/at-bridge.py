#!/usr/bin/env python3
"""Host side of the AT bridge (src/at_bridge.rs), over J-Link RTT.

Starts JLinkExe with an RTT telnet port, then forwards lines both ways. Every
line the board sends is printed and logged with the host's wall-clock time in
front, so other instruments (a logging PSU, a scope) line up with it.

Usage:
    ./at-bridge.py                      # interactive: type AT commands or !on, !off, ...
    ./at-bridge.py --script boot.txt    # run a script, then exit
    ./at-bridge.py --log run.log ...    # also write everything to a file

Script lines are sent as typed, except:
    sleep <seconds>
    wait <regex> [timeout_s]   wait for a board line matching regex (default 30 s)
    # comment
A `wait` that times out stops the script with exit code 1.
"""

import argparse
import datetime
import queue
import re
import socket
import subprocess
import sys
import threading
import time

RTT_ADDRESS = "0x2009FF00"


def stamp():
    return datetime.datetime.now().isoformat(timespec="milliseconds")


class Bridge:
    def __init__(self, args):
        self.log = open(args.log, "a", encoding="utf-8") if args.log else None
        self.lines = queue.Queue()
        self.jlink = subprocess.Popen(
            [
                "JLinkExe", "-NoGui", "1", "-Device", args.device, "-If", "SWD",
                "-Speed", "4000", "-AutoConnect", "1",
                "-RTTTelnetPort", str(args.port),
            ],
            stdin=subprocess.PIPE,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            text=True,
        )
        # The control block is pinned, so J-Link need not scan RAM for it
        self.jlink.stdin.write(f"exec SetRTTAddr {RTT_ADDRESS}\n")
        self.jlink.stdin.flush()
        self.sock = self._connect(args.port)
        threading.Thread(target=self._reader, daemon=True).start()

    @staticmethod
    def _connect(port):
        deadline = time.monotonic() + 10
        while True:
            try:
                return socket.create_connection(("127.0.0.1", port), timeout=1)
            except OSError:
                if time.monotonic() > deadline:
                    sys.exit("no RTT telnet port: is the J-Link connected to the board?")
                time.sleep(0.2)

    def _reader(self):
        pending = b""
        self.sock.settimeout(None)
        while True:
            data = self.sock.recv(4096)
            if not data:
                self.lines.put(None)
                return
            pending += data
            *complete, pending = pending.split(b"\n")
            for raw in complete:
                line = raw.decode("utf-8", errors="replace").rstrip("\r")
                if line.startswith("SEGGER J-Link"):
                    continue
                self.emit(line)
                self.lines.put(line)

    def emit(self, line):
        text = f"{stamp()} {line}"
        print(text, flush=True)
        if self.log:
            self.log.write(text + "\n")
            self.log.flush()

    def send(self, line):
        self.emit(f"host> {line}")
        self.sock.sendall(line.encode() + b"\n")

    def wait(self, pattern, timeout_s):
        regex = re.compile(pattern)
        deadline = time.monotonic() + timeout_s
        while (left := deadline - time.monotonic()) > 0:
            try:
                line = self.lines.get(timeout=left)
            except queue.Empty:
                break
            if line is None:
                return False
            if regex.search(line):
                return True
        return False

    def close(self):
        self.sock.close()
        self.jlink.terminate()


def run_script(bridge, path):
    with open(path, encoding="utf-8") as handle:
        for raw in handle:
            line = raw.strip()
            if not line or line.startswith("#"):
                continue
            word, _, rest = line.partition(" ")
            if word == "sleep":
                time.sleep(float(rest))
            elif word == "wait":
                pattern, _, timeout = rest.rpartition(" ")
                if not pattern or not re.fullmatch(r"[0-9.]+", timeout):
                    pattern, timeout = rest, "30"
                # Drop what arrived before this point, so an old match does not count
                while not bridge.lines.empty():
                    bridge.lines.get_nowait()
                if not bridge.wait(pattern, float(timeout)):
                    bridge.emit(f"host! timeout waiting for /{pattern}/")
                    return 1
            else:
                bridge.send(line)
    return 0


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--script", help="run this script, then exit")
    parser.add_argument("--log", help="append everything to this file")
    parser.add_argument("--device", default="STM32L4R7ZI")
    parser.add_argument("--port", type=int, default=19021)
    args = parser.parse_args()

    bridge = Bridge(args)
    try:
        if args.script:
            time.sleep(0.5)
            return run_script(bridge, args.script)
        for line in sys.stdin:
            bridge.send(line.rstrip("\n"))
    except KeyboardInterrupt:
        pass
    finally:
        bridge.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
