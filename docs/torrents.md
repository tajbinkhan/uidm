# Torrents and magnet links

UDM downloads torrents as well as web and FTP addresses: a `magnet:` link, or a `.torrent` file.
A torrent is a download in the list like any other. It waits in queues, starts and stops with the
scheduler, counts toward **Downloads that run at once**, follows the speed limit and the download
quota, pauses, resumes, and carries on after UDM starts again. Once it has finished, it shares
(*seeds*) what it downloaded with other people for a while, then stops by itself.

## Adding a torrent

- **Add URL**: paste a magnet link (`magnet:?xt=urn:btih:...`) as you would an address.
- **The clipboard**: with **Monitor the clipboard** on, a copied magnet link is offered like a
  copied address.
- **The browser**: the extension hands UDM the magnet links you click, and the `.torrent` files
  the browser downloads (by their type, `application/x-bittorrent`, or their name). UDM fetches a
  `.torrent` file itself, with the page's cookies, as it does any download the browser hands over.
  Holding the skip key while you click leaves a magnet link to the browser, which opens it with
  whatever program it gives magnet links to. In Safari, which tells extensions nothing about its
  downloads, a link to a `.torrent` file is taken as it is clicked. A torrent file is taken
  whatever its size and the captured file types, unless its site or address is excluded; with
  **Take .torrent files as torrents** off, it is a file like any other.
