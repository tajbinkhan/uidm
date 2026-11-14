# Catch browser downloads

Let Chrome, Edge, Brave, Vivaldi, Opera or Firefox hand their downloads to UDM, cookies and all, so
files behind a sign-in work too.

1. **Add the extension to the browser once.** It is not in the browsers' stores yet, so you load it
   by hand: [Chrome, Edge, Brave, Vivaldi and Opera](../install.md#windows) (the same steps on
   [macOS and Linux](../install.md#browsers-on-macos-and-linux)), or
   [Firefox](../install.md#firefox). Setup on Windows, and UDM's first start on macOS and Linux,
   tell the browsers where UDM is.
2. **Check that it is connected.** Start UDM, then click UDM's icon in the browser's toolbar (pin
   it from the browser's extensions menu if you do not see it). The popup should say *Connected.
   Downloads you start in the browser go to UDM.* and **Capture downloads** should be on. If it
   says it is not connected, start UDM, or restart the browser once after installing UDM.
3. **Download as usual.** Click a download link. The browser lets go of it and UDM opens
   **Download File Info**, where you pick the folder and click **Start Download**
   ([Your first download](first-download.md) goes through that dialog).
4. **Send any link to UDM.** Right-click a link, picture, video or audio and choose **Download with
   UDM**, or **Download later with UDM** to put it in a queue. **Download all links with UDM**
   offers every link on the page in a list to tick. Holding **Ctrl** while clicking a link to a file
   sends it to UDM too, whatever the rules below say.

## When the browser keeps a download

The browser keeps a download when UDM is not running, when **Capture downloads** is off, and in
these cases:

- **The file type is not in UDM's list.** UDM takes the types in **Options > File types**,
  **Automatically start downloading the following file types:** (every category's types and a few
  more at first; **Default** brings them back). Add a type there to have UDM take it.
- **The site is excluded**, in **Don't start downloading automatically from the following sites:**
  on the same page, or its address is in **Edit list ...**.
- **The file is small**: **Options > Browser** can set a **Minimum size**.
- **You held Alt** while clicking the link, or pressed **Skip next download** in the popup, to keep
  that one download in the browser. **Options > General > Keys...** changes the keys.
- **It is a private window**, unless **Take over downloads in private windows** is on in
  **Options > General** (and the browser lets the extension run there).
- **Capture in this tab** is off in the popup for that tab.

[What the extension takes](../browser-capture.md) has the whole list of rules.

## Safari

Safari on a Mac gets the extension from UDM.app ([how to turn it on](../install.md#safari)). Safari
has no downloads API, so UDM takes a link to a file of a listed type as you click it, and leaves
downloads that Safari starts by itself (a form's, a script's) to Safari.
[Known gaps](../known-gaps.md) lists what else differs there.
