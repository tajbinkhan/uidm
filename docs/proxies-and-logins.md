# Proxies and logins

The settings `network_adapter` (`{"id": "wg0", "name": "wg0"}`) and `dns`
(`"system"`, `"cloudflare"`, `"google"`, `"quad9"` or `{"custom": "https://..."}`)
bind every connection, the proxy's included, to one adapter and choose the
resolver; while an adapter is chosen, no configuration script is used. See
[Network adapter, encrypted DNS and the server's country](network.md).

The settings hold `proxy_mode` (`none`, `system`, `script` or `manual`), the
manual `proxy_http`, `proxy_https`, `proxy_ftp` and `proxy_socks5` (each
`host:port`), each one's user name (`proxy_username` for HTTP, then
`proxy_https_username`, `proxy_ftp_username` and `proxy_socks_username`),
`use_browser_proxy`, `browser_proxy_after_errors` and `site_logins` (`site`
and `username`). Passwords are write-only: `proxy_password`,
`proxy_https_password`, `proxy_ftp_password`, `proxy_socks_password` and each
login's `password` travel in `set_settings` only when changed, and the daemon
keeps them outside the settings file and never sends them back. Proxy
passwords are encrypted with DPAPI in `%LOCALAPPDATA%\UDM`: `proxy.bin` for
HTTP, `proxy-https.bin`, `proxy-ftp.bin` and `proxy-socks.bin`. A settings
file from before each proxy had a login of its own gives its shared login to
the HTTPS and SOCKS proxies, which used it, on the daemon's first start. Login
passwords are generic credentials named `UDM/login/<site>` in Windows
Credential Manager, removed with their login.

**Proxies.** The system proxy is read from the Internet settings in the
registry each time a download starts: `ProxyServer` (one proxy, or
`http=…;https=…;ftp=…;socks=…`) and the `ProxyOverride` bypass list, including
`<local>`, and its configuration script or automatic detection (WPAD), which
WinHTTP runs to pick the proxy per address. **Get System** in Options copies
the fixed part of that setting (the proxies and the bypass list, not the
script) into the manual fields, the FTP proxy only when Windows names one
with `ftp=`; Windows' SOCKS proxy speaks SOCKS4. The
manual HTTPS proxy is the proxy for `https://` URLs (an HTTP proxy reached
with `CONNECT`; empty means the HTTP one, with its login). A SOCKS4 or
SOCKS5 proxy carries the `http://` and `https://` URLs that have no proxy of
their own, and with `proxy_socks_https` every `https://` URL even when an
HTTP or HTTPS proxy is set. `ftp://` downloads go through the FTP proxy:
UDM logs in to it first with its own login, when it has one (`USER`,
`PASS`), then as `user@host`. Without an FTP proxy they go through the SOCKS
proxy (`proxy_socks_ftp`, on by default): UDM opens both the control and
the data connections with a SOCKS CONNECT itself (SOCKS4, SOCKS4a or SOCKS5,
with the SOCKS login), and always in passive mode, since the server cannot
connect back through the proxy. `ftps://` downloads skip the FTP proxy,
which would be the TLS end instead of the server, and take the SOCKS proxy
as `ftp://` ones do without an FTP proxy; an `ftp://` server reached through
an FTP proxy is not asked again with `AUTH TLS` when it wants TLS.
A proxy's login goes to the proxy only (`Proxy-Authorization`, or the
SOCKS handshake). UDM follows redirects itself, dropping `Cookie` and
`Authorization` on every hop, so a site's cookies and logins never reach
another server. With
`use_browser_proxy`, the extension reports the browser's whole proxy
setting (`chrome.proxy`, Firefox's `browser.proxy`) with each download, and
it wins for downloads that carry one: no proxy, its fixed HTTP and SOCKS
proxies (an HTTPS or QUIC proxy, which the engine cannot speak, is left
out), its configuration script (WinHTTP fetches one at an address; one the
browser was given as text is served to WinHTTP from a loopback-only address
of the daemon's), automatic detection (WPAD), or the system proxy, as
**Use system proxy settings** reads it. The bypass list goes along:
Chrome's `<local>`, `*.corp.net` and `host:port` entries and Firefox's
`.corp.net`; address ranges have no counterpart and are dropped. Without
WinHTTP (macOS, Linux) a script or detection cannot run, so those downloads
connect directly. With `browser_proxy_after_errors` too, a
download goes through UDM's own proxy setting first; when it fails, the
daemon retries it once through the browser's proxy, and that address keeps
using it until the daemon restarts.

**Logins.** A site matches the way capture rules do: `example.com` covers its
subdomains, and `*` works within a name. A path after the site limits the
login to it: `example.com/private/*` matches with `*` across the whole path,
and `example.com/private` covers that folder and everything below it, both
ignoring case. The longest matching pattern wins, so a folder's login beats
its server's. A download's own login (from **Log in to the site** or its
properties) replaces the site logins for its host. A request goes out
without credentials. On a 401, UDM picks the login for the address that
answered and sends the request again, once, straight to that URL,
with `Authorization` for the `WWW-Authenticate` challenge: Digest (MD5,
MD5-sess, SHA-256, with `qop=auth`) when offered, else NTLM, else Basic.
NTLM (offered as `NTLM`, or as `Negotiate`, under which Windows servers take
NTLM messages too) is a handshake on one connection: a negotiate message,
the server's challenge in its 401, then an NTLMv2 answer, which proves the
password without sending it, so plain http is fine. A user name written
`DOMAIN\user` gives the domain. Each segment's connection does its own
handshake. Signing in as the current Windows user without a saved login is
not supported. Basic over
plain http, and any FTP login over a plain connection, needs a login allowed on plain connections
(a download's own login always is); otherwise the download asks for one,
so a server offering only Basic cannot draw out a password saved for
https. `Authorization` is never carried across a redirect, so a login
never reaches another host. No login for the host fails with "This site needs a login: <host>",
and a refused login with "The login for <host> was refused"; neither is
retried.

---

[← Back to the help pages](README.md)
