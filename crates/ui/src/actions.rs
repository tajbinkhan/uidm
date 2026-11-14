//! What the toolbar's and menus' actions do to the selection: which state
//! messages and which daemon commands each one makes. A plain function of
//! the state, so it is tested without the daemon.

use udm_application::daemon::Command;
use udm_domain::download::{Answer, Status};
use udm_domain::id::QueueId;

use crate::contract::ListAction;
use crate::projection::main::{is_finished, is_running, is_startable};
use crate::state::{AppState, UiMessage};

/// The messages and commands an action makes.
#[derive(Debug, Default)]
pub struct Plan {
    pub messages: Vec<UiMessage>,
    pub commands: Vec<Command>,
}

pub fn plan(state: &AppState, action: ListAction) -> Plan {
    let selected = || state.selected.iter().filter_map(|id| state.table.get(*id));
    let mut p = Plan::default();
    match action {
        ListAction::Resume => {
            for e in selected().filter(|e| is_startable(e.status)) {
                p.commands.push(match e.status {
                    Status::Waiting => Command::Confirm(Answer::start(e.id)),
                    _ => Command::Retry { id: e.id },
                });
            }
        }
        ListAction::Stop => {
            p.commands.extend(selected().filter(|e| is_running(e.status)).map(|e| Command::Cancel { id: e.id }));
        }
        ListAction::StopAll => p.commands.push(Command::PauseAll),
        ListAction::ResumeAll => p.commands.push(Command::ResumeAll),
        ListAction::Delete => {
            let ask = state.settings.as_ref().is_none_or(|s| s.confirm_remove);
            if ask {
                p.messages.push(UiMessage::AskRemove);
            } else {
                p.commands.extend(selected().map(|e| Command::Remove { id: e.id, delete_files: false }));
            }
        }
        ListAction::DeleteCompleted => {
            p.commands.extend(
                state
                    .table
                    .entries()
                    .filter(|e| e.status == Status::Done)
                    .map(|e| Command::Remove { id: e.id, delete_files: false }),
            );
        }
        ListAction::Redownload => {
            p.commands.extend(selected().filter(|e| is_finished(e.status)).map(|e| Command::Redownload { id: e.id }));
        }
        ListAction::StartQueue { id } => p.commands.push(Command::StartQueue { id: QueueId::new(id.0) }),
        ListAction::StopQueue { id } => p.commands.push(Command::StopQueue { id: QueueId::new(id.0) }),
        ListAction::SelectAll => p.messages.push(UiMessage::SelectAll),
        ListAction::StopSeeding => {
            p.commands.extend(
                selected()
                    .filter(|e| crate::projection::torrent::is_seeding(e))
                    .map(|e| Command::StopSeeding { id: e.id }),
            );
        }
    }
    p
}

/// The commands that remove the downloads the Confirm deletion dialog
/// asked about.
pub fn remove(state: &AppState, delete_files: bool) -> Vec<Command> {
    state
        .to_remove
        .iter()
        .filter(|id| state.table.get(**id).is_some())
        .map(|id| Command::Remove { id: *id, delete_files })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::apply;
    use std::time::Instant;
    use udm_application::daemon::Event;
    use udm_domain::id::DownloadId;
    use udm_domain::settings::Settings;

    fn state(statuses: &[Status]) -> AppState {
        let items =
            statuses.iter().enumerate().map(|(i, s)| crate::testing::entry(i as u64 + 1, "f.zip", *s, 10, 0)).collect();
        let mut st = AppState::default();
        apply(&mut st, UiMessage::Daemon { event: Box::new(Event::Downloads { items }), at: Instant::now(), unix: 0 });
        let all = (1..=statuses.len() as u64).map(DownloadId::new).collect();
        apply(&mut st, UiMessage::Select(all));
        st
    }

    #[test]
    fn resume_starts_waiting_downloads_and_retries_stopped_ones() {
        let s = state(&[Status::Waiting, Status::Failed, Status::Done]);
        let p = plan(&s, ListAction::Resume);
        assert!(matches!(p.commands[..], [Command::Confirm(_), Command::Retry { .. }]));
    }

    #[test]
    fn stop_cancels_only_running_downloads() {
        let s = state(&[Status::Downloading, Status::Done, Status::Queued]);
        assert_eq!(plan(&s, ListAction::Stop).commands.len(), 2);
    }

    #[test]
    fn delete_asks_first_unless_the_setting_says_not_to() {
        let mut s = state(&[Status::Done]);
        let p = plan(&s, ListAction::Delete);
        assert!(matches!(p.messages[..], [UiMessage::AskRemove]) && p.commands.is_empty());
        let settings = Settings { confirm_remove: false, ..Settings::default() };
        apply(&mut s, UiMessage::Daemon { event: Box::new(Event::Settings { settings }), at: Instant::now(), unix: 0 });
        let p = plan(&s, ListAction::Delete);
        assert!(p.messages.is_empty() && p.commands.len() == 1);
    }

    #[test]
    fn stop_seeding_stops_the_seeding_torrents_only() {
        let mut s = state(&[Status::Done, Status::Done, Status::Downloading]);
        for (id, phase) in
            [(1, udm_domain::torrent::TorrentPhase::Seeding), (3, udm_domain::torrent::TorrentPhase::Seeding)]
        {
            let mut e = s.table.get(DownloadId::new(id)).cloned().unwrap();
            let mut t = udm_domain::torrent::TorrentEntry::new(udm_domain::torrent::InfoHash::from_bytes([1; 20]));
            t.phase = phase;
            e.torrent = Some(t);
            apply(
                &mut s,
                UiMessage::Daemon { event: Box::new(Event::Updated { item: e }), at: Instant::now(), unix: 0 },
            );
        }
        let p = plan(&s, ListAction::StopSeeding);
        assert!(matches!(p.commands[..], [Command::StopSeeding { id }] if id == DownloadId::new(1)));
    }

    #[test]
    fn delete_completed_removes_finished_downloads_only() {
        let s = state(&[Status::Done, Status::Failed, Status::Done]);
        assert_eq!(plan(&s, ListAction::DeleteCompleted).commands.len(), 2);
    }
}
