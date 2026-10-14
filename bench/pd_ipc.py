#!/usr/bin/env python3
"""Tiny client for the playdropd D-Bus interface, used by bench/desktop-to-phone.sh.

The daemon takes files only as open file descriptors (docs/ipc.md section 2, SR-26), so
`busctl call ... Request` cannot send files. This script passes the fds with GDBus.

  pd_ipc.py request '<json>'             one JSON request, prints the JSON response
  pd_ipc.py send <peer> <path>...        SendFiles; prints the response
  pd_ipc.py run <peer> <path>...         SendFiles, then poll `transfers` until the new
                                         transfer finishes; prints one JSON result line
                                         {id,state,bytes,total,files,wall_s,mib_s,note}

Needs python3-gi (PyGObject), which Pop!_OS and Ubuntu ship. Run with python3 -I.
"""

import json
import os
import sys
import time

import gi

gi.require_version("Gio", "2.0")
from gi.repository import Gio, GLib  # noqa: E402

NAME = "org.playdrop.PlayDrop.Daemon1"
PATH = "/org/playdrop/PlayDrop/Daemon1"
IFACE = "org.playdrop.PlayDrop.Daemon1"
FINISHED = {"complete", "failed", "cancelled"}


def proxy():
    bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
    return bus


def call(bus, method, params, fds=None, timeout_ms=600000):
    msg = Gio.DBusMessage.new_method_call(NAME, PATH, IFACE, method)
    msg.set_body(params)
    if fds is not None:
        msg.set_unix_fd_list(fds)
    reply = bus.send_message_with_reply_sync(
        msg, Gio.DBusSendMessageFlags.NONE, timeout_ms, None
    )[0]
    if reply.get_message_type() == Gio.DBusMessageType.ERROR:
        raise SystemExit(f"{method}: {reply.get_error_name()}: {reply.get_body()}")
    return reply.get_body().unpack()[0]


def request(bus, obj):
    return json.loads(call(bus, "Request", GLib.Variant("(s)", (json.dumps(obj),))))


def open_fd(path):
    if os.path.isdir(path):
        return os.open(path, os.O_PATH | os.O_DIRECTORY)
    return os.open(path, os.O_RDONLY)


def send_files(bus, peer, paths):
    fdl = Gio.UnixFDList()
    opened = [open_fd(p) for p in paths]
    handles = [fdl.append(fd) for fd in opened]
    for fd in opened:
        os.close(fd)
    body = GLib.Variant("(sah)", (peer, handles))
    return json.loads(call(bus, "SendFiles", body, fdl))


def transfers(bus):
    r = request(bus, {"op": "transfers"})
    if r.get("result") != "ok":
        raise SystemExit(f"transfers: {r}")
    return r["data"]


def run(bus, peer, paths):
    before = {t["id"] for t in transfers(bus)}
    t0 = time.monotonic()
    r = send_files(bus, peer, paths)
    if r.get("result") != "ok":
        raise SystemExit(f"send failed: {r}")
    tid = r["data"]["id"] if isinstance(r.get("data"), dict) else None
    if tid is None or tid in before:
        raise SystemExit(f"unexpected send response: {r}")
    while True:
        snap = next((t for t in transfers(bus) if t["id"] == tid), None)
        if snap and snap["state"] in FINISHED:
            wall = time.monotonic() - t0
            print(
                json.dumps(
                    {
                        "id": tid,
                        "state": snap["state"],
                        "bytes": snap["bytes"],
                        "total": snap["total"],
                        "files": snap["file_count"],
                        "wall_s": round(wall, 3),
                        "mib_s": round(snap["total"] / 1048576 / wall, 2) if wall else 0,
                        "note": snap.get("note"),
                    }
                )
            )
            return 0 if snap["state"] == "complete" else 1
        time.sleep(0.1)


def main(argv):
    if len(argv) < 2:
        print(__doc__)
        return 2
    bus = proxy()
    cmd = argv[1]
    if cmd == "request":
        print(json.dumps(request(bus, json.loads(argv[2]))))
        return 0
    if cmd == "send":
        print(json.dumps(send_files(bus, argv[2], argv[3:])))
        return 0
    if cmd == "run":
        return run(bus, argv[2], argv[3:])
    print(__doc__)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv))
