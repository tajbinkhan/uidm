# Resume a broken download

A download stopped halfway: the connection dropped, the computer restarted, or the server sent an
error. UDM keeps what already came, so it carries on from there instead of starting over.

1. **Find it in the list.** Pick **Unfinished** under **Categories**. Its **Status** says why it
   stopped: **Interrupted** (UDM or the computer stopped while it ran), **Failed** (the server or
   the network gave up) or **Canceled** (it was stopped by hand).
2. **Select it and click Resume** in the toolbar, or right-click it and choose **Resume
   Download**. **Downloads > Resume All** (also in the tray icon's menu) starts every stopped and
   interrupted download at once.
3. **Check that it carried on.** The progress starts from where it was, not from 0%. Double-click
   the download for its window: **Resume capability: Yes** means the server sends parts of the
   file, so UDM can resume it. With **No**, the server only sends the whole file, and the download
   starts again from the beginning.

To have UDM pick up unfinished downloads by itself whenever it starts, tick **Resume unfinished
downloads at startup** in **Options > Behavior**. A queue can try failed downloads again:
**Number of retries for each file if downloading failed** in the Scheduler
([Download overnight](download-overnight.md)).

## When the link has expired

Many sites hand out addresses that work only for a while, so resuming the next day fails. Give the
download a new address and keep what is already downloaded:

1. Right-click the download and choose **Refresh download address**. The **New link** window
   opens.
2. Either paste a new address for the same file and click **Use this link**, or choose **Use my next
   download in the browser** and start the download again on its web page: the extension hands that
   download to UDM as the new link, instead of starting a second copy.
3. If the new link turns out to be another file (another size, or the server says it changed), UDM
   asks *Start over from the beginning with the new link?* **Start over** downloads it from the
   beginning; **Cancel** leaves the download as it was.

## When the site asks for a login

A download that needs a user name and password stops and opens **Login**, which names the site. Fill
them in and the download goes on; **Save this password** keeps them for that site in
**Options > Sites Logins**. [Proxies and logins](../proxies-and-logins.md) has the details.

More: [Using UDM](../using-udm.md) (the download window, **New link**), and
[how the engine resumes a file](../architecture.md).
