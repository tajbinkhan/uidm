//! What the desktop window needs besides its daemon connection (see
//! `daemon::DaemonClient`): ports for its own files, the grabber's library,
//! list files, the settings' disk checks, file pickers and the operating
//! system.

use std::path::{Path, PathBuf};

use udm_domain::download::{Download, DownloadEntry};
use udm_domain::grab::{Filter, GrabRequest, Project, Template};
use udm_domain::ports::{FilesError, SettingsError};
use udm_domain::settings::{Settings, Sound};
use udm_domain::theme::{CustomTheme, InvalidTheme, SystemContrast};

/// The window's own files, such as its layout: small, rewritten whole, and
/// kept per user.
pub trait WindowFiles: Send + Sync {
    /// The window's file `name`, or `None` when it was never saved or
    /// cannot be read.
    fn read(&self, name: &str) -> Option<Vec<u8>>;

    /// Save `bytes` as the window's file `name`, without waiting: the
    /// write happens on another thread and replaces the file whole, so a
    /// crash never leaves half a file. Of two saves of one name, the later
    /// wins.
    fn save(&self, name: &str, bytes: Vec<u8>);
}

/// The site grabber's saved projects, templates and filters, for the
/// grabber dialog. The window has one when the projects folder can be
/// found.
pub trait GrabLibrary: Send + Sync {
    /// Where the projects are kept, for a file picker to start in.
    fn folder(&self) -> PathBuf;

    /// The saved projects' names, the most recently used first.
    fn recent(&self) -> Vec<String>;

    /// The project named `name`.
    fn load(&self, name: &str) -> Result<Project, FilesError>;

    /// A project file from anywhere, such as one copied from another
    /// computer, read and checked as a saved one is.
    fn load_file(&self, path: &Path) -> Result<Project, FilesError>;

    /// Save `project` under its name, its login cookies sealed.
    fn save(&self, project: &Project) -> Result<(), FilesError>;

    /// Whether no saved project other than `own` (the one being edited)
    /// is called `name`.
    fn name_is_unique(&self, name: &str, own: Option<&str>) -> bool;

    /// The request a new project starts from.
    fn template(&self) -> GrabRequest;

    /// The built-in templates and the saved ones.
    fn templates(&self) -> Vec<Template>;

    /// Save `req` as the template named `name`, and as the one a new
    /// project starts from.
    fn save_template(&self, name: &str, req: &GrabRequest) -> Result<(), FilesError>;

    /// Delete the saved template named `name`.
    fn delete_template(&self, name: &str) -> Result<(), FilesError>;

    /// The file-type filters the user keeps.
    fn filters(&self) -> Vec<Filter>;

    fn save_filters(&self, filters: &[Filter]) -> Result<(), FilesError>;
}

/// The download list's file formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListFormat {
    Json,
    Text,
    /// IDM's export format.
    Ef2,
}

/// Why a list could not be read, in words for the user.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ListError(pub String);

/// Why a file could not be read whole.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReadError {
    /// It is larger than the most the caller takes, in bytes, so it was not
    /// read.
    #[error("The file is larger than {max} bytes.")]
    TooBig { max: u64 },
    #[error(transparent)]
    Failed(#[from] ListError),
}

/// Exporting and importing the download list, and reading the files the
/// window opens.
pub trait ListFiles: Send + Sync {
    /// The text of the list file at `path`, whatever its encoding.
    fn read(&self, path: &Path) -> Result<String, ListError>;

    /// The bytes of the file at `path`, such as a `.torrent` file the window
    /// opens, unless it is larger than `max` bytes.
    fn read_bytes(&self, path: &Path, max: u64) -> Result<Vec<u8>, ReadError>;

    /// Save `text` as the list file at `path`, replacing it whole.
    fn write(&self, path: &Path, text: &str) -> Result<(), ListError>;

    /// The downloads in a list's `text`, and how many lines were not
    /// downloads.
    fn import(&self, text: &str) -> Result<(Vec<Download>, usize), ListError>;

    /// `entries` written as `format`.
    fn export(&self, format: ListFormat, entries: &[&DownloadEntry]) -> String;
}

/// A kind of file a picker offers: its name, such as "Lists", and its
/// extensions without the dot, or `*` for any file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileKind {
    pub name: String,
    pub extensions: Vec<String>,
}

impl FileKind {
    pub fn new(name: &str, extensions: &[&str]) -> FileKind {
        FileKind { name: name.to_string(), extensions: extensions.iter().map(|e| e.to_string()).collect() }
    }
}

/// The system's dialogs for choosing a file or folder. Each call waits
/// until the user chooses or cancels, so the window calls them on a worker.
/// `None` means the user cancelled.
pub trait FilePicker: Send + Sync {
    /// A folder, starting in `start` when given.
    fn pick_folder(&self, title: &str, start: Option<&Path>) -> Option<PathBuf>;

    /// An existing file of one of `kinds` (any file when empty), starting
    /// in `start` when given.
    fn pick_file(&self, title: &str, kinds: &[FileKind], start: Option<&Path>) -> Option<PathBuf>;

    /// Where to save a new file, suggesting the name `name`.
    fn save_file(&self, title: &str, name: &str, kinds: &[FileKind]) -> Option<PathBuf>;
}

/// The checks of the settings that look at the disk.
pub trait SettingsChecks: Send + Sync {
    /// The rules `Settings::validate` checks, and then that folder settings
    /// do not name files and ffmpeg is not a folder.
    fn validate(&self, settings: &Settings) -> Result<(), SettingsError>;

    /// The ffmpeg `settings` would use: the one set if it exists, else the
    /// first on `PATH`.
    fn ffmpeg(&self, settings: &Settings) -> Option<PathBuf>;

