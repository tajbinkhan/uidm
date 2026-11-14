# Debrid accounts (experimental)

File hosts such as Rapidgator, 1fichier or Nitroflare download slowly, or not at all, without a
premium account of their own. A **debrid service** has premium accounts on many of them and
turns a link to one of those hosts into a fast direct link for its paying users. UDM can send
such links through your account with one of these services:

- [Real-Debrid](https://real-debrid.com/)
- [AllDebrid](https://alldebrid.com/)
- [Premiumize](https://www.premiumize.me/)

This is **experimental**: it is new, has been tried against stand-ins for each service's API
rather than through months of real use, and the services change their APIs from time to time.
If something goes wrong, turn it off and the links download as before.

## Turning it on

1. Get your account's API key from the service:
   - Real-Debrid: <https://real-debrid.com/apitoken>
   - AllDebrid: <https://alldebrid.com/apikeys/> (create a key there)
   - Premiumize: <https://www.premiumize.me/account> (the **API Key** on the account page)
2. Open **Options > Debrid**.
3. Tick **Download file hosts' links through a debrid account**, choose the service and paste
   the key into **API key**.
4. Choose **Check account**. UDM asks the service about the account and says whether the key
   works, the account's name and until when it is premium.
5. Choose **OK**.

The key field starts empty each time Options opens; **A key is stored** says one is. Typing a
new key and choosing OK replaces it, and **Remove the stored key when saving** removes it.

A key is stored for the service it was saved with, and UDM never sends it to another one. If you
choose another service, the page says whose key is stored; type that service's key before you
check the account or save. Until then, links download as they would without an account.

## What happens to a download

When a download starts, UDM looks at its address. If the account is on and the service supports
the address's host (UDM reads the service's list of hosts and reads it again every six hours),
UDM asks the service to unlock the link and downloads the direct link it gets back. The list and
the save-as dialog keep showing the link you added; the file's name and size come from the
direct link.

Direct links last a few hours. UDM asks for a fresh one each time the download starts, so a
download that is resumed the next day, retried, or given a **New link** goes on without you
doing anything. Asking twice for the same link within ten minutes (the save-as dialog's look at
the file, then the download) asks the service once.

A fresh direct link can come from another of the service's servers. UDM resumes the part
already downloaded only when the server says it has the same file (same size, and the same
ETag or date when it sends them); when the new server describes the file differently, the
download starts again from the beginning. A queue that keeps files in sync compares the
unlocked file too, not the file host's page.

- A link to a host the service does not support, or a link the service says it does not take,
  downloads as it always did.
- If the service refuses the link (the key is wrong, the account is not premium, the file is
  gone from the host, a limit was reached), the download fails and its error says why, starting
  with the service's name. It does not fall back to the slow download from the host.
- If the service cannot be reached for its list of hosts (it is down), links download as they
  would without the account, and UDM tries the list again five minutes later.
- If the service refuses the key or the account when UDM reads the list (AllDebrid and
  Premiumize need the key for it, for example after the key was revoked or the account stopped
  being premium), downloads go on as they would without the account, and the main window's
  banner (and the Debrid page) says the service refused the account, once an hour until you fix
  the key or turn the account off. A link to a file host then downloads from the host itself,
  which may give a web page instead of the file. When UDM still has an earlier list from the
  same service, only the links to its hosts fail, each with the service's reason.

Streams, video pages (yt-dlp) and form posts never go through the account. Neither do `ftp://`
links.

## Privacy

While the account is on, the **address of every download on a host the service supports** is
sent to that service, which is how unlocking works. That happens as soon as UDM looks at the
file: the save-as dialog already shows the unlocked file's name and size, so the link has been
sent to the service even if you then cancel the dialog. The link picker's list of a page's links
is not unlocked, so those links are not sent. The service sees which files you download
from those hosts. Addresses on other sites are never sent. The cookies, referrer and other
browser headers a download carries for the file host are not sent to the service or to the
direct link's server; only UDM's user agent is.

The key is stored sealed beside UDM's history (`debrid.bin` in UDM's data folder), together with
the service it is for: with DPAPI on Windows and with UDM's own key on macOS and Linux, like the
proxies' passwords. It is never
written to `settings.json`, never sent to the window (which only learns that a key is stored),
and never written to UDM's logs or problem reports. It goes only to the service you chose: in an
`Authorization` header to Real-Debrid and AllDebrid, and as the `apikey` field Premiumize's API
asks for.

Requests to the services go through the proxy set in **Options > Proxy / Socks**, like UDM's
other requests.

## For developers

The rule deciding which links go to the service is `udm_domain::debrid::wants_unrestrict`; the
use cases are `udm_application::debrid` (`ResolveLink`, `CheckAccount`, `SetKey`), and the
adapters `udm_infrastructure::debrid` (`DebridApi` for the three APIs, `KeyFile` for the key).
The engine asks the resolver (`ResolveLink`, typed `engine::Resolver`) on the download's own
thread just before it connects (`DownloadRequest::resolve`). The tests run against a local stand-in for each API and never
reach the real services.
