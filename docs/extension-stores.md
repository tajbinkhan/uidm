# Publishing the extension in the browser stores

For maintainers. This is how UDM's browser extension gets listed in the
Chrome Web Store and on Firefox Add-ons (addons.mozilla.org, AMO), and what
changes in the repository once it is. Until then users load it by hand
([Install](install.md)), and in Firefox only until Firefox closes (#248).

## What the repository already does

- **The zips.** `node installer/make-store-zips.js [folder]` writes
  `UDM-<version>-chromium-store.zip` and `UDM-<version>-firefox-store.zip`
  (to `dist` by default), with the version from the extension's manifest.
  The Chromium zip is `extension/chromium` without the manifest's `key`:
  the Chrome Web Store refuses a manifest with one and gives the listed copy
  an ID of its own. The key stays in git, so a copy loaded by hand keeps its
  fixed ID `epmjakacaejhdhldkpbjnakhfmjjbhfm`. The Firefox zip is
  `extension/firefox` as `installer/make-firefox.js` makes it, with the ID
  `udm@udm.local`, which AMO keeps; the script refuses to make it from a
  copy older than the Chromium extension. Both hold the files at their root
  in a fixed order and time, so the same extension makes the same bytes
  with the same Node and zlib version (`extension/test/store.test.js`).
- **The release.** The Release workflow's `extension` job checks that the
  tag matches the version in `extension/chromium/manifest.json` (a store
  takes each version once), runs the extension's tests, makes both zips and
  attaches them to the release with the installers, in its signed
  `SHA256SUMS`.
- **Publishing on a tag**, when the repository has the stores' keys (below):
  the `firefox-store` job submits the Firefox zip to AMO with
  [web-ext](https://github.com/mozilla/web-ext) (`web-ext sign --channel
  listed`), and the `chrome-store` job uploads the Chromium zip through the
  Chrome Web Store API v2 and submits it for review. The two jobs are
  separate, so one failing does not stop the other. A store whose keys are
  missing is skipped; upload its zip from the release page by hand instead.
- **The privacy policy**, which both stores ask for: [privacy.md](privacy.md),
  at <https://github.com/tajbinkhan/UDM/blob/main/docs/privacy.md>.
- **The host lets in a list of IDs.** `CHROMIUM_EXTENSION_IDS` in
  `crates/infrastructure/src/browsers.rs` is every Chromium extension ID the
  native host `com.udm.host` lets in, the unpacked one first. A test there,
  `every_file_lets_in_the_same_extensions`, checks that `host/com.udm.host.json`,
  `installer/windows/udm.iss` (`ChromiumIds`) and `install-host.ps1`
  (`$ExtensionId`) list the same IDs, and that every file names the same
  Firefox ID. On macOS, Linux and in a portable copy, UDM writes the
  browsers' host manifests again when they lack an ID it lets in (one that
  lets in more is kept), so an update brings a new ID to copies registered
  before. On Windows, Setup writes its own manifests on every install, and
  also the ones in UDM's data folder that Options wrote for Brave or
  Chromium when they name the installed host.

The extension has no build step: no bundler, no minifier, nothing
transpiled. The zips are its source as it is in git.

## The Chrome Web Store

1. Sign in to the
   [Chrome Web Store developer dashboard](https://chrome.google.com/webstore/devconsole)
   with the Google account that will own the listing, and register as a
   developer (a one-time fee). Turn on two-step verification, which the
   store requires to publish.
2. **New item**, and upload `UDM-<version>-chromium-store.zip` from the
   latest release.
3. **Store listing**: the description (start from the README's), the
   category (*Productivity* or *Tools*), the language, at least one
   1280×800 screenshot and the 440×280 small promo tile. The 128-pixel icon
   comes from the zip. The homepage is <https://github.com/tajbinkhan/UDM>
   and support is <https://github.com/tajbinkhan/UDM/issues>.
4. **Privacy**:
   - Single purpose: *Hands the downloads, videos and links the user
     chooses, or that UDM's capture rules take, to the UDM download manager
     on the same computer.*
   - Permission justifications: the table below.
   - Remote code: **No, I am not using remote code.**
   - Data usage: nothing leaves the computer, so the extension collects
     none of the listed kinds of data. Should the review count handing data
     to the program on the same computer, tick *Authentication information*
     (cookies), *Web history* and *Website content*, and say that they go
     only to UDM on the same computer. Tick the three certifications (no
     selling or transfer, no use unrelated to the single purpose, no
     creditworthiness).
   - Privacy policy URL:
     <https://github.com/tajbinkhan/UDM/blob/main/docs/privacy.md>.
5. **Distribution**: public, all regions. Submit for review. Access to all
   sites and `webRequest` mean an in-depth review, which can take some days.
6. Once it is accepted, the dashboard shows the item's ID (32 letters from
   a to p) and its page,
   `https://chromewebstore.google.com/detail/<id>`. Then do
   [After the listing](#after-the-listing).

Microsoft Edge installs from the Chrome Web Store once the user allows
extensions from other stores. Listing in Edge Add-ons as well takes the same
zip and gives yet another ID, which goes in the same list.

### Permission justifications for the form

| Permission | Justification |
| --- | --- |
| `nativeMessaging` | Talks to the UDM download manager installed on the same computer through its native-messaging host, com.udm.host. Everything the extension reads goes there and nowhere else. |
| `downloads` | Sees the downloads the browser starts, cancels the browser's copy of one UDM takes over, hands a download back to the browser when UDM cannot take it, and lets the popup send UDM a download the browser already started. |
| `downloads.ui` | Hides the browser's download bubble while UDM takes a download over, so the browser does not show a download it is not doing. |
| `cookies` | Reads the cookies for a download's address, to send them with the download to UDM, so the server gives UDM the same file it gives the browser. Never sets or changes a cookie. |
| `contextMenus` | Adds Download with UDM, Download later with UDM, the links, gallery and video items to the right-click menu. |
| `storage` | Keeps the extension's few settings: whether capture is on, the skip and force keys, and the tabs where capture is off. |
| `webRequest` | Reads the requests of pages and video players to rebuild a form's POST download, to send UDM the browser's whole request for a download, and to notice the videos and streams a page loads for the video button. Changes no request. |
| `webNavigation` | Finds the frame a link in an about:blank frame belongs to, for the right referrer, and notices when a video site moves to another video without loading a page. |
| `proxy` | Reads the browser's proxy setting for UDM's option to download through the browser's proxy. Never changes it. |
| `scripting` | Runs the extension's own scripts in a page when the user asks for its links or selection, adds the video button to tabs already open when the extension starts, and reads a page's HTML for UDM's site grabber. |
| `notifications` | Says when UDM could not be reached or could not take a download over. |
| Host permission `<all_urls>` | Downloads, videos and links come from any site, and webRequest, cookies and scripting only work on sites the extension may access. The content script on every page shows the video button over playing videos. |

### Keys for the Release workflow

So that each release's tag uploads and publishes the new version:

1. In the [Google Cloud console](https://console.cloud.google.com/), make a
   project and turn on the **Chrome Web Store API**.
2. Set up the OAuth consent screen (external), then set its publishing
   status to **In production**. While it is in *Testing*, Google lets its
   refresh tokens expire after seven days, and the release's upload would
   start failing a week later. An unverified app in production only shows
   you a warning when you sign in to make the token. Then make an OAuth
   client ID of type *Desktop app*.
3. Get a refresh token for the scope
   `https://www.googleapis.com/auth/chromewebstore` with that client, as
   Google's [Chrome Web Store API guide](https://developer.chrome.com/docs/webstore/using-api)
   shows.
4. Add the repository secrets (**Settings > Secrets and variables >
   Actions**): `CWS_CLIENT_ID`, `CWS_CLIENT_SECRET`, `CWS_REFRESH_TOKEN`,
   `CWS_PUBLISHER_ID` (the publisher ID the developer dashboard shows under
   the account's settings) and `CWS_EXTENSION_ID` (the item's ID).

The workflow uses the API v2: `POST
https://chromewebstore.googleapis.com/upload/v2/publishers/<publisher>/items/<item>:upload`,
then `:fetchStatus` while the upload is still being processed, then
`POST https://chromewebstore.googleapis.com/v2/publishers/<publisher>/items/<item>:publish`.
Google is retiring the older `v1.1` calls. The API v2 also takes a service
account in place of the OAuth client: add the service account's address in
the developer dashboard's account settings, and have the job get its token
from the service account's key instead of the refresh token. That needs a
change to the `chrome-store` job.

## Firefox Add-ons

1. Sign in to the [Add-on Developer Hub](https://addons.mozilla.org/developers/)
   with the Mozilla account that will own the listing.
2. **Submit a New Add-on**, choose **On this site** (listed), and upload
   `UDM-<version>-firefox-store.zip` from the latest release. The automatic
   validation runs on it.
3. **Do you need to submit source code?** No: the add-on's code is not
   minified, bundled or transpiled, and the zip is its source. (Its files
   are `extension/chromium`'s, with the manifest `installer/make-firefox.js`
   makes from `extension/firefox.json`.)
4. Listing details: the name, a summary and description (start from the
   README's), the categories (*Download Management*), the license (MIT, as
   the repository's), the homepage and the support site as above, and the
   privacy policy: paste the text of [privacy.md](privacy.md), since AMO
   takes the policy itself, not a link. The manifest already says the
   add-on collects no data (`data_collection_permissions`).
5. Submit. The version is signed at once and listed after Mozilla's review.
   Its page is `https://addons.mozilla.org/firefox/addon/<slug>/`, with the
   slug chosen in step 4. Then do [After the listing](#after-the-listing).

The ID stays `udm@udm.local`, so the host manifests need no change for
Firefox.

### Keys for the Release workflow

In the Developer Hub, **Tools > Manage API Keys**, generate a key, and add
the repository secrets `AMO_JWT_ISSUER` (the JWT issuer) and
`AMO_JWT_SECRET` (the JWT secret). Each release's tag then submits the new
version with `web-ext sign --channel listed`, which does not wait for the
review.

## After the listing

1. **Add the Chrome Web Store's ID** where the host lets extensions in, in
   one commit:
   - `CHROMIUM_EXTENSION_IDS` in `crates/infrastructure/src/browsers.rs`,
     after the unpacked ID;
   - `allowed_origins` in `host/com.udm.host.json`, as
     `chrome-extension://<id>/`;
   - `ChromiumIds` in `installer/windows/udm.iss`:
     `#define ChromiumIds ExtensionId + " <id>"`;
   - the default of `$ExtensionId` in `install-host.ps1`:
     `@("epmjakacaejhdhldkpbjnakhfmjjbhfm", "<id>")`.

   `cargo test -p udm-infrastructure every_file_lets_in_the_same_extensions`
   fails until all four agree. Release UDM with the new ID before pointing
   users at the store: an older UDM's host refuses the store's copy.
2. **Point users at the store pages** instead of loading the extension by
   hand:
   - `ChromeStorePage` and `FirefoxStorePage` in `installer/windows/udm.iss`
     (Setup's last page switches to them by itself), and its `[Run]` entry
     that opens the extension folder;
   - the extension steps in [install.md](install.md) (Windows, macOS, Linux
     and Firefox, and Firefox's known gaps);
   - the closing lines of `install-host.ps1` and `installer/linux/install.sh`;
   - the README's install section and its **Not yet** line, and
     [known-gaps.md](known-gaps.md) if it mentions loading by hand.

   Keep the Load unpacked steps for developers and for browsers the stores
   do not serve: the unpacked ID keeps working.
3. Users who loaded the extension by hand and then install the store's copy
   have two: they remove the unpacked one in `chrome://extensions`. In
   Firefox the signed copy replaces the temporary one.

## Each release

Push the tag as usual ([Building](building.md)). With the keys in place the
new version goes to both stores; without them, upload the two zips from the
release page in each store's dashboard. Each store takes a version once, and
only a higher one than it has. A pre-release tag (`v0.6.0-rc.1`) needs the
version it leads to in the extension's manifest (`0.6.0`), since a browser's
version has only numbers, and goes to no store.

---

[← Back to the help pages](README.md)
