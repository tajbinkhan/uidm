# Network adapter, encrypted DNS and the server's country

People who download through a VPN usually want a guarantee: nothing leaves the computer outside
the VPN, not even when the VPN drops for a moment. UDM can bind every download to one network
adapter and stop downloading when that adapter goes down, look host names up with DNS over HTTPS,
and show the country of each download's server.

All three are on **Options > Network**, next to **Proxy / Socks**.

## Downloading through one adapter

Choose the adapter under **Download through**: a VPN's adapter (`wg0`, `tun0`, `utun3`,
"WireGuard Tunnel", "TAP-Windows Adapter" and the like), or any other the computer has. The list
shows each adapter's addresses, read when Options opens; one that is not up says **(not
connected)**. **Any network (the system chooses)** is how UDM worked before: the system picks.

With an adapter chosen:

- **Every connection is bound to it**: downloads, their probes and save-as dialog checks, stream
  segments, FTP control connections and both passive and active FTP data connections, the
  connection to a proxy, the DNS over HTTPS queries below, the update check, installing yt-dlp,
  ffmpeg and gallery-dl, the pages the site grabber fetches itself, and debrid accounts.
- **The kill switch.** Before every connection, UDM looks the adapter up again (the system's list,
  read at most once a second). When it is missing, not running, has no address or is another
  connection that took its name (below), the connection is not made, and the download stops with
  "The network adapter … is down". Nothing falls back to another network. A download already
  receiving data is watched too: one watcher looks at the adapter of every running download each
  second, and stops them within two seconds of the adapter going; the main window says why.
- **Changing the setting** (the adapter, or how names are looked up) reaches every download at
  once: those waiting take the new setting when they start, and running ones start again from where
  they are, so none goes on the old way.
- **When the adapter comes back**, stopped downloads stay stopped. Start them again (they resume
  where they were); a running queue puts them back to waiting while they have retries left, as it
  does with any failed download, and they start once the adapter is up. Otherwise UDM does not
  restart them on its own, so a VPN that flaps does not send a burst of connections.
- **No configuration script.** A proxy configuration script (PAC, including Windows' automatic
  detection) is fetched and run by the system, which UDM cannot bind, so it is not used while an
  adapter is chosen. Proxies set by hand still apply, and the connection to them is bound too.

How the binding is made, on each system:

| System | How | Notes |
| --- | --- | --- |
| Linux | `SO_BINDTODEVICE` on the interface | Packets can only leave through that interface. Unprivileged on Linux 5.7 and later; before that, only administrators may, and downloads stop with "The system would not bind connections to the network adapter …" instead of going out unbound. |
| macOS | `IP_BOUND_IF` / `IPV6_BOUND_IF` on the interface | Same guarantee. |
| Windows | The socket is bound to the adapter's address | See below. UDM binds the adapter's IPv4 address when it has one, so a server with only an IPv6 address cannot be reached through an adapter that also has IPv4. When the adapter gets a new address (a VPN reconnecting), running downloads start again from where they are, bound to it. |

**Windows and "weak host" sending.** Binding to an address keeps the packets on that adapter
because Windows' default *strong host* model sends a packet only through the adapter that owns its
source address. Windows lets each interface be switched to the *weak host* model for sending
(`weakhostsend`), and then a packet from the VPN's address may leave through another adapter if the
routing table says so. An administrator can turn it on, and so can some VPN, firewall or
virtualization software. UDM cannot prevent that. To check, in PowerShell:

```powershell
Get-NetIPInterface | Where-Object WeakHostSend -eq Enabled | Select-Object InterfaceAlias, AddressFamily
```

Nothing listed means every interface sends the strong way. An interface listed there (the VPN's or
another one) weakens the guarantee on this computer.

**Adapters named by number.** macOS gives tunnels the names `utun0`, `utun1` and so on in turn, and
Linux `tun0` or `ppp0`, so after a VPN reconnects, its old name may belong to another tunnel (iCloud
Private Relay and other VPNs use `utun` names too). For such a name UDM remembers the networks the
adapter was on when you chose it, and an adapter of that name on none of them counts as down: the
downloads stop rather than go through the other tunnel. A VPN that comes back on the same network
(as WireGuard and most VPNs do) is recognized again. If yours comes back on another network, choose
**Any network**, choose **OK**, then choose its adapter again. Names you give adapters yourself
(`wg0`) and Windows' adapters, which keep their id, are not checked this way.

### Programs UDM runs

