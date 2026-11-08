//! Downloads and notifications pages start: kept here, newest first, and
//! shown in the sidebar's Activity until closed. A notification arriving
//! while Gate's window isn't the active one also goes to the system's
//! notifications.

use crate::url;
use crate::workspace::Workspace;
use gpui::{Context, SystemNotification, Window};
use layout::PaneId;
use web::{Download, DownloadState, Notification};

pub struct DownloadItem {
    pub pane: PaneId,
    pub download: Download,
}

pub struct NoteItem {
    pub pane: PaneId,
    pub note: Notification,
}

/// A notification's system-notification tag, and back.
fn tag(pane: PaneId, id: u64) -> String {
    format!("page-{pane}-{id}")
}

fn untag(tag: &str) -> Option<(PaneId, u64)> {
    let (pane, id) = tag.strip_prefix("page-")?.split_once('-')?;
    Some((pane.parse().ok()?, id.parse().ok()?))
}

impl Workspace {
    /// A system notification clicked: its page comes forward.
    pub fn on_system_notification(window: &mut Window, cx: &mut Context<Self>) {
        let workspace = cx.entity().downgrade();
        let handle = window.window_handle();
        cx.on_system_notification_response(move |response, cx| {
            let Some((pane, id)) = untag(&response.tag) else { return };
            let _ = handle.update(cx, |_, window, cx| {
                window.activate_window();
                let _ = workspace.update(cx, |ws, cx| {
                    if let Some(index) = ws.notes.iter().position(|n| n.pane == pane && n.note.id == id) {
                        ws.open_note(index, window, cx);
                    }
                });
            });
        });
    }

    pub fn download_changed(&mut self, pane: PaneId, download: &Download, cx: &mut Context<Self>) {
        match self.downloads.iter_mut().find(|d| d.pane == pane && d.download.id == download.id) {
            Some(item) => item.download = download.clone(),
            None => self.downloads.insert(0, DownloadItem { pane, download: download.clone() }),
        }
        cx.notify();
    }

    /// Opens a finished download.
    pub fn open_download(&self, index: usize, cx: &mut Context<Self>) {
        if let Some(item) = self.downloads.get(index).filter(|d| d.download.state == DownloadState::Done) {
            cx.open_with_system(&item.download.path);
        }
    }

    /// Shows a finished download in its folder.
    pub fn show_download(&self, index: usize, cx: &mut Context<Self>) {
        if let Some(item) = self.downloads.get(index) {
            cx.reveal_path(&item.download.path);
        }
    }

    /// Stops a running download (its entry then says so), or closes the
    /// entry of one that ended.
    pub fn close_download(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(item) = self.downloads.get(index) else { return };
        if item.download.state == DownloadState::Running {
            if let Some(p) = self.panes.get(&item.pane) {
                let id = item.download.id;
                p.view.update(cx, |v, _| v.cancel_download(id));
            }
        } else {
            self.downloads.remove(index);
        }
        cx.notify();
    }

    pub fn note_shown(&mut self, pane: PaneId, note: &Notification, window: &Window, cx: &mut Context<Self>) {
        match self.notes.iter_mut().find(|n| n.pane == pane && n.note.id == note.id) {
            Some(item) => item.note = note.clone(),
            None => self.notes.insert(0, NoteItem { pane, note: note.clone() }),
        }
        // Activity is out of sight while Gate's window isn't active.
        if !window.is_window_active() {
            let site = self.panes.get(&pane).map(|p| url::host(p.view.read(cx).url()).to_string()).unwrap_or_default();
            let body = match (note.body.is_empty(), site.is_empty()) {
                (_, true) => note.body.clone(),
                (true, false) => site,
                (false, false) => format!("{}\n{site}", note.body),
            };
            cx.show_system_notification(SystemNotification {
                tag: tag(pane, note.id).into(),
                title: note.title.clone().into(),
                body: body.into(),
                actions: Vec::new(),
            });
        }
        cx.notify();
    }

    /// The page took its notification back.
    pub fn note_closed(&mut self, pane: PaneId, id: u64, cx: &mut Context<Self>) {
        self.notes.retain(|n| !(n.pane == pane && n.note.id == id));
        cx.dismiss_system_notification(&tag(pane, id));
        cx.notify();
    }

    /// A notification clicked: its page comes forward and hears of it.
    pub fn open_note(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.notes.len() {
            return;
        }
        let item = self.notes.remove(index);
        cx.dismiss_system_notification(&tag(item.pane, item.note.id));
        if let Some(p) = self.panes.get(&item.pane) {
            p.view.update(cx, |v, _| v.notification_clicked(item.note.id));
        }
        self.reveal_pane(item.pane, window, cx);
    }

    /// A notification dismissed.
    pub fn close_note(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.notes.len() {
            return;
        }
        let item = self.notes.remove(index);
        cx.dismiss_system_notification(&tag(item.pane, item.note.id));
        if let Some(p) = self.panes.get(&item.pane) {
            p.view.update(cx, |v, _| v.notification_closed(item.note.id));
        }
        cx.notify();
    }

    /// A page went: its notifications with it; its downloads stop with
    /// the page.
    pub fn forget_page_notices(&mut self, pane: PaneId, cx: &mut Context<Self>) {
        for item in self.notes.iter().filter(|n| n.pane == pane) {
            cx.dismiss_system_notification(&tag(pane, item.note.id));
        }
        self.notes.retain(|n| n.pane != pane);
        for item in self.downloads.iter_mut().filter(|d| d.pane == pane && d.download.state == DownloadState::Running) {
            item.download.state = DownloadState::Cancelled;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_round_trip() {
        assert_eq!(untag(&tag(7, 42)), Some((7, 42)));
        assert_eq!(untag("something-else"), None);
    }
}
