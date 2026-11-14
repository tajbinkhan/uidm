use std::path::Path;

use udm_domain::download::{Answer, Download, DownloadError, DuplicateStep};
use udm_domain::id::DownloadId;
use udm_domain::ports::{
    Attention, DownloadControl, DownloadFiles, DownloadList, EarlyStarts, QueueStore, SettingsError, SettingsStore,
};
use udm_domain::queue::{self, MAIN_QUEUE};
use udm_domain::settings::Duplicate;

use super::{Existing, RemoveDownload, StartOrWait, StartOrWaitError, UseExisting};

/// What [`ConfirmAnswer`] did.
#[derive(Debug, PartialEq, Eq)]
pub enum Confirmed {
    /// The download duplicated download `old`, which the user chose to use
    /// instead (as `kept`); the new one left the list.
    UsedExisting { old: DownloadId, kept: DownloadId },
    /// It had started early; that copy carries on as the download.
    HandedOver,
    /// It started, or waits in a queue.
    Settled,
}

/// Something that went wrong on the side without stopping the answer.
#[derive(Debug, PartialEq, Eq)]
pub enum ConfirmWarning {
    /// The duplicate choice was not kept as the setting.
    DuplicateNotRemembered(SettingsError),
    /// The dialog's "remember" boxes were not kept in the settings.
    ChoicesNotRemembered(SettingsError),
    /// The download the new one replaces stayed in the list.
    OldNotRemoved(DownloadId, DownloadError),
    /// The download the user chose to use instead did not resume.
    OldNotResumed(DownloadId, DownloadError),
}

/// Why [`ConfirmAnswer`] refused the answer.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfirmError {
    #[error(transparent)]
    Download(#[from] DownloadError),
    #[error(transparent)]
    Start(#[from] StartOrWaitError),
}

/// Take the save-as dialog's answer for a download waiting in it: settle a
/// duplicate first, then keep the file name, folder, category and
/// description chosen, remember what the dialog's boxes asked, and start the
/// download or have it wait in a queue.
pub struct ConfirmAnswer<'a, Q, D, X, F, A, S, E> {
    queues: &'a Q,
    downloads: &'a D,
    control: &'a X,
    files: &'a F,
    attention: &'a A,
    settings: &'a S,
    early: &'a E,
}

