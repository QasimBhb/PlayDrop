# Can't find your device?

If a device does not show up, PlayDrop can usually tell you which layer failed. Open
**Diagnostics** (in the desktop window, or in the phone app's settings). It names the layer and
the likely fix. The apps show the same help under "Can't find your device?" (on the phone, "Can't
find your computer?"), with the likely fix first.

There are three layers, from the network up.

## 1. The devices are not on the same network

Symptom: Diagnostics says there is no interface on the same network, or that a device was last
seen on another network.

- Put both on the same Wi-Fi, or use one of the guest Wi-Fi paths below.
- **Client isolation.** Guest, hotel and campus Wi-Fi often stop devices from reaching each
  other. Nothing in PlayDrop can get around that. Use a guest Wi-Fi path below.
- **A VPN** may route local traffic away. Allow local network access in the VPN app, or pause the
  VPN while you use PlayDrop.

## 2. The network hides devices from each other (multicast blocked)

Symptom: both devices are on the same Wi-Fi, but neither appears in the other's list.

Mesh Wi-Fi, some routers and VPNs filter the broadcasts devices use to find each other. The
connection itself would work. So pair another way, once:

- **QR code.** Show the QR code in **Add device** on one device and scan it with the other.
- **Address.** Open **Add device** on the computer, note its address, and type it in **Add
  device** on the phone.

After that, paired devices reconnect by their last known address, without looking for each other.
The iPhone build from the releases always works this way.

## 3. The port is blocked

Symptom: Diagnostics says port 42187 is blocked, or the computer's own check fails.

PlayDrop uses one port, 42187: TCP for transfers and UDP for discovery. It never uses the ports
of KDE Connect (1714 to 1764) or LocalSend (53317).

**Ubuntu, Pop!_OS (ufw).** The window shows the command that fits your setup. For ufw:

```sh
sudo ufw allow 42187
```

**Fedora and others (firewalld):**

```sh
sudo firewall-cmd --permanent --add-port=42187/tcp --add-port=42187/udp && sudo firewall-cmd --reload
```

**Windows.** The installer adds the rule. To check it, in a terminal:

```
netsh advfirewall firewall show rule name=PlayDrop verbose
```

If it is missing, run the installer again. Or, in a terminal started as administrator:

```
netsh advfirewall firewall add rule name=PlayDrop dir=in action=allow protocol=TCP localport=42187
netsh advfirewall firewall add rule name=PlayDrop dir=in action=allow protocol=UDP localport=42187
```

(That second form opens the port for every program. The installer's rule is limited to
`playdropd.exe`, which is better.) Press Refresh on Diagnostics afterwards.

Pop!_OS and Ubuntu ship ufw turned off, so a blocked port there usually means you turned it on.
Diagnostics also says if the daemon is not running at all: see [troubleshooting.md](troubleshooting.md).

## Guest, hotel or campus Wi-Fi

When the network itself is the problem, give the two devices their own network. Three ways, from
easiest to fastest. Whichever you use, then pair by QR code or address.

**QR code.** Works if the devices can reach each other at all. Try it first.

**The phone's hotspot.** Turn on the phone's hotspot and connect the computer to it. PlayDrop finds
the phone within seconds.

- Android: Settings, Network, Hotspot and tethering (on some phones Settings, Connections, Mobile
  Hotspot and Tethering). The Android app has a button that opens this screen.
- iPhone: Settings, Personal Hotspot.

**USB tether.** The fastest fallback. Plug the phone into the computer and turn on USB tethering.
The computer gets a wired network to the phone.

- Android: Settings, Network, Hotspot and tethering, USB tethering.
- iPhone: turn on Personal Hotspot, plug in the phone and tap Trust on it.

Honest status: these three paths are written and wired up in the apps, but they have not been
tried on a real client-isolated network, hotspot or cable yet. Mesh Wi-Fi and VPNs are also
untested by a person. If one of these does not work for you, please open an issue.

## Still stuck

See [troubleshooting.md](troubleshooting.md). KDE Connect and LocalSend can stay installed:
PlayDrop does not touch their ports.
