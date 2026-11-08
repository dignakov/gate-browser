//! The browser's own database (`gate.db` in the profile): every
//! visit ever (history is never trimmed), searchable as you type; site
//! icons, so restored spaces show theirs before loading; and the session.
//!
//! SQLite in WAL mode on the main thread: writes are a row or two, and
//! searches use a trigram full-text index, so any part of a title or
//! address matches (as Atuin finds commands) in well under a millisecond.

use rusqlite::{Connection, OptionalExtension as _, params};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct Db {
    conn: Connection,
}

/// A page from history.
#[derive(Clone, Debug, PartialEq)]
pub struct Page {
    pub url: String,
    pub title: String,
    pub visits: i64,
    /// Last visit, in Unix milliseconds.
    pub last: i64,
}

const SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS visits (
        id INTEGER PRIMARY KEY,
        url TEXT NOT NULL,
        at INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS visits_url ON visits(url);
    -- One row per address: what search ranks.
    CREATE TABLE IF NOT EXISTS pages (
        id INTEGER PRIMARY KEY,
        url TEXT NOT NULL UNIQUE,
        title TEXT NOT NULL DEFAULT '',
        visits INTEGER NOT NULL DEFAULT 0,
        last INTEGER NOT NULL DEFAULT 0
    );
    CREATE VIRTUAL TABLE IF NOT EXISTS pages_fts USING fts5(
        title, url, content='pages', content_rowid='id', tokenize='trigram'
    );
    CREATE TRIGGER IF NOT EXISTS pages_ai AFTER INSERT ON pages BEGIN
        INSERT INTO pages_fts(rowid, title, url) VALUES (new.id, new.title, new.url);
    END;
    CREATE TRIGGER IF NOT EXISTS pages_ad AFTER DELETE ON pages BEGIN
        INSERT INTO pages_fts(pages_fts, rowid, title, url) VALUES ('delete', old.id, old.title, old.url);
    END;
    CREATE TRIGGER IF NOT EXISTS pages_au AFTER UPDATE OF title, url ON pages BEGIN
        INSERT INTO pages_fts(pages_fts, rowid, title, url) VALUES ('delete', old.id, old.title, old.url);
        INSERT INTO pages_fts(rowid, title, url) VALUES (new.id, new.title, new.url);
    END;
    CREATE TABLE IF NOT EXISTS favicons (
        host TEXT PRIMARY KEY,
        png BLOB NOT NULL
    );
    CREATE TABLE IF NOT EXISTS state (
        key TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );
";

pub fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// Addresses worth remembering: web pages and files, not blank pages or
/// the browser's own.
pub fn remembered(url: &str) -> bool {
    url.starts_with("https://") || url.starts_with("http://") || url.starts_with("file://")
}

impl Db {
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    /// For tests: a database that lives in memory.
    #[cfg(test)]
    fn memory() -> Self {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        Self { conn }
    }

    /// A page was visited (navigated to).
    pub fn visit(&self, url: &str, title: &str) {
        if !remembered(url) {
            return;
        }
        let at = now_ms();
        let result = (|| {
            self.conn.execute("INSERT INTO visits(url, at) VALUES (?1, ?2)", params![url, at])?;
            self.conn.execute(
                "INSERT INTO pages(url, title, visits, last) VALUES (?1, ?2, 1, ?3)
                 ON CONFLICT(url) DO UPDATE SET visits = visits + 1, last = ?3,
                    title = CASE WHEN ?2 = '' THEN title ELSE ?2 END",
                params![url, title, at],
            )
        })();
        if let Err(error) = result {
            tracing::warn!("recording a visit: {error}");
        }
    }

    /// A page's title arrived (after its visit).
    pub fn set_title(&self, url: &str, title: &str) {
        if !remembered(url) || title.is_empty() {
            return;
        }
        if let Err(error) = self.conn.execute("UPDATE pages SET title = ?2 WHERE url = ?1 AND title != ?2", params![url, title]) {
            tracing::warn!("recording a title: {error}");
        }
    }

    /// Pages whose title or address contain every word of `query`, most
    /// visited and recent first (at most `limit`).
    pub fn search(&self, query: &str, limit: usize) -> Vec<Page> {
        let words: Vec<&str> = query.split_whitespace().collect();
        if words.is_empty() {
            return Vec::new();
        }
        // The trigram index needs three letters a word; shorter ones scan.
        let result = if words.iter().all(|w| w.chars().count() >= 3) {
            let fts: Vec<String> = words.iter().map(|w| format!("\"{}\"", w.replace('"', "\"\""))).collect();
            self.pages(
                "SELECT p.url, p.title, p.visits, p.last FROM pages_fts f JOIN pages p ON p.id = f.rowid
                 WHERE pages_fts MATCH ?1 ORDER BY p.last DESC LIMIT 400",
                &fts.join(" AND "),
            )
        } else {
            let like = format!("%{}%", query.trim().replace('%', "").replace('_', ""));
            self.pages(
                "SELECT url, title, visits, last FROM pages WHERE title LIKE ?1 OR url LIKE ?1 ORDER BY last DESC LIMIT 400",
                &like,
            )
        };
        let mut pages = result.unwrap_or_else(|error| {
            tracing::warn!("searching history: {error}");
            Vec::new()
        });
        let now = now_ms();
        pages.sort_by(|a, b| frecency(b, now).total_cmp(&frecency(a, now)));
        pages.truncate(limit);
        pages
    }

    /// The pages visited last, latest first.
    pub fn recent(&self, limit: usize) -> Vec<Page> {
        let mut pages = self.pages("SELECT url, title, visits, last FROM pages WHERE ?1 = ?1 ORDER BY last DESC LIMIT 400", "").unwrap_or_else(|error| {
            tracing::warn!("reading history: {error}");
            Vec::new()
        });
        pages.truncate(limit);
        pages
    }

    fn pages(&self, sql: &str, arg: &str) -> rusqlite::Result<Vec<Page>> {
        let mut statement = self.conn.prepare_cached(sql)?;
        let rows = statement.query_map([arg], |r| {
            Ok(Page { url: r.get(0)?, title: r.get(1)?, visits: r.get(2)?, last: r.get(3)? })
        })?;
        rows.collect()
    }

    pub fn favicon(&self, host: &str) -> Option<Vec<u8>> {
        self.conn.query_row("SELECT png FROM favicons WHERE host = ?1", [host], |r| r.get(0)).optional().ok().flatten()
    }

    pub fn set_favicon(&self, host: &str, png: &[u8]) {
        if host.is_empty() {
            return;
        }
        let result = self.conn.execute(
            "INSERT INTO favicons(host, png) VALUES (?1, ?2) ON CONFLICT(host) DO UPDATE SET png = ?2",
            params![host, png],
        );
        if let Err(error) = result {
            tracing::warn!("saving a favicon: {error}");
        }
    }

    pub fn get_state(&self, key: &str) -> Option<String> {
        self.conn.query_row("SELECT value FROM state WHERE key = ?1", [key], |r| r.get(0)).optional().ok().flatten()
    }

    pub fn set_state(&self, key: &str, value: &str) {
        let result = self.conn.execute(
            "INSERT INTO state(key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = ?2",
            params![key, value],
        );
        if let Err(error) = result {
            tracing::warn!("saving {key}: {error}");
        }
    }
}

/// Visited often and lately ranks first: visits count less as they age
/// (halving every two weeks or so), as Firefox's frecency does.
fn frecency(page: &Page, now: i64) -> f64 {
    let days = ((now - page.last).max(0) as f64) / 86_400_000.0;
    (1.0 + page.visits as f64).ln() * 0.5f64.powf(days / 14.0) + 1.0 / (1.0 + days)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_any_part_of_title_or_address() {
        let db = Db::memory();
        db.visit("https://app.slack.com/client/T1", "Slack | general");
        db.visit("https://mail.google.com/mail/u/0/", "");
        db.set_title("https://mail.google.com/mail/u/0/", "Inbox (3) - Gmail");
        db.visit("about:blank", "ignored");

        assert_eq!(db.search("slack", 10)[0].url, "https://app.slack.com/client/T1");
        assert_eq!(db.search("gmail", 10)[0].title, "Inbox (3) - Gmail");
        // Middle of a word, and short words (no index).
        assert_eq!(db.search("oogle", 10).len(), 1);
        assert_eq!(db.search("sl", 10).len(), 1);
        // Every word must match.
        assert!(db.search("slack inbox", 10).is_empty());
        assert!(db.search("blank", 10).is_empty());
    }

    #[test]
    fn visits_count_and_state_round_trips() {
        let db = Db::memory();
        db.visit("https://a.example/", "A");
        db.visit("https://a.example/", "");
        let page = &db.search("a.example", 1)[0];
        assert_eq!((page.visits, page.title.as_str()), (2, "A"));
        db.set_state("session", "{}");
        assert_eq!(db.get_state("session").as_deref(), Some("{}"));
    }
}
