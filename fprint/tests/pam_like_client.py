#!/usr/bin/env python3
"""Drive an fprintd-compatible service the way pam_fprintd does, over one connection.

   pam_like_client.py [--session] [--timeout S] [--hold]
"""
import sys
from gi.repository import Gio, GLib

session = "--session" in sys.argv
timeout = float(sys.argv[sys.argv.index("--timeout") + 1]) if "--timeout" in sys.argv else 30
bus = Gio.bus_get_sync(Gio.BusType.SESSION if session else Gio.BusType.SYSTEM)
NAME = "net.reactivated.Fprint"


def call(path, iface, method, args=None, sig=None):
    return bus.call_sync(NAME, path, iface, method, args, GLib.VariantType(sig) if sig else None,
                         Gio.DBusCallFlags.NONE, -1, None).unpack()


devs = call("/net/reactivated/Fprint/Manager", "net.reactivated.Fprint.Manager", "GetDevices", None, "(ao)")[0]
dev = devs[0]
DEV = "net.reactivated.Fprint.Device"
props = bus.call_sync(NAME, dev, "org.freedesktop.DBus.Properties", "GetAll", GLib.Variant("(s)", (DEV,)),
                      None, Gio.DBusCallFlags.NONE, -1, None).unpack()[0]
print("device", dev, props)
print("enrolled", call(dev, DEV, "ListEnrolledFingers", GLib.Variant("(s)", ("",)), "(as)")[0])

call(dev, DEV, "Claim", GLib.Variant("(s)", ("",)))
print("claimed")
if "--hold" in sys.argv:
    input("holding the claim, press enter to release ")
    call(dev, DEV, "Release")
    sys.exit(0)

loop = GLib.MainLoop()
result = {}


def on_signal(conn, sender, path, iface, signal, params):
    print("signal", signal, params.unpack())
    if signal == "VerifyStatus":
        result["status"] = params.unpack()
        loop.quit()


bus.signal_subscribe(NAME, DEV, None, dev, None, Gio.DBusSignalFlags.NONE, on_signal)
call(dev, DEV, "VerifyStart", GLib.Variant("(s)", ("any",)))
print("verifying, timeout", timeout)
GLib.timeout_add(int(timeout * 1000), lambda: (result.setdefault("status", ("timeout", True)), loop.quit()))
loop.run()
print("result", result["status"])
call(dev, DEV, "VerifyStop")
if result["status"][0] != "verify-match":
    call(dev, DEV, "Release")
# on a match pam_fprintd just disconnects without Release
