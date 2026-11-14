# AI assistants (MCP)

`udm-mcp` lets an AI assistant see and control your downloads. It is a
[Model Context Protocol](https://modelcontextprotocol.io) server: an assistant such as Claude
Desktop, Claude Code, Cursor or VS Code starts it and talks to it over standard input and output.
You can then ask things like "what is UDM downloading?", "download this link into my ISO
folder tonight" or "pause everything".

`udm-mcp` is one more client of UDM's background program (the daemon), like the window and
[`udm-cli`](command-line.md#the-command-line-tool). It starts the daemon when it is not
running, so no window needs to be open. It never connects to the internet itself; the downloads
you ask for do. Everything stays on your computer.

## What the assistant can do

| Tool | What it does |
| --- | --- |
| `list_downloads` | Lists the downloads (id, file name, status, bytes done and in all, address, saved path, error) and the queues (id, name, started or not, the downloads in it). It can keep only one status, such as `failed`. |
| `add_download` | Adds a link (`http`, `https`, `ftp` or `ftps`, or a torrent's `magnet:` link). Without a queue it starts now; with one (by name or id) it waits there until that queue starts. It can take a folder (a full path) and a file name. No dialog opens, and an address already in the list is downloaded again under a numbered name. |
| `pause_download` | Stops a running download and keeps what it has, or with `all`, stops every running download and started queue. |
| `resume_download` | Starts a stopped, failed or waiting download again from where it stopped, or with `all`, everything a pause stopped. A stopped download goes on as a new download with a new id, as Retry does in the window. |
| `remove_download` | Takes a download out of the list. Its file stays on disk unless `delete_files` is `true`. |
| `start_queue`, `stop_queue` | Starts or stops a queue by its name or id, or the main queue. |

The tools return as soon as UDM has done what was asked. A download goes on in UDM after
`add_download` returns, so ask the assistant to list the downloads to see how it is doing.

### Deleting files

Nothing deletes a file unless the assistant asks for it in so many words. `remove_download` keeps
the file by default. Its `delete_files` argument is off unless set, and the tool's description
tells the assistant to set it only after you have confirmed, in the conversation, that the file
itself should go. The tool is also marked as destructive, so assistants that ask before such
tools (most do by default) ask you first. Even then the file goes to the Recycle Bin or trash, not
away for good. A running download cannot be removed; pause it first.

## Where `udm-mcp` is

Every UDM package has it, next to the other programs:

| Installed with | `command` to give the assistant |
| --- | --- |
| Windows Setup | `C:\Users\YOU\AppData\Local\Programs\UDM\udm-mcp.exe` (Setup's usual folder; check yours) |
| Windows portable zip | the copy's `target\release\udm-mcp.exe` |
| macOS | `/Applications/UDM.app/Contents/MacOS/udm-mcp` |
| Linux `.deb` or `.rpm` | `/usr/bin/udm-mcp` |
| Linux tar (`install.sh`) | `/home/YOU/.local/bin/udm-mcp` |
| Linux AppImage | the AppImage's full path, with `args` `["udm-mcp"]` |
| Built from source | `target/release/udm-mcp` in your checkout, as a full path |

Give the full path. Assistants start programs with their own `PATH`, which often leaves out
`~/.local/bin` or UDM's folder. In JSON, write each `\` in a Windows path as `\\`.

## Setting it up

Each assistant keeps a list of MCP servers. Add one named `udm` whose command is
`udm-mcp`'s path. The files below are where each assistant usually keeps that list; if your
version differs, its own documentation has the current place. After saving, restart the assistant
(or reload its MCP servers) and check that the UDM tools appear.

### Claude Desktop

Open **Settings**, then **Developer**, then **Edit Config**. That opens
`claude_desktop_config.json`, which is usually:

- Windows: `%APPDATA%\Claude\claude_desktop_config.json`
- macOS: `~/Library/Application Support/Claude/claude_desktop_config.json`

Add UDM under `mcpServers` (keep any servers already there):

```json
{
  "mcpServers": {
    "udm": {
      "command": "/Applications/UDM.app/Contents/MacOS/udm-mcp",
      "args": []
    }
  }
}
```

On Windows the command looks like
`"C:\\Users\\YOU\\AppData\\Local\\Programs\\UDM\\udm-mcp.exe"`. Quit Claude Desktop fully
(from its tray or menu bar icon) and start it again.

### Claude Code

Add it from a terminal. `--scope user` makes it available in every project; leave it out to add it
to the current project only:

```sh
claude mcp add --scope user udm -- /usr/bin/udm-mcp
```

For the AppImage, put `udm-mcp` after the AppImage's path:
`claude mcp add --scope user udm -- /home/YOU/Apps/UDM.AppImage udm-mcp`.
`claude mcp list` shows whether it connects. A project can also share the setting with its other
users in a `.mcp.json` file at its root, in the same `mcpServers` shape as Claude Desktop's.

### Cursor

Cursor reads `~/.cursor/mcp.json` for every project (on Windows,
`%USERPROFILE%\.cursor\mcp.json`), or `.cursor/mcp.json` inside one project. Its **Settings**
also have an MCP page that opens the file. Use the same shape:

```json
{
  "mcpServers": {
    "udm": {
      "command": "/usr/bin/udm-mcp",
      "args": []
    }
  }
}
```

### VS Code

VS Code uses MCP servers in Copilot's agent mode. A workspace keeps them in `.vscode/mcp.json`;
the **MCP: Open User Configuration** command opens the list for all workspaces, and **MCP: Add
Server** walks through adding one. VS Code calls the list `servers`, not `mcpServers`:

```json
{
  "servers": {
    "udm": {
      "type": "stdio",
      "command": "/usr/bin/udm-mcp",
      "args": []
    }
  }
}
```

### Other assistants

Any assistant that starts local MCP servers over stdio works. Give it `udm-mcp`'s full path as
the command, with no arguments (or `udm-mcp` as the one argument for the AppImage).

## Good to know

- **The daemon stays up while the assistant runs.** `udm-mcp` keeps its connection to the
  daemon open between requests, as the window does, so the daemon does not exit while the
  assistant is open. If the daemon stops anyway (you chose **Exit** in the tray, say), the next
  request starts it again.
- **A named copy.** To point an assistant at a second, separate UDM (a
  [named copy](building.md#build), such as a test build), set `UDM_INSTANCE`
  in the server's `env`, for example `"env": { "UDM_INSTANCE": "test" }`. A portable copy's
  `udm-mcp` uses that copy's own data, like its other programs.
- **Its log.** `udm-mcp.log` sits in UDM's data folder, beside the daemon's log (see
  [How UDM fits together](architecture.md)). It records each tool call by name and what was
  refused, never cookies or passwords. Standard output carries only the protocol.
- **Protocol versions.** `udm-mcp` speaks MCP versions 2024-11-05, 2025-03-26, 2025-06-18 and
  2025-11-25, and takes the one the assistant asks for. Newer assistants also get each tool's
  title, its read-only and destructive hints, and results as structured JSON besides the text.
- **What it does not do.** It does not follow a download to the end, change settings, edit
  queues or schedules, or run the site grabber. [`udm-cli`](command-line.md#the-command-line-tool)
  and the [automation API](command-line.md#the-automation-api) are there for scripts.