yt-dlp (video pages) and gallery-dl (picture galleries) make their own connections. While an
adapter is chosen, UDM gives them the adapter's address with their own `--source-address` option,
and does not start them at all while the adapter is down; a running yt-dlp download is stopped
with the others when it goes. This is weaker than UDM's own binding:

- They bind to the adapter's address, not to the adapter. On Windows that is the same thing; on
  Linux and macOS the system's routing table decides which interface such a packet leaves by,
  although a packet from the VPN's address sent elsewhere cannot get an answer back.
- They look host names up with the system's resolver, even when DNS over HTTPS is chosen.
- When yt-dlp hands a download to ffmpeg (some live and HLS formats), ffmpeg's connections are not
  bound.

ffmpeg itself, when UDM runs it, only joins and converts files that are already downloaded.

### What is not bound

- The remote-control page for phones listens for connections; it is not a download, and its
  addresses are chosen on **Options > Remote**.
- UDM's own programs talk to each other on `127.0.0.1`.
- The browser extension and the browser make their own connections. That includes the pages the
  site grabber asks the browser to open and render (pages that need JavaScript).
- A custom DNS over HTTPS server's own name (below) is looked up once by the system.
- On Windows and macOS, the system's TLS may fetch a missing intermediate certificate or check
  whether a certificate was revoked (AIA, CRL and OCSP) through its own services, outside UDM's
  binding. Linux's (OpenSSL) fetches nothing.

## Encrypted DNS (DNS over HTTPS)

Under **Look up host names with**:

- **The system's (not encrypted)**: as every other program does. With an adapter chosen, the
  system may send these lookups through another adapter; Options says so.
- **Cloudflare (1.1.1.1)**, **Google (8.8.8.8)** or **Quad9 (9.9.9.9)**: DNS over HTTPS
  ([RFC 8484](https://www.rfc-editor.org/rfc/rfc8484)) to `cloudflare-dns.com`, `dns.google` or
  `dns.quad9.net`. UDM knows their addresses, so finding the resolver needs no lookup of its own.
- **Another DNS over HTTPS server**: any resolver's `https://` address, such as
  `https://dns.example/dns-query`. Its host name, unless it is an address, is looked up once by the
  system, when UDM first asks it something: that one lookup goes wherever the system sends it,
  unbound and not encrypted, though only once the chosen adapter is up. Give the server by its
  address (`https://192.0.2.53/dns-query`, if its certificate names the address) to avoid it.

The queries go over the same TLS as downloads, bound to the chosen adapter, and through no proxy.
UDM asks for IPv4 and IPv6 addresses at once, pads each query to a multiple of 128 bytes (EDNS(0)
padding, [RFC 8467](https://www.rfc-editor.org/rfc/rfc8467)) so its length says less about the
name, follows CNAME records, and keeps each answer for as long as the resolver says, between half a
minute and an hour. It **fails closed**: when the resolver cannot be reached or does not know a
name, the download fails; the name is never asked of the system instead. `localhost` is always
this computer and is never asked.

With a proxy, the proxy looks up the names of the servers it connects to: HTTP proxies and SOCKS5
always (UDM hands every SOCKS5 proxy on as `socks5h`, which says so, to yt-dlp and gallery-dl too),
and SOCKS4a. UDM looks up only the proxy's own name, and with SOCKS4 the server's.

## The server's country

The list's **Country** column (hidden at first: **More > Columns...** shows it) and
the download window show the country of the server a download connected to, with its flag on macOS
and Linux, such as "🇺🇸 US" (Windows draws no flags, so there it is "US"). It is looked up in a
table of address ranges built into UDM, so no address is ever sent anywhere to find it. Private and
local addresses have no country, and neither does a download through a proxy, since the address UDM
sees is the proxy's. Streams and video pages show none.

The table contains data from the Number Resource Organization ([nro.net](https://www.nro.net/)),
licensed under [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/), through the
[ip-location-db](https://github.com/sapics/ip-location-db) project's `geo-whois-asn-country`
database, converted into a table of address ranges. UDM ships that notice in
`THIRD-PARTY-NOTICES.txt` beside its programs and shows it in **About UDM**. The table is
`crates/infrastructure/data/ip-country.bin`, about 640 KB; its
[README](../crates/infrastructure/data/README.md) names the data's version and how to make it
again, which [Building UDM](building.md#making-a-release) asks for before each release. Country
lookups are a best guess: an address block registered in one country can be used in another.
