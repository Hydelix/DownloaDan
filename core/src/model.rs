//! Data model. The field names and shapes match the old Electron version's `state.json`, so an existing
//! file (downloads, queues, settings, themes, pairing token) can be loaded as it is.

use crate::util;
use serde::{Deserialize, Serialize};

pub const MAIN_QUEUE: &str = "main";
pub const MAIN_NAME: &str = "Main";

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    #[default]
    Queued,
    Downloading,
    Completed,
    Error,
    /// Also what any unknown value in an old file becomes (safe: nothing starts by itself).
    #[serde(other)]
    Paused,
}

/// One connection's share of a file. `end` is -1 when the size is unknown.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Segment {
    pub start: i64,
    pub end: i64,
    pub pos: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Item {
    pub id: String,
    pub url: String,
    pub referer: Option<String>,
    pub cookie: Option<String>,
    pub dir: String,
    pub queue_id: String,
    pub status: Status,
    /// Started by the user (skips the queue's on/off switch and goes first).
    pub force: bool,
    /// Added with "don't start yet": waits until its queue is started.
    pub hold: bool,
    pub filename: Option<String>,
    pub size: i64,
    pub downloaded: i64,
    pub speed: i64,
    pub eta: Option<i64>,
    pub error: Option<String>,
    pub added_at: i64,
    pub completed_at: Option<i64>,
    pub final_url: Option<String>,
    pub accept_ranges: bool,
    pub part_path: Option<String>,
    pub segments: Option<Vec<Segment>>,
    pub note: Option<String>,
    pub auto_resume: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub max_simultaneous: i64,
    pub connections: i64,
    pub download_dir: String,
    pub minimize_to_tray: bool,
    pub run_at_startup: bool,
    pub notify: bool,
    pub remove_missing: bool,
    pub auto_update: bool,
    pub bypass_key: String,
    pub bypass_key_v: i64,
    pub bridge_port: i64,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            max_simultaneous: 3,
            connections: 8,
            download_dir: String::new(),
            minimize_to_tray: true,
            run_at_startup: false,
            notify: true,
            remove_missing: true,
            auto_update: true,
            bypass_key: "F8".into(),
            bypass_key_v: 3,
            bridge_port: 18642,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Queue {
    pub id: String,
    pub name: String,
    pub enabled: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Colors {
    pub bg: String,
    pub surface: String,
    pub surface2: String,
    pub border: String,
    pub text: String,
    pub text_muted: String,
    pub accent: String,
    pub accent_text: String,
    pub success: String,
    pub warning: String,
    pub danger: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Shape {
    pub radius: u32,
    pub density: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Theme {
    #[serde(default)]
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub author: Option<String>,
    pub colors: Colors,
    pub shape: Shape,
    pub font: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct ThemesState {
    pub active: String,
    pub custom: Vec<Theme>,
}

impl Default for ThemesState {
    fn default() -> Self {
        ThemesState { active: "dark".into(), custom: vec![] }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct State {
    pub settings: Settings,
    pub queues: Vec<Queue>,
    pub downloads: Vec<Item>,
    pub themes: ThemesState,
    pub token: String,
}

impl Default for State {
    fn default() -> Self {
        State {
            settings: Settings::default(),
            queues: vec![Queue { id: MAIN_QUEUE.into(), name: MAIN_NAME.into(), enabled: true }],
            downloads: vec![],
            themes: ThemesState::default(),
            token: util::new_token(),
        }
    }
}

/// What the window needs to know about a download (no cookies, no part-file internals).
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub id: String,
    pub url: String,
    pub filename: Option<String>,
    pub dir: String,
    pub queue_id: String,
    pub status: Status,
    pub size: i64,
    pub downloaded: i64,
    pub speed: i64,
    pub eta: Option<i64>,
    pub error: Option<String>,
    pub hold: bool,
    pub note: Option<String>,
    pub added_at: i64,
    pub completed_at: Option<i64>,
}

impl From<&Item> for Summary {
    fn from(d: &Item) -> Self {
        Summary {
            id: d.id.clone(),
            url: d.url.clone(),
            filename: d.filename.clone(),
            dir: d.dir.clone(),
            queue_id: d.queue_id.clone(),
            status: d.status,
            size: d.size.max(0),
            downloaded: d.downloaded.max(0),
            speed: d.speed.max(0),
            eta: d.eta,
            error: d.error.clone(),
            hold: d.hold,
            note: d.note.clone(),
            added_at: d.added_at,
            completed_at: d.completed_at,
        }
    }
}

#[derive(Serialize, Clone, Debug)]
pub struct ThemesSnapshot {
    pub active: String,
    pub custom: Vec<Theme>,
}

#[derive(Serialize, Clone, Debug)]
pub struct Snapshot {
    pub downloads: Vec<Summary>,
    pub queues: Vec<Queue>,
    pub settings: Settings,
    pub themes: ThemesSnapshot,
    pub token: String,
}
