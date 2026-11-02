//! The sidebar's Activity list: pages that want the user's attention, found
//! from what the pages report (questions, camera/microphone, media,
//! unread counts).

use crate::workspace::Workspace;
use gpui::App;
use layout::PaneId;

/// Why a page is listed, most urgent first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// A question waits (dialog, permission) or the page crashed.
    NeedsYou,
    /// Using the camera or microphone.
    Live,
    /// Media that played: playing now, or paused.
    Media,
    /// Shows an unread count in its title.
    Unread,
}

impl Kind {
    pub const ALL: [Kind; 4] = [Kind::NeedsYou, Kind::Live, Kind::Media, Kind::Unread];

    pub fn label(self) -> &'static str {
        match self {
            Kind::NeedsYou => "Needs you",
            Kind::Live => "Live",
            Kind::Media => "Media",
            Kind::Unread => "Unread",
        }
    }
}

pub struct Item {
    pub pane: PaneId,
    pub kind: Kind,
    /// Unread count, for [`Kind::Unread`].
    pub count: Option<u32>,
    pub camera: bool,
    pub microphone: bool,
}

/// Every page wanting attention, in sidebar order, each under its most
/// urgent reason.
pub fn items(workspace: &Workspace, cx: &App) -> Vec<Item> {
    let mut out = Vec::new();
    for tab in &workspace.tabs {
        for pane in tab.root.panes() {
            let Some(p) = workspace.panes.get(&pane) else { continue };
            let view = p.view.read(cx);
            let (camera, microphone) = view.media();
            let count = view.unread();
            let kind = if view.ask().is_some() || view.crashed() {
                Kind::NeedsYou
            } else if camera || microphone {
                Kind::Live
            } else if view.playback() != web::Playback::None {
                Kind::Media
            } else if count.is_some() {
                Kind::Unread
            } else {
                continue;
            };
            out.push(Item { pane, kind, count, camera, microphone });
        }
    }
    out
}