    /// The 7-Zip `settings` would use: the one set if it exists, else one
    /// on `PATH` or where 7-Zip installs itself.
    fn seven_zip(&self, settings: &Settings) -> Option<PathBuf>;
}

/// The system's fixed proxies, as Options' Get System copies them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SystemProxy {
    /// `host:port` for http addresses.
    pub http: Option<String>,
    /// `host:port` for https addresses.
    pub https: Option<String>,
    /// `scheme://host:port` of a SOCKS proxy.
    pub socks: Option<String>,
    /// `host:port` of an FTP proxy.
    pub ftp: Option<String>,
    /// The addresses that go without a proxy.
    pub bypass: Vec<String>,
}

/// What the window asks of the operating system.
pub trait SystemServices: Send + Sync {
    /// The system's fixed proxies, without a configuration script. `None`
    /// when none is set.
    fn fixed_proxy(&self) -> Option<SystemProxy>;

    /// Whether the power plan lets programs wake the computer, on mains
    /// power and on battery. `None` when it cannot be read.
    fn wake_timers_allowed(&self) -> Option<(bool, bool)>;

    /// Whether this system can ask for administrator rights to save a file.
    fn can_elevate(&self) -> bool;

    /// Whether a downloaded file's creation time can be set.
    fn can_set_creation_time(&self) -> bool;

    /// Whether this is a portable copy, which keeps everything in its own
    /// folder and does not start when the user signs in.
    fn is_portable(&self) -> bool;

    /// What the system calls files with extension `ext` (no dot), such as
    /// "PDF Document", from the file types registered on this computer.
    /// `None` where the system names no file types (only Windows does).
    fn file_type_name(&self, ext: &str) -> Option<String>;

    /// Play `file` (a WAV file) for `sound`, or the system's sound for it,
    /// without waiting.
    fn play(&self, sound: Sound, file: Option<&Path>);

    /// Keep `password` for `username` in the system's credential store
    /// under `target`.
    fn save_credential(&self, target: &str, username: &str, password: &str) -> Result<(), FilesError>;

    /// The system's high-contrast colors while its high-contrast setting
    /// is on, `None` while it is off or where the system has none.
    fn contrast(&self) -> Option<SystemContrast>;
}

/// The theme files in the themes folder, beside the settings file.
pub trait ThemeFiles: Send + Sync {
    /// The themes folder, made if it is missing, for Options' Open themes
    /// folder.
    fn open_folder(&self) -> Result<PathBuf, FilesError>;

    /// Every theme file in the folder, read: its theme, or why it is not
    /// one. None when the folder is missing.
    fn read(&self) -> Vec<ThemeFile>;
}

/// One file of the themes folder, as [`ThemeFiles::read`] found it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeFile {
    /// The file's name, such as `Ocean.theme`.
    pub file: String,
    pub theme: Result<CustomTheme, ThemeFileError>,
}

/// Why a theme file gives no theme.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ThemeFileError {
    #[error("The file cannot be read.")]
    Unreadable,
    #[error("Line {line} is not a setting = value line.")]
    NotASetting { line: usize },
    #[error("Line {line}: {error}")]
    Invalid { line: usize, error: InvalidTheme },
    #[error("The theme has no name; add a name = line.")]
    NoName,
    #[error("Another theme file already has the name {0}.")]
    NameTaken(String),
}

/// A browser UDM can register with: its name, and the place it looks for
/// programs like UDM, which some browsers share.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserSlot {
    pub name: String,
    pub place: String,
}

impl BrowserSlot {
    /// Whether `other` reads the same registration, so the two are
    /// registered together.
    pub fn same_place(&self, other: &BrowserSlot) -> bool {
        self.place == other.place
    }
}

/// The browser a program is, as far as UDM can tell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentifiedBrowser {
    /// The browser whose registration it reads.
    pub browser: BrowserSlot,
    /// Whether that is known, rather than guessed from its family.
    pub known: bool,
}

/// The system clipboard's text, for the clipboard monitor that offers the
/// links the user copies.
pub trait Clipboard: Send + Sync {
    /// The clipboard's text, if it holds any.
    fn text(&self) -> Option<String>;
    /// A number that changes whenever the clipboard does, so the text is
    /// read only after a change.
    fn sequence(&self) -> u32;
}

/// Registering UDM with the browsers, for Options.
pub trait BrowserRegistry: Send + Sync {
    /// The browsers UDM knows that are installed for this user or
    /// registered, each with whether it is registered.
    fn known(&self) -> Vec<(BrowserSlot, bool)>;

    /// The browser the program at `program` is, or `None` when UDM cannot
    /// tell.
    fn identify(&self, program: &Path) -> Option<IdentifiedBrowser>;

    /// Whether UDM is registered with `browser`.
    fn registered(&self, browser: &BrowserSlot) -> bool;

    /// Register UDM with `browser`, or with `on` false take it out.
    fn set_registered(&self, browser: &BrowserSlot, on: bool) -> Result<(), FilesError>;

    /// A name for the browser at `program`, from its file name.
    fn display_name(&self, program: &Path) -> String;

    /// The installed browsers that read Google Chrome's registration.
    fn chrome_readers(&self) -> Vec<String>;

    /// Whether `browser` can be restarted: one UDM knows the program of,
    /// or an added one, whose `program` is given.
    fn can_restart(&self, browser: &BrowserSlot, program: Option<&Path>) -> bool;

    /// Close `browser` (or the added one at `program`) and start it again,
    /// waiting for it.
    fn restart(&self, browser: &BrowserSlot, program: Option<&Path>) -> Result<(), FilesError>;
}