impl<'a, Q, D, X, F, A, S, E> ConfirmAnswer<'a, Q, D, X, F, A, S, E>
where
    Q: QueueStore,
    D: DownloadList,
    X: DownloadControl,
    F: DownloadFiles,
    A: Attention,
    S: SettingsStore,
    E: EarlyStarts,
{
    #[allow(clippy::too_many_arguments)] // One per port; a struct of them would only rename the list.
    pub fn new(
        queues: &'a Q,
        downloads: &'a D,
        control: &'a X,
        files: &'a F,
        attention: &'a A,
        settings: &'a S,
        early: &'a E,
    ) -> Self {
        ConfirmAnswer { queues, downloads, control, files, attention, settings, early }
    }

    pub fn run(&self, answer: Answer) -> Result<(Confirmed, Vec<ConfirmWarning>), ConfirmError> {
        let Answer {
            id,
            mut filename,
            mut folder,
            start,
            queue,
            mut file_exists,
            duplicate,
            category,
            description,
            remember_folder,
            remember_duplicate,
            remember_queue,
            elevate,
            files,
        } = answer;
        let mut warnings = Vec::new();
        let category = category.map(|c| c.trim().to_string()).filter(|c| !c.is_empty());
        let description = description.map(|d| d.trim().to_string());

        let other = self.downloads.entry(id).and_then(|e| e.duplicate_of).and_then(|o| self.downloads.entry(o));
        if let Some(old) = other {
            let choice = duplicate.unwrap_or(Duplicate::NewName);
            if remember_duplicate {
                let mut s = self.settings.settings();
                if s.if_duplicate != choice {
                    s.if_duplicate = choice;
                    if let Err(e) = self.settings.save_settings(s) {
                        warnings.push(ConfirmWarning::DuplicateNotRemembered(e));
                    }
                }
            }
            let mut download = self.downloads.request(id).unwrap_or_default();
            let kept = match download.settle_duplicate(&old, choice) {
                DuplicateStep::Add => None,
                DuplicateStep::Replace => {
                    let remove = RemoveDownload::new(self.queues, self.downloads, self.control, self.files);
                    if let Err(e) = remove.run(old.id, false) {
                        warnings.push(ConfirmWarning::OldNotRemoved(old.id, e));
                    }
                    None
                }
                DuplicateStep::UseExisting => {
                    let existing =
                        UseExisting::new(self.queues, self.downloads, self.control, self.files, self.attention);
                    match existing.run(&old) {
                        Existing::Gone => None,
                        Existing::Shown { id, .. } | Existing::Resumed { id, .. } => Some(id),
                        Existing::NotResumed { id, error, .. } => {
                            warnings.push(ConfirmWarning::OldNotResumed(id, error));
                            Some(id)
                        }
                    }
                }
            };
            if let Some(kept) = kept {
                // The new one never ran: it just leaves the list.
                self.control.forget(id);
                if self.queues.edit(|qs| queue::leave(qs, id)) {
                    self.queues.queues_changed();
                }
                return Ok((Confirmed::UsedExisting { old: old.id, kept }, warnings));
            }
            // Replacing the other download means writing over its file.
            if choice == Duplicate::Overwrite && old.path.is_some() {
                filename = download.filename.clone();
                folder = download.folder.clone();
            }
            file_exists = file_exists.or(download.file_exists);
        }

        // A torrent's files are chosen first, so a choice it cannot take
        // leaves the download waiting in its dialog.
        if let Some(files) = &files {
            let mut picked = Ok(());
            self.downloads.update(id, |e| {
                picked = e.select_torrent_files(files.clone());
                picked.is_ok()
            });
            picked.map_err(DownloadError::Torrent)?;
        }
        let mut applied = Ok(());
        let updated = self.downloads.update(id, |e| {
            applied =
                e.apply_answer(filename.as_deref(), folder.as_deref(), category.as_deref(), description.as_deref());
            applied.is_ok()
        });
        updated.ok_or(DownloadError::NoDownload(id))?;
        applied?;
        let item = self.downloads.entry(id).ok_or(DownloadError::NoDownload(id))?;

        let mut s = self.settings.settings();
        let picked = folder.as_deref().is_some_and(|f| !f.trim().is_empty());
        let later = (remember_queue && !start).then(|| queue.unwrap_or(MAIN_QUEUE));
        let path = item.path.as_deref().map(Path::new);
        if s.remember_answer(path, item.url.as_str(), item.category.as_deref(), remember_folder, picked, later) {
            if let Err(e) = self.settings.save_settings(s) {
                warnings.push(ConfirmWarning::ChoicesNotRemembered(e));
            }
        }

        if file_exists.is_some() || category.is_some() || description.is_some() || elevate || files.is_some() {
            let mut request =
                self.downloads.request(id).unwrap_or_else(|| Download { url: item.url.clone(), ..Download::default() });
            let elevate_into = path.and_then(Path::parent).filter(|_| elevate);
            let new_description = description.is_some().then_some(item.description.as_deref());
            request.apply_answer(file_exists, category.as_deref(), new_description, elevate_into);
            if elevate && elevate_into.is_none() {
                request.elevate = true;
            }
            if files.is_some() {
                request.torrent_files = item.torrent.as_ref().and_then(|t| t.selection.clone());
            }
            self.downloads.keep_request(id, request);
        }

        // A download started early stops first, so its bytes can move to the
        // file chosen.
        if self.early.hand_over(id, start, queue) {
            return Ok((Confirmed::HandedOver, warnings));
        }
        let later_queue = self.settings.settings().later_queue;
        StartOrWait::new(self.queues, self.downloads, self.control).run(id, start, queue, later_queue)?;
        Ok((Confirmed::Settled, warnings))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{row, Control, Downloads, Early, Files, Prefs, Queues, Shown};
    use udm_domain::download::{DownloadEntry, Status};
    use udm_domain::settings::{FileExists, Settings};

    struct World {
        queues: Queues,
        downloads: Downloads,
        control: Control,
        files: Files,
        shown: Shown,
        prefs: Prefs,
        early: Early,
    }

    impl World {
        fn with(rows: Vec<DownloadEntry>) -> Self {
            World {
                queues: Queues::main(),
                downloads: Downloads::rows(rows),
                control: Control::default(),
                files: Files::default(),
                shown: Shown::default(),
                prefs: Prefs::with(Settings::default()),
                early: Early::default(),
            }
        }

        fn confirm(&self, answer: Answer) -> Result<(Confirmed, Vec<ConfirmWarning>), ConfirmError> {
            let w = self;
            ConfirmAnswer::new(&w.queues, &w.downloads, &w.control, &w.files, &w.shown, &w.prefs, &w.early).run(answer)
        }
    }

    fn asking(id: u64) -> DownloadEntry {
        let mut e = row(id, Status::Confirming);
        e.path = Some(Path::new("/dl").join(format!("{id}.zip")).display().to_string());
        e
    }

    #[test]
    fn starting_keeps_the_choices_and_hands_it_to_the_engine() {
        let w = World::with(vec![asking(1)]);
        let answer = Answer {
            category: Some(" Music ".into()),
            file_exists: Some(FileExists::Overwrite),
            ..Answer::start(DownloadId::new(1))
        };
        let (done, warnings) = w.confirm(answer).unwrap();
        assert_eq!((done, warnings), (Confirmed::Settled, vec![]));
        assert_eq!(w.control.dispatched(), [1]);
        assert_eq!(w.downloads.entry(DownloadId::new(1)).unwrap().category.as_deref(), Some("Music"));
        let req = w.downloads.request(DownloadId::new(1)).unwrap();
        assert_eq!((req.file_exists, req.category.as_deref()), (Some(FileExists::Overwrite), Some("Music")));
    }

    #[test]
    fn download_later_waits_and_can_remember_the_queue() {
        let w = World::with(vec![asking(1)]);
        let answer = Answer { start: false, remember_queue: true, ..Answer::start(DownloadId::new(1)) };
        w.confirm(answer).unwrap();
        assert_eq!(w.queues.get(MAIN_QUEUE).order, [DownloadId::new(1)]);
        assert_eq!(w.prefs.settings().later_queue, Some(MAIN_QUEUE));
        assert_eq!(w.prefs.saves(), 1, "the settings are saved once");
        assert!(w.control.dispatched().is_empty());
    }

    #[test]
    fn an_early_start_carries_on() {
        let w = World { early: Early::started(&[1]), ..World::with(vec![asking(1)]) };
        assert_eq!(w.confirm(Answer::start(DownloadId::new(1))).unwrap().0, Confirmed::HandedOver);
        assert_eq!(w.early.handed(), [(1, true, None)]);
        assert!(w.control.dispatched().is_empty());
    }

    #[test]
    fn a_duplicate_can_use_the_older_download_instead() {
        let mut new = asking(2);
        new.duplicate_of = Some(DownloadId::new(1));
        let w = World::with(vec![row(1, Status::Failed), new]);
        let answer = Answer {
            duplicate: Some(Duplicate::Existing),
            remember_duplicate: true,
            ..Answer::start(DownloadId::new(2))
        };
        let (done, _) = w.confirm(answer).unwrap();
        assert!(matches!(done, Confirmed::UsedExisting { old, .. } if old == DownloadId::new(1)));
        assert_eq!(w.control.forgotten(), [2], "the new one leaves the list");
        assert_eq!(w.control.restarted().len(), 1, "the old one resumes");
        assert_eq!(w.prefs.settings().if_duplicate, Duplicate::Existing);
    }

    #[test]
    fn an_answer_for_a_download_not_waiting_is_refused() {
        let w = World::with(vec![row(1, Status::Downloading)]);
        let err = w.confirm(Answer::start(DownloadId::new(1))).unwrap_err();
        assert_eq!(err, ConfirmError::Download(DownloadError::NotWaiting(DownloadId::new(1))));
        assert_eq!(
            w.confirm(Answer::start(DownloadId::new(9))).unwrap_err(),
            ConfirmError::Download(DownloadError::NoDownload(DownloadId::new(9)))
        );
    }
}
