//! Errors as the user reads them (AGENTS.md section 9): commands fail with
//! an [`InvokeError`] whose message the shell has already translated, and
//! [`text`] turns the domain's and the daemon's errors into the user's
//! language.

use udm_application::daemon::Failure;
use udm_domain::address::AddressError;

use crate::contract::{ErrorKind, InvokeError};
use crate::i18n::{fill, Language};

/// The daemon's failure, in the user's language.
pub fn failure(lang: &'static Language, e: &Failure) -> String {
    text::daemon(lang, e)
}

fn error(kind: ErrorKind, message: impl Into<String>) -> InvokeError {
    InvokeError { kind, message: message.into() }
}

pub fn offline(lang: &'static Language) -> InvokeError {
    error(ErrorKind::Offline, lang.tr("UDM's background service is not connected. Try again in a moment."))
}

pub fn gone(lang: &'static Language) -> InvokeError {
    error(ErrorKind::Gone, lang.tr("That download is no longer in the list."))
}

pub fn address(lang: &'static Language, e: &AddressError) -> InvokeError {
    let text = match e {
        AddressError::Bad(address, _) | AddressError::Scheme(address) | AddressError::NotDownload(address) => {
            fill(lang.tr("{address} is not an http, https or ftp address."), &[("address", address)])
        }
    };
    error(ErrorKind::Invalid, text)
}

/// Why Options could not save.
pub fn options(lang: &'static Language, e: &crate::options::OptionsError) -> InvokeError {
    use crate::options::OptionsError as E;
    let message = match e {
        E::Invalid(invalid) => text::settings(lang, invalid),
        E::CategoryName => lang.tr("Type a name for each category.").to_string(),
        E::CategoryTwice(name) => fill(lang.tr("There are two categories named \"{name}\"."), &[("name", name)]),
        E::Theme(name) => fill(lang.tr("\"{name}\" is not a theme."), &[("name", name)]),
        E::Port => lang.tr("Type a port from 1 to 65535.").to_string(),
        E::DebridService => lang.tr("Choose the debrid service the account is with.").to_string(),
        E::Network(n) => text::network(lang, n),
    };
    error(ErrorKind::Invalid, message)
}

/// Why a queue cannot be saved.
pub fn queue(lang: &'static Language, e: &udm_domain::queue::InvalidQueue) -> InvokeError {
    error(ErrorKind::Invalid, text::queue(lang, e))
}

/// Why settings cannot be saved.
pub fn settings(lang: &'static Language, e: &udm_domain::settings::InvalidSettings) -> InvokeError {
    error(ErrorKind::Invalid, text::settings(lang, e))
}

/// Why the crawl cannot start.
pub fn grab(lang: &'static Language, e: &udm_domain::grab::InvalidGrab) -> InvokeError {
    error(ErrorKind::Invalid, text::grab(lang, e))
}

/// A file the shell could not read or write, such as a project.
pub fn files(e: &udm_domain::ports::FilesError) -> InvokeError {
    error(ErrorKind::Gone, e.to_string())
}

pub fn no_projects(lang: &'static Language) -> InvokeError {
    error(ErrorKind::Gone, lang.tr("There is no projects folder."))
}

pub fn category(lang: &'static Language, e: &crate::tools::CategoryError) -> InvokeError {
    use crate::tools::CategoryError as E;
    let message = match e {
        E::NoName => lang.tr("Type a name for the category.").to_string(),
        E::Taken(name) => fill(lang.tr("There is a category named \"{name}\" already."), &[("name", name)]),
    };
    error(ErrorKind::Invalid, message)
}

pub fn batch(lang: &'static Language, e: &udm_domain::batch::BatchError) -> InvokeError {
    error(ErrorKind::Invalid, text::batch(lang, e))
}

pub fn list(e: &udm_application::window::ListError) -> InvokeError {
    error(ErrorKind::Gone, e.0.clone())
}

/// A refusal whose text is already the user's to read.
pub fn invalid_text(message: String) -> InvokeError {
    error(ErrorKind::Invalid, message)
}

pub fn invalid(lang: &'static Language, english: &'static str) -> InvokeError {
    error(ErrorKind::Invalid, lang.tr(english))
}

/// The shell itself failed (a window did not open).
#[derive(Debug, thiserror::Error)]
pub enum UiError {
    #[error("the window did not start: {0}")]
    Tauri(#[from] tauri::Error),
    #[error("the TypeScript bindings were not written: {0}")]
    Bindings(String),
}

/// The domain's errors, and the daemon's, in the window's language. Each
/// English text matches the error's own message, which the daemon, the logs
/// and the clients that do not translate use.
pub mod text {
    use udm_application::daemon::service::ServiceError;
    use udm_application::daemon::Failure;
    use udm_application::downloads::{ConfirmError, StartOrWaitError};
    use udm_application::window::ThemeFileError;
    use udm_domain::batch::{BatchError, Bound, MAX_BATCH, MAX_WILDCARD_SIZE};
    use udm_domain::command::{CommandError, PATH_TOKEN};
    use udm_domain::download::{DownloadError, Mismatch};
    use udm_domain::gallery::GalleryError;
    use udm_domain::grab::{InvalidGrab, AT_ONCE_RANGE, DELAY_MS_RANGE, DEPTH_RANGE, MAX_MB_RANGE, MAX_PAGES_RANGE};
    use udm_domain::package::{PackageError, UnpackFailure, MAX_NAME};
    use udm_domain::pairing::{PairingError, RemoteFault, MAX_DEVICES};
    use udm_domain::ports::SettingsError;
    use udm_domain::queue::{InvalidQueue, QueueError, QUEUE_CONCURRENCY_RANGE, QUEUE_RETRIES_MAX};
    use udm_domain::settings::{
        InvalidSettings, ProxyEntry, SettingsField, CONNECTIONS_RANGE, GENERAL, IDLE_MINUTES_RANGE,
        MAX_CONCURRENT_RANGE, QUOTA_HOURS_RANGE, REMOTE_PORT_RANGE, SERVER_CONNECTIONS_RANGE, SPEED_LIMIT_RANGE_KBPS,
        TEXT_SIZE_RANGE,
    };
    use udm_domain::theme::InvalidTheme;
    use udm_domain::update::UpdateError;

