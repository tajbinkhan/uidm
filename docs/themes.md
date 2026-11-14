# Themes and interface size

**Options > General** has a **Theme**, and **Options > Behavior** an
**Interface size**. Both change the window as soon as you save.

## The built-in themes

| Theme | What it looks like |
|---|---|
| **Follow Windows** | Light or Dark, as Windows' app mode is. While Windows' contrast theme is on (Settings > Accessibility > Contrast themes), UDM takes its colors instead. |
| **Light** | Dark text on white. |
| **Dark** | Light text on near black. |
| **High contrast** | Windows' contrast theme colors while one is on. Otherwise white on black, white edges twice as wide as usual, a yellow edge on the control under the mouse, and selections in a blue that white text reads on. Every color reads at 7:1 or better against what it is drawn on. |

macOS and Linux have no contrast colors that every program takes, so there
**Follow Windows** follows only the light or dark mode, and **High contrast**
is always UDM's own.

## Interface size

**Interface size** goes from 80% to 200% in steps of 10%. Everything in the
window grows with it: text, buttons, the download list, the toolbar and every
dialog. Ctrl+Plus and Ctrl+Minus zoom the window for a while without changing
the setting; the setting takes over again when it or the theme changes.

At a large size the screen holds fewer points, so UDM keeps things
reachable:

- the main window's smallest size is never more than 90% of the screen,
- dialogs are at most 85% of the screen's height and 95% of its width, and
  scroll past that,
- each Options page gets shorter so its tabs and Save button stay in view,
- the toolbar scrolls sideways when its buttons are wider than the window.

## Making your own theme

A theme is a small text file whose name ends in `.theme`, in the `themes`
folder beside UDM's settings:

| System | Folder |
|---|---|
| Windows | `%APPDATA%\UDM\themes` |
| macOS | `~/Library/Application Support/UDM/themes` |
| Linux | `~/.config/udm/themes` (or `$XDG_CONFIG_HOME/udm/themes`) |
| Portable copy | `Data\themes` beside the programs |

**Options > General > Open themes folder** makes the folder if it is missing
and opens it. UDM looks at the folder every two seconds, so a new or
changed file shows in the Theme list within seconds, and a theme in use
changes as you save its file.

### The file

One `setting = value` per line. Lines starting with `#` or `;` are comments,
and so is anything after ` #` on a line (except in `name`). Blank lines and
`[section]` lines are skipped. Settings ignore case, and `-` or a space in a
setting's name reads as `_`.

```ini
# Ocean: a dark blue theme
name = Ocean
base = dark

background = #0b1e2d
foreground = #e6f1ff
accent = #13314a        # hovered buttons and the progress bar's track
selected = #1f4e79
link = #7fdbff

corner_radius = 6
```

A file sets only what it changes. Everything else comes from its **base**,
one of the built-in themes: `light` (when the file names none), `dark` or
`high_contrast`.

### Settings

| Setting | Value | What it sets |
|---|---|---|
| `name` | up to 40 characters | The name in the Theme list. Without it, the file's name (`Ocean.theme` is Ocean). |
| `base` | `light`, `dark` or `high_contrast` | What the file leaves out. |
| `dark` | `yes` or `no` | Whether the theme is dark, which the parts the file does not color follow. The base's when left out. |
| `background` | color | Windows, panels and the download list. |
| `foreground` | color | Text and icons. |
| `accent` | color | Hovered controls, secondary surfaces and a progress bar's empty part. |
| `pressed` | color | Pressed controls. |
| `border` | color | Separators and the edges of windows and menus. |
| `input` | color | The edges of buttons, text boxes and check boxes. |
| `hover_border` | color | The edge of the control under the mouse. |
| `selected` | color | Selected items and selected text. |
| `selected_text` | color | Text on selected items and progress bars, and the edge of the text box you type in. |
| `progress` | color | The filled part of a progress bar. |
| `stripe` | color | Every other row of the download list. |
| `link` | color | Links. |
| `error` | color | Errors and failed downloads. |
| `warning` | color | Warnings. |
| `shadow` | color | The shadow under windows and menus. Give it a transparency (`#00000000` for none). |
| `text_size` | 9 to 24 | Normal text, in points; small text, headings and code follow. 12 by default. |
| `control_height` | 16 to 40 | The height of buttons, text boxes and other controls. 20 by default. |
| `corner_radius` | 0 to 12 | How round the corners of controls are; menus, dialogs, lists and panels get twice as much. 4 by default. |
| `border_width` | 1 to 4 | The width of edges. 1 by default, 2 in High contrast. |

A color is `#rrggbb`, `#rgb`, or `#rrggbbaa` with a transparency (`ff`
opaque, `00` invisible). The sizes are before **Interface size** scales them.

Keep text readable: WCAG asks for 4.5:1 between text and what it is drawn on
(`foreground` on `background`, `accent` and `pressed`; `selected_text` on
`selected` and `progress`), and 7:1 for high contrast. A contrast checker
such as WebAIM's gives the ratio of two colors.

### When a file is wrong

A file that cannot be used is left out of the list, and Options says which
file and why under the Theme list, for example
`Ocean.theme is not a theme: Line 4: blue is not a color; write it as #rrggbb.`

A setting that names a theme whose file is gone (renamed, deleted or broken)
follows Windows until the file is back. Of two files that give one name
(case aside), the first by file name is used and the other is reported.

Theme files are read only on the computer they are on. To share a theme,
share its file.