- **The command line**: `udm-cli get "magnet:?xt=urn:btih:..."`, or `udm-cli get file.torrent`
  ([Command line](command-line.md#get)).
- **AI assistants**: `udm-mcp`'s `add_download` takes magnet links ([AI assistants](ai-assistants.md)).

UDM needs the torrent's *metadata*, the list of its files, before it can save anything. A
`.torrent` file has it; for a magnet link UDM asks the torrent's peers for it, which can take from
a second to a few minutes. The save-as dialog waits for it, for at most three minutes, and then
lists the torrent's files: tick the ones you want, and choose the folder and category as for any
download. With **Ask where to save** off, the torrent downloads every file. A torrent of several
files gets a folder of its own, named after the torrent; a torrent of one file is saved under that
file's name. UDM keeps a copy of each torrent's `.torrent` file, so it never asks for the
metadata twice.

You can change which files a torrent downloads later. A running torrent takes the new choice at
once; a finished one given more files starts again for them; a stopped one takes it when it
starts.

## Downloading

A torrent finds its peers through its trackers, the DHT (the peer network that needs no tracker)
and, on your local network, Local Service Discovery. Its row in the list shows its size, its
progress, its speed and how long it has left, as any download's does. Its download window shows
its files, its peers, its trackers and a map of its pieces.

- **Speed limits.** The speed limit in **Options** applies to torrents together, as it applies to
  the other downloads together. A torrent's own speed limit applies from when it starts. A limit
  under 16 KB/s is taken as 16 KB/s, the least a torrent can be held to.
- **The quota** counts what torrents download, and pauses them with everything else when it is
  reached. What they upload does not count.
- **Pausing** stops a torrent where it is. Starting it again, or starting UDM again, goes on from
  there: UDM keeps which pieces it has (its *resume data*) beside its copy of the `.torrent` file,
  in UDM's data folder, so it does not read all the data again.
- Files you did not choose may still get a few bytes: a piece at the edge of a chosen file can
  overlap the next file, and the torrent writes the whole piece.

## Seeding

A torrent that has finished seeds: other people download its pieces from you. Seeding takes no
place among the downloads that run at once. It stops by itself at the first of two limits:

- **Share ratio**: what it uploaded, divided by what it downloaded. The default is **1.0**: it gives
  back as much as it took.
- **Seeding time**: the default is **60 minutes**, so a torrent nobody asks for does not keep the
  computer uploading. It counts from when the torrent finished.

Either limit can be turned off; with both off, a torrent seeds until you stop it or remove it. A
ratio of 0, or 0 minutes, means no seeding at all. A torrent can have limits of its own, in place
of these. When UDM starts again, the torrents that were seeding check their files and go on
seeding; one whose files are gone or changed stops instead, and is never downloaded again on its
own. Removing a torrent from the list stops its seeding and forgets its resume data.

## Settings

In `settings.json`, the `torrents` section (**Options > Torrents** in the window):

| Setting | What it does | Default |
|---|---|---|
| `port` | The port peers connect to, for TCP and the DHT's UDP. Missing (or 0) takes a free one each time UDM starts. | free port |
| `dht` | Find peers through the DHT. | on |
| `upnp` | Ask the router to forward the port to this computer (UPnP). | on |
| `local_peers` | Find peers on the local network (Local Service Discovery). | on |
| `max_peers` | The most peers each torrent connects to, 1 to 1000. | librqbit's (128) |
| `upload_limit_kbps` | The most all torrents upload together, in KB/s. | no limit |
| `seed_limits` | `ratio` and `minutes`, as above; leave one out to turn it off. | 1.0 and 60 |
| `catch_torrent_files` | Take `.torrent` files the browser downloads as torrents. | on |

## Security and privacy

- **Your address is public to the swarm.** Everyone downloading the same torrent, and the
  trackers, see your IP address and what you download and share. The DHT announces it too. To keep
  it hidden, use a VPN and bind UDM to its adapter (below).
- **No encryption.** UDM's BitTorrent connections are not encrypted (the protocol's MSE/PE
  obfuscation is not offered), so your network and internet provider can see that you use
  BitTorrent and what you exchange.
- **An open port.** To seed and to be reached by peers, UDM listens on its port on every network
  interface (`[::]` or `0.0.0.0`), and with UPnP on asks the router to forward that port from the
  internet. Turn UPnP off to keep the router closed; peers can still be reached from your side.
  With a network adapter chosen, it listens on that adapter only.
- **Torrents do not use the proxy settings.** A proxy in **Options** applies to web and FTP
  downloads; torrent connections go straight out (or through the chosen adapter). Use a VPN rather
  than a proxy to hide them.
- **Untrusted file names.** A torrent's file names come from whoever made it. UDM refuses a
  torrent with a name it could not save safely inside its folder: `..`, a path separator, NUL, a
  character Windows does not allow (`< > : " | ? *`), a device name such as `CON`, a name ending in
  a dot or a space, or two names that differ only in case. It also refuses torrents of more than
  10,000 files and `.torrent` files (or metadata from peers) larger than 16 MB.
- **What it never does**: run anything from a torrent, open a web interface or API, or listen on
  any port but the one above.

### With a network adapter or encrypted DNS

With an adapter chosen in **Options > Network** ([Network adapter](network.md)), every socket the
torrents use is bound to it: the listener, the connections to peers, the DHT's and the UDP
trackers' sockets, and the HTTP trackers' connections. UPnP and Local Service Discovery are off
while an adapter is chosen. When the adapter goes down, running torrents stop with the other
downloads, and seeding stops; nothing goes out another way. Changing the adapter, the name lookups
or the torrent settings moves running and seeding torrents to the new setting at once.

- **On Windows** librqbit cannot bind its sockets to an adapter, so torrents do not run while one is
  chosen: they stop with "Torrents cannot be bound to a network adapter on Windows". Choose **Any
  network** to download torrents there.
- **With DNS over HTTPS**, no tracker or DHT name goes to the system's resolver: the DHT's starting
  nodes and the UDP trackers are looked up through UDM's resolver and handed to librqbit as
  addresses, and HTTP trackers named by a host name are left out (the download window shows them as
  skipped). A torrent whose only trackers are HTTP ones then finds its peers through the DHT, the
  magnet link's peers and peer exchange.

## What is not there

Sequential downloading (playing a video while it downloads), RSS feeds, IP blocklists, web seeds
(the `ws=` part of a magnet link), protocol encryption and torrents that are BitTorrent v2 only
(magnet links with `xt=urn:btmh:` and no v1 hash) are not supported. Incoming connections are TCP
only. Trackers are shown as used or skipped, not with what they last answered.

UDM's BitTorrent client is [librqbit](https://github.com/ikatson/rqbit) (Apache-2.0). UDM carries
one small fix of its own to it, for systems without IPv6 (`vendor/librqbit-tracker-comms`).

---

[← Back to the help pages](README.md)