    use crate::i18n::{trf, Language};

    fn bound(lang: &'static Language, b: Bound) -> &'static str {
        match b {
            Bound::From => lang.tr("From"),
            Bound::To => lang.tr("To"),
        }
    }

    /// A batch pattern, IDM-style address or list that makes no downloads.
    pub fn batch(lang: &'static Language, e: &BatchError) -> String {
        use BatchError as E;
        match e {
            E::NumberTooBig(r) => trf!(lang, "[{range}]: the number is too big", range = r),
            E::StepTooBig(r) => trf!(lang, "[{range}]: the step is too big", range = r),
            E::StepZero(r) => trf!(lang, "[{range}]: the step must be at least 1", range = r),
            E::MixedCaseRange(r) => trf!(lang, "[{range}]: both letters must be the same case", range = r),
            E::PatternNotWeb => lang.tr("The pattern must start with http://, https:// or ftp://.").to_string(),
            E::PatternTooBig(n) => {
                trf!(
                    lang,
                    "The pattern makes {count} addresses; at most {max} can be added at once.",
                    count = n,
                    max = MAX_BATCH
                )
            }
            E::AddressNotWeb => lang.tr("The address must start with http://, https:// or ftp://.").to_string(),
            E::NoAsterisk => lang.tr("The address must have a * where the numbers or letters go.").to_string(),
            E::TwoAsterisks => lang.tr("The address may have only one *.").to_string(),
            E::NotWholeNumber(b) => trf!(lang, "{field} must be a whole number.", field = bound(lang, *b)),
            E::BoundTooBig(b) => trf!(lang, "{field} is too big.", field = bound(lang, *b)),
            E::WildcardSize => trf!(lang, "The wildcard size must be from 1 to {max}.", max = MAX_WILDCARD_SIZE),
            E::NumbersReversed(a, b) => {
                trf!(lang, "From ({from}) must not be greater than To ({to}).", from = a, to = b)
            }
            E::NotOneLetter(b) => trf!(lang, "{field} must be one letter, a to z.", field = bound(lang, *b)),
            E::MixedCaseLetters => lang.tr("From and To must be letters of the same case.").to_string(),
            E::LettersReversed(a, b) => trf!(lang, "From ({from}) must not be after To ({to}).", from = a, to = b),
            E::RangeTooBig(a, b, n) => trf!(
                lang,
                "From {from} to {to} makes {count} addresses; at most {max} can be added at once.",
                from = a,
                to = b,
                count = n,
                max = MAX_BATCH
            ),
            E::ListTooLong(n) => {
                trf!(
                    lang,
                    "The list has {count} addresses; at most {max} can be added at once.",
                    count = n,
                    max = MAX_BATCH
                )
            }
        }
    }

    /// A scanner command line that cannot be run.
    pub fn command(lang: &'static Language, e: &CommandError) -> String {
        match e {
            CommandError::UnclosedQuote => lang.tr("The scanner command has an unclosed quote.").to_string(),
            CommandError::Empty => lang.tr("The scanner command is empty.").to_string(),
            CommandError::NoPath => {
                trf!(lang, "The scanner command must contain {token} where the file goes.", token = PATH_TOKEN)
            }
        }
    }

    fn field(lang: &'static Language, f: &SettingsField) -> String {
        match f {
            SettingsField::DownloadFolder => lang.tr("The download folder").to_string(),
            SettingsField::CategoryFolder(name) => trf!(lang, "The folder of \"{name}\"", name = name),
            SettingsField::Category(name) => trf!(lang, "The category \"{name}\"", name = name),
            SettingsField::CapturedTypes => lang.tr("The captured file types").to_string(),
            SettingsField::VideoTypes => lang.tr("The video types").to_string(),
            SettingsField::PlayerTypes => lang.tr("The types taken from web players").to_string(),
            SettingsField::AutoStartTypes => lang.tr("The file types that start without asking").to_string(),
            SettingsField::UnpackFolder => lang.tr("The folder to unpack into").to_string(),
        }
    }

    fn proxy(lang: &'static Language, p: ProxyEntry) -> &'static str {
        match p {
            ProxyEntry::Http => lang.tr("The HTTP proxy"),
            ProxyEntry::Https => lang.tr("The HTTPS proxy"),
            ProxyEntry::Ftp => lang.tr("The FTP proxy"),
            ProxyEntry::Socks => lang.tr("The SOCKS proxy"),
        }
    }

    /// Settings that break one of their rules.
    pub fn settings(lang: &'static Language, e: &InvalidSettings) -> String {
        use InvalidSettings as E;
        match e {
        E::ScriptNotWeb => lang.tr("The configuration script must be an http or https address.").to_string(),
        E::Language(code) => trf!(lang, "\"{code}\" is not a language code, such as de or pt-BR.", code = code),
        E::TextSize => {
            trf!(lang, "The interface size must be between {lo}% and {hi}%.", lo = TEXT_SIZE_RANGE.0, hi = TEXT_SIZE_RANGE.1)
        }
        E::QuotaHours => trf!(
                lang,
            "The quota's window must be between {lo} and {hi} hours.",
            lo = QUOTA_HOURS_RANGE.0,
            hi = QUOTA_HOURS_RANGE.1
        ),
        E::QuotaZero => lang.tr("The quota must be at least 1 MB.").to_string(),
        E::ServerName(site) => trf!(lang, "\"{site}\" is not a server name, such as files.example.com.", site = site),
        E::ServerConnections(site) => trf!(
                lang,
            "The connections for {site} must be between {lo} and {hi}.",
            site = site,
            lo = SERVER_CONNECTIONS_RANGE.0,
            hi = SERVER_CONNECTIONS_RANGE.1
        ),
        E::SameCaptureKeys => lang.tr("The keys that skip and force capture must differ.").to_string(),
        E::InsertDelSkip => lang.tr("Insert and Del can only force capture, not skip it.").to_string(),
        E::TempNotFull(dir) => trf!(lang, "The temporary folder must be a full path: {path}", path = dir),
        E::UserAgentLines => lang.tr("The User-Agent must be one line of text.").to_string(),
        E::Connections => trf!(
                lang,
            "Connections per file must be between {lo} and {hi}.",
            lo = CONNECTIONS_RANGE.0,
            hi = CONNECTIONS_RANGE.1
        ),
        E::DownloadsAtOnce => trf!(
                lang,
            "Downloads at once must be between {lo} and {hi}.",
            lo = MAX_CONCURRENT_RANGE.0,
            hi = MAX_CONCURRENT_RANGE.1
        ),
        E::SpeedLimit => trf!(
                lang,
            "The speed limit must be between {lo} and {hi} KB/s.",
            lo = SPEED_LIMIT_RANGE_KBPS.0,
            hi = SPEED_LIMIT_RANGE_KBPS.1
        ),
        E::SpeedLimitPerFile => trf!(
                lang,
            "The speed limit per file must be between {lo} and {hi} KB/s.",
            lo = SPEED_LIMIT_RANGE_KBPS.0,
            hi = SPEED_LIMIT_RANGE_KBPS.1
        ),
        E::RemotePort => trf!(
                lang,
            "The remote-control port must be between {lo} and {hi}.",
            lo = REMOTE_PORT_RANGE.0,
            hi = REMOTE_PORT_RANGE.1
        ),
        E::RemoteAddress(address) => {
            trf!(lang, "\"{address}\" is not an IP address of this computer, such as 192.168.1.20.", address = address)
        }
        E::IdleMinutes => trf!(
                lang,
            "The time away must be between {lo} and {hi} minutes.",
            lo = IDLE_MINUTES_RANGE.0,
            hi = IDLE_MINUTES_RANGE.1
        ),
        E::FolderEmpty(f) => trf!(lang, "{field} is empty.", field = field(lang, f)),
        E::FolderNotFull(f, dir) => trf!(lang, "{field} must be a full path: {path}", field = field(lang, f), path = dir),
        E::CategoryUnnamed => lang.tr("Every category needs a name.").to_string(),
        E::GeneralName => trf!(lang, "\"{name}\" is kept for files of every other type.", name = lang.tr(GENERAL)),
        E::TwoCategories(name) => trf!(lang, "There are two categories named \"{name}\".", name = name),
        E::NoFileTypes(name) => trf!(lang, "The category \"{name}\" has no file types.", name = name),
        E::BadFileType(f, ext) => trf!(
                lang,
            "{field} has an invalid file type \"{ext}\". Use letters and digits, such as zip or mp4.",
            field = field(lang, f),
            ext = ext
        ),
        E::VideoMinSize => lang.tr("The minimum video size is too large.").to_string(),
        E::NoPlayerTypes => lang.tr("List the types to take from web players, such as mp4.").to_string(),
        E::MenuItem(item) => trf!(lang, "\"{item}\" is not a context menu item.", item = item),
        E::CaptureMinSize => lang.tr("The minimum size to capture is too large.").to_string(),
        E::NotSite(site) => {
            trf!(lang, "\"{site}\" is not a site. Use a host name such as example.com or *.example.com.", site = site)
        }
        E::FfmpegNotFull(path) => trf!(lang, "ffmpeg must be the full path of ffmpeg.exe: {path}", path = path),
        E::SevenZipNotFull(path) => {
            trf!(lang, "7-Zip must be the full path of its program, such as 7z.exe: {path}", path = path)
        }
        E::ProxyNotHostPort(p) => trf!(lang, "{proxy} must be host:port, such as proxy.example.com:8080.", proxy = proxy(lang, *p)),
        E::NoProxy => lang.tr("Set at least one proxy, or choose another proxy option.").to_string(),
        E::NotLoginSite(site) => trf!(
                lang,
            "\"{site}\" is not a site. Use a host name such as example.com, or one with a folder such as example.com/private/*.",
            site = site
        ),
        E::LoginNoUser(site) => trf!(lang, "The login for {site} needs a user name.", site = site),
        E::TwoLogins(site) => trf!(lang, "There are two logins for {site}.", site = site),
        E::SoundNotWav(path) => trf!(lang, "A sound must be the full path of a .wav file: {path}", path = path),
        E::Scanner(e) => command(lang, e),
        E::DefenderNotHere => {
            lang.tr("Microsoft Defender is only on Windows. Choose a scanner program such as clamscan.").to_string()
        }
    }
    }

    /// Why settings could not be saved: a broken rule, translated, or a disk
    /// check's message.
    pub fn settings_error(lang: &'static Language, e: &SettingsError) -> String {
        match e {
            SettingsError::Invalid(e) => settings(lang, e),
            SettingsError::Refused(why) => why.clone(),
        }
    }

    /// A queue's settings or schedule that break a rule.
    pub fn queue(lang: &'static Language, e: &InvalidQueue) -> String {
        use InvalidQueue as E;
        match e {
            E::NoName => lang.tr("A queue needs a name.").to_string(),
            E::AtOnce => trf!(
                lang,
                "A queue runs between {lo} and {hi} downloads at once.",
                lo = QUEUE_CONCURRENCY_RANGE.0,
                hi = QUEUE_CONCURRENCY_RANGE.1
            ),
            E::Retries => trf!(lang, "A queue retries a download at most {max} times.", max = QUEUE_RETRIES_MAX),
            E::OpenNotFull(path) => trf!(lang, "The file to open must be a full path: {path}", path = path),
            E::StartTime(t) => trf!(lang, "\"{time}\" is not a start time. Use HH:MM, such as 23:00.", time = t),
            E::StopTime(t) => trf!(lang, "\"{time}\" is not a stop time. Use HH:MM, such as 07:00.", time = t),
            E::SameTimes => lang.tr("The stop time must differ from the start time.").to_string(),
            E::Repeat => lang.tr("Start again every 1 minute to 99 hours 59 minutes.").to_string(),
            E::Date(d) => trf!(lang, "\"{date}\" is not a date. Use YYYY-MM-DD.", date = d),
            E::NoDays => lang.tr("Pick at least one day, or a date, for the schedule.").to_string(),
            E::DayOfWeek => lang.tr("Days of the week run from 0 (Monday) to 6 (Sunday).").to_string(),
        }
    }

    /// A grabber request that cannot run.
    pub fn grab(lang: &'static Language, e: &InvalidGrab) -> String {
        use InvalidGrab as E;
        match e {
            E::StartNotAddress => lang.tr("The start page must be a full web address.").to_string(),
            E::StartNotWeb => lang.tr("The start page must start with http:// or https://.").to_string(),
            E::Depth => trf!(lang, "The depth must be between {lo} and {hi}.", lo = DEPTH_RANGE.0, hi = DEPTH_RANGE.1),
            E::AtOnce => {
                trf!(lang, "Files at once must be between {lo} and {hi}.", lo = AT_ONCE_RANGE.0, hi = AT_ONCE_RANGE.1)
            }
            E::NoCategory => lang.tr("Choose the category to save the files in.").to_string(),
            E::FolderNotFull(dir) => {
                trf!(lang, "The folder to save the files in must be a full path: {path}", path = dir)
            }
            E::Pages => {
                trf!(
                    lang,
                    "The page budget must be between {lo} and {hi}.",
                    lo = MAX_PAGES_RANGE.0,
                    hi = MAX_PAGES_RANGE.1
                )
            }
            E::Size => trf!(
                lang,
                "The size budget must be between {lo} and {hi} MB.",
                lo = MAX_MB_RANGE.0,
                hi = MAX_MB_RANGE.1
            ),
            E::Delay => trf!(
                lang,
                "The wait between pages must be between {lo} and {hi} ms.",
                lo = DELAY_MS_RANGE.0,
                hi = DELAY_MS_RANGE.1
            ),
            E::NoTypes => lang.tr("Choose at least one file type to keep.").to_string(),
            E::LoginNotAddress => lang.tr("The login page must be a full web address.").to_string(),
            E::LoginNotWeb => lang.tr("The login page must start with http:// or https://.").to_string(),
            E::MirrorNotFull(dir) => {
                trf!(lang, "The folder to keep the site in must be a full path: {path}", path = dir)
            }
            E::SizesReversed => lang.tr("The smallest file size is larger than the largest.").to_string(),
        }
    }

    /// A failure the daemon reported.
    pub fn daemon(lang: &'static Language, e: &Failure) -> String {
        match e {
            Failure::Service(e) => service(lang, e),
            Failure::Settings(e) => settings_error(lang, e),
            Failure::Mismatch(e) => mismatch(lang, e),
            Failure::InvalidMessage(why) => trf!(lang, "invalid message: {why}", why = why),
            Failure::Probe(why) | Failure::Other(why) => why.clone(),
        }
    }

    /// Why a new link does not fit a partial download.
    pub fn mismatch(lang: &'static Language, e: &Mismatch) -> String {
        match e {
            Mismatch::UnknownSize => lang
                .tr("The new link does not say how big its file is, so it cannot be matched to the partial download.")
                .to_string(),
            Mismatch::Size { new, partial } => trf!(
                lang,
                "The new link is for a {new} file; the partial download is of a {partial} file.",
                new = new.megabytes(),
                partial = partial.megabytes()
            ),
            Mismatch::Changed => lang
                .tr("The file at the new link has changed since the download started (its ETag or date differs).")
                .to_string(),
            Mismatch::NotResumable => {
                lang.tr("The new link's server cannot resume downloads, so it must start over.").to_string()
            }
        }
    }

    /// A command the daemon refused.
    fn service(lang: &'static Language, e: &ServiceError) -> String {
        match e {
            ServiceError::Queue(e) => queue_error(lang, e),
            ServiceError::Download(e) | ServiceError::Confirm(ConfirmError::Download(e)) => download(lang, e),
            ServiceError::Confirm(ConfirmError::Start(StartOrWaitError::Queue(e))) => queue_error(lang, e),
            // The engine's own words.
            ServiceError::Confirm(ConfirmError::Start(StartOrWaitError::Start(e))) => e.0.clone(),
            ServiceError::Update(e) => update(lang, e),
            ServiceError::Grab(e) => grab(lang, e),
            ServiceError::CrawlRunning => lang.tr("A site crawl is already running.").to_string(),
            ServiceError::BadBody(url) => trf!(lang, "the form body of {url} is not valid base64", url = url),
            ServiceError::NotDownloadAddress(url) => {
                trf!(lang, "{url} is not an http, https or ftp address", url = url)
            }
            ServiceError::Tool(e) => tool(lang, e),
            ServiceError::Pairing(e) => pairing(lang, e),
            ServiceError::NoPackage(id) => trf!(lang, "There is no package {id}.", id = id),
            ServiceError::PackageBusy => lang.tr("The package is being unpacked already.").to_string(),
            ServiceError::Gallery(e) => gallery(lang, e),
        }
    }

    /// Why a gallery page gave no pictures.
    pub fn gallery(lang: &'static Language, e: &GalleryError) -> String {
        match e {
        GalleryError::NotWebPage(url) => trf!(lang, "{url} is not an http or https page.", url = url),
        GalleryError::Missing => {
            lang.tr("Image galleries need gallery-dl. Install it in UDM's Options > Tools, or put gallery-dl next to UDM.")
                .to_string()
        }
        GalleryError::Unsupported => lang.tr("gallery-dl does not know this site.").to_string(),
        GalleryError::NothingFound => lang.tr("gallery-dl found no pictures on this page.").to_string(),
        // gallery-dl's own words.
        GalleryError::Failed(why) => format!("gallery-dl: {why}"),
        GalleryError::TimedOut(seconds) => {
            trf!(lang, "gallery-dl did not answer within {seconds} seconds.", seconds = seconds)
        }
        GalleryError::Busy => lang.tr("This page is being read already.").to_string(),
        GalleryError::TooMany => lang.tr("UDM is reading other galleries. Try again when they are done.").to_string(),
    }
    }

    /// Why a package name was refused.
    pub fn package_name(lang: &'static Language, e: &PackageError) -> String {
        match e {
            PackageError::EmptyName => lang.tr("The package needs a name.").to_string(),
            PackageError::LongName => {
                trf!(lang, "A package name may be at most {max} characters long.", max = MAX_NAME)
            }
            PackageError::NotAFolder(name) => trf!(lang, "{name} cannot be used as a folder name.", name = name),
        }
    }

    /// Why a package's archives could not be unpacked.
    pub fn unpack(lang: &'static Language, e: &UnpackFailure) -> String {
        match e {
            UnpackFailure::NoArchives => lang.tr("There are no archives among its files.").to_string(),
            UnpackFailure::NotComplete => lang.tr("Not all of its archives are downloaded yet.").to_string(),
            UnpackFailure::MissingPart { archive, part } => {
                trf!(lang, "Part {part} of {archive} is missing.", part = part, archive = archive)
            }
            UnpackFailure::NeedsSevenZip { archive } => trf!(
                lang,
                "{archive} needs 7-Zip, which was not found. Install 7-Zip, or set where it is in Options.",
                archive = archive
            ),
            UnpackFailure::Unsafe { archive, entry } => trf!(
                lang,
                "{archive} holds a file that would be written outside the folder: {entry}",
                archive = archive,
                entry = entry
            ),
            UnpackFailure::TooManyFiles { archive, max } => {
                trf!(lang, "{archive} holds more than {max} files.", archive = archive, max = max)
            }
            UnpackFailure::NoSpace { needed, free } => trf!(
                lang,
                "Unpacking needs {needed}, but only {free} is free.",
                needed = crate::projection::format::bytes(lang, *needed, 1),
                free = crate::projection::format::bytes(lang, *free, 1)
            ),
            // The archive's or the system's own words.
            UnpackFailure::Damaged { archive, detail } => {
                trf!(lang, "{archive} is damaged, or not an archive: {detail}", archive = archive, detail = detail)
            }
            UnpackFailure::Io { archive, detail } => format!("{archive}: {detail}"),
        }
    }

    /// Why yt-dlp or ffmpeg was not installed or updated.
    pub fn tool(lang: &'static Language, e: &udm_domain::tools::ToolError) -> String {
        use udm_domain::tools::ToolError as E;
        match e {
            E::NoBuild(tool, command) => {
                trf!(
                    lang,
                    "UDM cannot install {tool} on this system. Install it with: {command}",
                    tool = tool,
                    command = command
                )
            }
            E::Unreachable(why) => trf!(lang, "Could not reach GitHub: {why}", why = why),
            E::Answer(why) => trf!(lang, "GitHub's answer could not be read: {why}", why = why),
            E::NoAsset(name) => trf!(lang, "The latest release has no file {name}.", name = name),
            E::NoChecksum(name) => {
                trf!(lang, "The release publishes no checksum for {name}, so UDM will not install it.", name = name)
            }
            E::WrongChecksum(name) => {
                trf!(
                    lang,
                    "{name} does not match its published checksum. It was deleted and nothing was installed.",
                    name = name
                )
            }
            E::Download(why) => trf!(lang, "The download failed: {why}", why = why),
            E::Unpack(name, why) => trf!(lang, "Could not unpack {name}: {why}", name = name, why = why),
            E::Save { path, reason } => trf!(lang, "Could not save {path}: {reason}", path = path, reason = reason),
            E::UpToDate(tool) => trf!(lang, "{tool} is up to date.", tool = tool),
            E::Busy(tool) => trf!(lang, "{tool} is being installed already.", tool = tool),
        }
    }

    /// Why a network setting is not usable, or the adapters' list could not
    /// be read.
    pub fn network(lang: &'static Language, e: &udm_domain::network::NetworkError) -> String {
        use udm_domain::network::NetworkError as E;
        match e {
            E::NoAdapter => lang.tr("Choose a network adapter.").to_string(),
            E::BadAdapter => lang.tr("That is not the name of a network adapter.").to_string(),
            E::DohAddress => {
                lang.tr("The DNS over HTTPS address must start with https:// and name a server.").to_string()
            }
            E::Adapters(why) => {
                crate::i18n::fill(lang.tr("The network adapters could not be read: {error}"), &[("error", why)])
            }
        }
    }

    /// Why a debrid service would not unlock a link or show the account.
    pub fn debrid(lang: &'static Language, e: &udm_domain::debrid::DebridError) -> String {
        use udm_domain::debrid::DebridError as E;
        match e {
            E::BadKey => lang.tr("The service refused the API key, or no key is stored.").to_string(),
            E::NotPremium => lang.tr("The account is not premium.").to_string(),
            E::HostNotSupported => lang.tr("The service does not support this host.").to_string(),
            E::LinkDead => lang.tr("The file is not available on the host.").to_string(),
            E::Limit => lang.tr("A limit of the account or the host was reached. Try again later.").to_string(),
            E::IpNotAllowed => lang
                .tr("The service does not allow this computer's IP address. A VPN or a proxy is often the reason.")
                .to_string(),
            E::HostUnavailable => lang.tr("The file host is unavailable for now. Try again later.").to_string(),
            E::KeyForOther(other) => {
                trf!(lang, "The stored key is for {service}. Type a key for this service.", service = other.name())
            }
            E::Service(said) => trf!(lang, "The service said: {message}", message = said),
            E::Network(why) => trf!(lang, "Could not reach the service: {why}", why = why),
        }
    }

    /// Why pairing a phone, or removing one, failed.
    pub fn pairing(lang: &'static Language, e: &PairingError) -> String {
        match e {
            PairingError::WrongCode => {
                lang.tr("That pairing code is wrong or has run out. Show a new one in UDM's Options.").to_string()
            }
            PairingError::TooManyTries { wait } => {
                trf!(lang, "Too many wrong tries. Try again in {wait} seconds.", wait = wait)
            }
            PairingError::NotPaired => lang.tr("This device is not paired with UDM.").to_string(),
            PairingError::TooManyDevices => {
                trf!(lang, "{count} devices are paired already. Remove one in Options first.", count = MAX_DEVICES)
            }
            PairingError::NoDevice(id) => trf!(lang, "There is no paired device {id}.", id = id),
            PairingError::NotListening => lang.tr("Remote control is not on.").to_string(),
            // The device list's or the random source's own words.
            PairingError::Store(why) => why.clone(),
        }
    }

    /// Why remote control is on but its page is not served.
    pub fn remote_fault(lang: &'static Language, e: &RemoteFault) -> String {
        match e {
            RemoteFault::PortInUse { address } => {
                trf!(lang, "Another program is using {address}. Choose another port.", address = address)
            }
            RemoteFault::AddressUnavailable { address } => {
                trf!(
                    lang,
                    "This computer has no address {address}. Choose another, or every interface.",
                    address = address
                )
            }
            RemoteFault::NoPermission { address } => trf!(lang, "UDM may not listen on {address}.", address = address),
            RemoteFault::Certificate(why) => trf!(lang, "Could not make the page's certificate: {why}", why = why),
            RemoteFault::Other(why) => why.clone(),
        }
    }

    /// A change to the queues that was refused.
    fn queue_error(lang: &'static Language, e: &QueueError) -> String {
        match e {
            QueueError::NoQueue(id) => trf!(lang, "no queue with id {id}", id = id),
            QueueError::NoDownload(id) => trf!(lang, "no download with id {id}", id = id),
            QueueError::NameTaken(name) => trf!(lang, "There is already a queue named \"{name}\".", name = name),
            QueueError::MainQueue => lang.tr("The main queue cannot be deleted.").to_string(),
            QueueError::Finished => lang.tr("Only a download that has not finished can wait in a queue.").to_string(),
            QueueError::NotQueued => lang.tr("That download is not in a queue.").to_string(),
            QueueError::Invalid(e) => queue(lang, e),
        }
    }

    /// A change to a download that was refused.
    fn download(lang: &'static Language, e: &DownloadError) -> String {
        use DownloadError as E;
        match e {
            E::NoDownload(id) => trf!(lang, "no download with id {id}", id = id),
            E::StillRunning(id) => trf!(lang, "download {id} is still running; cancel it first", id = id),
            E::NotRetryable(id) => trf!(lang, "download {id} is not failed, canceled or interrupted", id = id),
            E::NotFinishedOrStopped => {
                lang.tr("Only a finished or stopped download can be downloaded again.").to_string()
            }
            E::PlaylistGone => lang
                .tr("This stream's playlist is no longer known. Play it in the browser again to download it.")
                .to_string(),
            E::StartedMeanwhile => lang.tr("The download started again in the meantime.").to_string(),
            E::BadSpeedLimit { kbps, lo, hi } => {
                trf!(
                    lang,
                    "{kbps} KB/s is not a speed limit between {lo} and {hi} KB/s.",
                    kbps = kbps,
                    lo = lo,
                    hi = hi
                )
            }
            E::NotStopped => lang.tr("Stop the download before changing its properties.").to_string(),
            E::LinkNeedsStop => lang.tr("Only a stopped download can get a new link.").to_string(),
            E::NotDownloadLink(url) => trf!(lang, "{url} is not an http, https or ftp link", url = url),
            E::NotWebAddress(url) => trf!(lang, "{url} is not an http or https address", url = url),
            E::LoginOnFinished => {
                lang.tr("A finished download keeps no login. Download it again to use one.").to_string()
            }
            E::RenameUnfinished => lang.tr("Only a finished download's file can be renamed.").to_string(),
            E::RelativeFolder(dir) => trf!(lang, "The folder must be a full path: {path}", path = dir),
            E::NoFile => lang.tr("The download has no file.").to_string(),
            E::NoFileName => lang.tr("The download has no file name.").to_string(),
            E::NoFolder => lang.tr("The download has no folder.").to_string(),
            E::Exists(path) => trf!(lang, "{path} already exists.", path = path),
            E::StartedWhileSaving => lang.tr("The download started while its properties were being saved.").to_string(),
            E::NotConvertible => {
                lang.tr("Only a finished download whose file is still there can be converted.").to_string()
            }
            E::NoFfmpeg => lang.tr("Converting needs ffmpeg. Install it, or choose it in Options > Tools.").to_string(),
            E::NotWaiting(id) => trf!(lang, "download {id} is not waiting to be started", id = id),
            E::AnswerNoFileName => lang.tr("the download has no file name").to_string(),
            E::AnswerNoFolder => lang.tr("the download has no folder").to_string(),
            E::AnswerRelativeFolder(dir) => trf!(lang, "the folder must be a full path: {path}", path = dir),
            E::AlreadyFinished => lang.tr("This download has already finished.").to_string(),
            E::Unreachable(why) => trf!(lang, "could not reach the new link: {why}", why = why),
            // How moving the file failed, in the system's words.
            E::Files(why) => why.clone(),
            E::FilesStay { url, reason } => {
                trf!(lang, "Removed {url} from the list, but its files stay: {reason}", url = url, reason = reason)
            }
            E::Torrent(e) => torrent(lang, e),
        }
    }

    /// Why a torrent or magnet link cannot be used.
    pub fn torrent(lang: &'static Language, e: &udm_domain::torrent::TorrentError) -> String {
        use udm_domain::torrent::TorrentError as T;
        match e {
            T::NotMagnet(link) => trf!(lang, "{link} is not a magnet link", link = link),
            T::NoInfoHash => lang.tr("The magnet link names no BitTorrent info hash (xt=urn:btih:...).").to_string(),
            T::BadInfoHash(hash) => trf!(lang, "{hash} is not a BitTorrent info hash", hash = hash),
            T::NoFiles => lang.tr("The torrent holds no files.").to_string(),
            T::TooManyFiles { count, max } => {
                trf!(lang, "The torrent holds {count} files; UDM takes at most {max}.", count = count, max = max)
            }
            T::UnsafePath(path) => trf!(lang, "The torrent names a file UDM cannot save safely: {path}", path = path),
            T::TooBig { max } => trf!(
                lang,
                "The torrent file is larger than {size} and was not read.",
                size = udm_domain::bytes::ByteCount::new(*max as u64).megabytes()
            ),
            T::BadTorrent(why) => trf!(lang, "This is not a torrent file: {why}", why = why),
            T::NothingSelected => lang.tr("Choose at least one of the torrent's files.").to_string(),
            T::NoSuchFile(n) => trf!(lang, "The torrent has no file number {number}.", number = n),
            T::BadRatio(value) => trf!(lang, "{value} is not a share ratio between 0 and 100.", value = value),
            T::FileListTooBig { max } => trf!(
                lang,
                "The torrent's file names take more than {size}; UDM takes shorter lists.",
                size = udm_domain::bytes::ByteCount::new(*max as u64).megabytes()
            ),
            T::NameFixed(name) => {
                trf!(lang, "A torrent of one file keeps its file's name, {name}; choose only its folder.", name = name)
            }
        }
    }

    /// Looking for or installing an update failed.
    fn update(lang: &'static Language, e: &UpdateError) -> String {
        match e {
            UpdateError::Unreachable(why) => trf!(lang, "Could not reach GitHub: {why}", why = why),
            // How GitHub's answer or the Setup program was wrong.
            UpdateError::Answer(why) | UpdateError::BadSetup(why) => why.clone(),
            UpdateError::NewestAlready => lang.tr("This is the newest version.").to_string(),
            UpdateError::NoSetup => {
                lang.tr("This release has no Setup program; download it from its page.").to_string()
            }
            UpdateError::Download(why) => trf!(lang, "Could not download the update: {why}", why = why),
            UpdateError::Save { path, reason } => {
                trf!(lang, "Could not save the update to {path}: {reason}", path = path, reason = reason)
            }
            UpdateError::Start { path, reason } => {
                trf!(lang, "Could not start {path}: {reason}", path = path, reason = reason)
            }
        }
    }

    /// Why a theme file gives no theme.
    pub fn theme_file(lang: &'static Language, e: &ThemeFileError) -> String {
        match e {
            ThemeFileError::Unreadable => lang.tr("The file cannot be read.").to_string(),
            ThemeFileError::NotASetting { line } => {
                trf!(lang, "Line {line} is not a setting = value line.", line = line)
            }
            ThemeFileError::Invalid { line, error } => {
                trf!(lang, "Line {line}: {error}", line = line, error = theme(lang, error))
            }
            ThemeFileError::NoName => lang.tr("The theme has no name; add a name = line.").to_string(),
            ThemeFileError::NameTaken(name) => {
                trf!(lang, "Another theme file already has the name {name}.", name = name)
            }
        }
    }

    fn theme(lang: &'static Language, e: &InvalidTheme) -> String {
        match e {
            InvalidTheme::UnknownKey(key) => trf!(lang, "{key} is not a theme setting.", key = key),
            InvalidTheme::BadColor(text) => trf!(lang, "{value} is not a color; write it as #rrggbb.", value = text),
            InvalidTheme::BadSwitch(text) => trf!(lang, "{value} is not yes or no.", value = text),
            InvalidTheme::BadBase(text) => {
                trf!(lang, "{value} is not a built-in theme; use light, dark or high_contrast.", value = text)
            }
            InvalidTheme::BadSize { key, lo, hi } => {
                trf!(lang, "{key} must be a whole number from {lo} to {hi}.", key = key, lo = lo, hi = hi)
            }
            InvalidTheme::BadName(_) => lang.tr("A theme's name must be 1 to 40 characters on one line.").to_string(),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use udm_domain::bytes::ByteCount;
        use udm_domain::id::{DownloadId, QueueId};
        use udm_domain::ports::ControlError;

        /// In English each translation reads as the error's own message.
        #[test]
        fn english_matches_the_messages() {
            let lang = crate::i18n::resolve("en", &[]);
            let batch_errors = [
                BatchError::NumberTooBig("1-99".into()),
                BatchError::PatternTooBig(10_100),
                BatchError::NotWholeNumber(Bound::From),
                BatchError::WildcardSize,
                BatchError::NumbersReversed(10, 1),
                BatchError::LettersReversed('c', 'a'),
                BatchError::RangeTooBig("0".into(), "10000".into(), 10_001),
                BatchError::ListTooLong(10_001),
            ];
            for e in &batch_errors {
                assert_eq!(batch(lang, e), e.to_string());
            }
            let settings_errors = [
                InvalidSettings::TextSize,
                InvalidSettings::ServerConnections("a.com".into()),
                InvalidSettings::FolderNotFull(SettingsField::CategoryFolder("Music".into()), "rel".into()),
                InvalidSettings::BadFileType(SettingsField::Category(GENERAL.into()), "Zip".into()),
                InvalidSettings::GeneralName,
                InvalidSettings::ProxyNotHostPort(ProxyEntry::Socks),
                InvalidSettings::NotLoginSite("a b".into()),
                InvalidSettings::Scanner(CommandError::NoPath),
                InvalidSettings::RemotePort,
                InvalidSettings::RemoteAddress("pc.local".into()),
            ];
            for e in &settings_errors {
                assert_eq!(settings(lang, e), e.to_string());
            }
            for e in [InvalidQueue::AtOnce, InvalidQueue::Retries, InvalidQueue::StartTime("25:00".into())] {
                assert_eq!(queue(lang, &e), e.to_string());
            }
            let theme_errors = [
                ThemeFileError::Unreadable,
                ThemeFileError::NotASetting { line: 2 },
                ThemeFileError::NoName,
                ThemeFileError::NameTaken("Ocean".into()),
                ThemeFileError::Invalid { line: 3, error: InvalidTheme::UnknownKey("colour".into()) },
                ThemeFileError::Invalid { line: 4, error: InvalidTheme::BadColor("blue".into()) },
                ThemeFileError::Invalid { line: 5, error: InvalidTheme::BadSwitch("maybe".into()) },
                ThemeFileError::Invalid { line: 6, error: InvalidTheme::BadBase("sepia".into()) },
                ThemeFileError::Invalid {
                    line: 7,
                    error: InvalidTheme::BadSize { key: "text_size".into(), lo: 9, hi: 24 },
                },
                ThemeFileError::Invalid { line: 8, error: InvalidTheme::BadName(String::new()) },
            ];
            for e in &theme_errors {
                assert_eq!(theme_file(lang, e), e.to_string());
            }
            for e in [InvalidGrab::Depth, InvalidGrab::Delay, InvalidGrab::Size, InvalidGrab::FolderNotFull("x".into())]
            {
                assert_eq!(grab(lang, &e), e.to_string());
            }
        }

        /// The daemon's errors read in English as the daemon's own text, which
        /// the clients that do not translate show.
        #[test]
        fn daemon_errors_match_their_messages() {
            let lang = crate::i18n::resolve("en", &[]);
            let failures: Vec<Failure> = vec![
                Failure::InvalidMessage("expected value".into()),
                Failure::Probe("HTTP 404".into()),
                Failure::Other("from an old daemon".into()),
                Mismatch::UnknownSize.into(),
                Mismatch::Size { new: ByteCount::new(3 << 20), partial: ByteCount::new(1 << 20) }.into(),
                Mismatch::Changed.into(),
                Mismatch::NotResumable.into(),
                SettingsError::Invalid(InvalidSettings::TextSize).into(),
                ServiceError::CrawlRunning.into(),
                ServiceError::BadBody("http://a/".into()).into(),
                ServiceError::NotDownloadAddress("file:///x".into()).into(),
                ServiceError::Grab(InvalidGrab::Depth).into(),
                ServiceError::Confirm(ConfirmError::Download(DownloadError::AnswerNoFolder)).into(),
                ServiceError::Confirm(ConfirmError::Start(StartOrWaitError::Queue(QueueError::NoQueue(QueueId::new(
                    4,
                )))))
                .into(),
                ServiceError::Confirm(ConfirmError::Start(StartOrWaitError::Start(ControlError("busy".into())))).into(),
                QueueError::NameTaken("Night".into()).into(),
                QueueError::Invalid(InvalidQueue::NoName).into(),
                QueueError::NotQueued.into(),
                DownloadError::NoDownload(DownloadId::new(3)).into(),
                DownloadError::StillRunning(DownloadId::new(3)).into(),
                DownloadError::BadSpeedLimit { kbps: 1, lo: 2, hi: 3 }.into(),
                DownloadError::NotDownloadLink("javascript:".into()).into(),
                DownloadError::Exists("/d/f.zip".into()).into(),
                DownloadError::Unreachable("refused".into()).into(),
                DownloadError::Files("disk full".into()).into(),
                DownloadError::FilesStay { url: "http://a/f".into(), reason: "in use".into() }.into(),
                UpdateError::Unreachable("offline".into()).into(),
                UpdateError::Answer("no tag".into()).into(),
                UpdateError::NewestAlready.into(),
                UpdateError::Save { path: "/t/s.exe".into(), reason: "denied".into() }.into(),
                UpdateError::Start { path: "/t/s.exe".into(), reason: "denied".into() }.into(),
                ServiceError::Pairing(PairingError::WrongCode).into(),
                ServiceError::Pairing(PairingError::TooManyTries { wait: 30 }).into(),
                ServiceError::Pairing(PairingError::NotPaired).into(),
                ServiceError::Pairing(PairingError::TooManyDevices).into(),
                ServiceError::Pairing(PairingError::NoDevice(udm_domain::id::DeviceId::new(4))).into(),
                ServiceError::Pairing(PairingError::NotListening).into(),
                ServiceError::Pairing(PairingError::Store("disk full".into())).into(),
            ];
            for e in &failures {
                assert_eq!(daemon(lang, e), e.to_string(), "{e:?}");
            }
            assert_eq!(daemon(lang, &Failure::from(QueueError::MainQueue)), "The main queue cannot be deleted.");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::resolve;

    #[test]
    fn a_bad_address_names_itself() {
        let e = address(resolve("en", &[]), &AddressError::NotDownload("mailto:x".into()));
        assert_eq!(e.kind, ErrorKind::Invalid);
        assert_eq!(e.message, "mailto:x is not an http, https or ftp address.");
    }
}
