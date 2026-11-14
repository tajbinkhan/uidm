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
  ffmpeg and gallery-dl, the site grabber, and debrid accounts.
- **The kill switch.** Before every connection, UDM looks the adapter up again. When it is
  missing, not running or has no address, the connection is not made, and the download stops with
  "The network adapter … is down". Nothing falls back to another network. A download already
  receiving data is watched too: UDM looks at the adapter every two seconds and stops the download
  as soon as the adapter goes, and the main window says why.
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
| Linux | `SO_BINDTODEVICE` on the interface | Packets can only leave through that interface. Unprivileged on Linux 5.7 and later. |
| macOS | `IP_BOUND_IF` / `IPV6_BOUND_IF` on the interface | Same guarantee. |
| Windows | The socket is bound to the adapter's address | Windows sends from an address only through the adapter that owns it. UDM binds the adapter's IPv4 address when it has one, so a server with only an IPv6 address cannot be reached through an adapter that also has IPv4. |

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
- The browser extension and the browser make their own connections.

## Encrypted DNS (DNS over HTTPS)

Under **Look up host names with**:

- **The system's (not encrypted)**: as every other program does. With an adapter chosen, the
  system may send these lookups through another adapter; Options says so.
- **Cloudflare (1.1.1.1)**, **Google (8.8.8.8)** or **Quad9 (9.9.9.9)**: DNS over HTTPS
  ([RFC 8484](https://www.rfc-editor.org/rfc/rfc8484)) to `cloudflare-dns.com`, `dns.google` or
  `dns.quad9.net`. UDM knows their addresses, so finding the resolver needs no lookup of its own.
- **Another DNS over HTTPS server**: any resolver's `https://` address, such as
  `https://dns.example/dns-query`. Its host name, unless it is an address, is looked up once by the
  system, when UDM first asks it something.

The queries go over the same TLS as downloads, bound to the chosen adapter, and through no proxy.
UDM asks for both IPv4 and IPv6 addresses, follows CNAME records, and keeps each answer for as long
as the resolver says, between half a minute and an hour. It **fails closed**: when the resolver
cannot be reached or does not know a name, the download fails; the name is never asked of the
system instead. `localhost` is always this computer and is never asked.

With a proxy, the proxy looks up the names of the servers it connects to (HTTP proxies and SOCKS5
always, SOCKS4a too); UDM looks up only the proxy's own name, and with SOCKS4 the server's.

## The server's country

The list's **Country** column (hidden at first: **More > Columns...** shows it) and
the download window show the country of the server a download connected to, with its flag, such as
"🇺🇸 US". It is looked up in a table of address ranges built into UDM, so no address is ever sent
anywhere to find it. Private and local addresses have no country, and neither does a download
through a proxy, since the address UDM sees is the proxy's. Streams and video pages show none.

The table comes from the npm package
[`@ip-location-db/geo-whois-asn-country`](https://www.npmjs.com/package/@ip-location-db/geo-whois-asn-country)
(published under CC0-1.0; its WHOIS data is the Number Resource Organization's, under
[CC BY 4.0](https://creativecommons.org/licenses/by/4.0/), [nro.net](https://www.nro.net/)). It
is `crates/infrastructure/data/ip-country.bin`, about 640 KB, whose
[README](../crates/infrastructure/data/README.md) names the version. To refresh it:

```bash
npm pack @ip-location-db/geo-whois-asn-country
tar -xzf ip-location-db-geo-whois-asn-country-*.tgz
python3 tools/ip-country/make_table.py package crates/infrastructure/data/ip-country.bin
```

then put the package's version in the README. Country lookups are a best guess: an address block
registered in one country can be used in another.
