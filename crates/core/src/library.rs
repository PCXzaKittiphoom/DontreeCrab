//! DontreeCrab's own song library: a small SQLite index built by scanning
//! the folder roots configured in Settings.
//!
//! The schema is native to this project. It does not depend on dBase, DBF or
//! the CDX index files used by older karaoke software.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};

/// Schema revision written to `meta` and `PRAGMA user_version`.
///
/// 1. Base library tables.
/// 2. FTS5 lyric index (`songs_fts`) kept in sync by triggers.
/// 3. Case-insensitive `code` index so browsing does not sort the table.
/// 4. Search covers code, file name, title and artist only; the lyric text is
///    no longer stored or indexed.
const SCHEMA_VERSION: i64 = 4;

use crate::meter::MeterMap;
use crate::model::{EventKind, Song, SourceFormat};
use crate::{LoadOptions, load};

/// One row in the song library table.
#[derive(Debug, Clone)]
pub struct SongRecord {
    pub code: String,
    pub format: String,
    pub path: PathBuf,
    pub file_name: String,
    pub title: String,
    pub artist: String,
    pub key: Option<String>,
    pub tempo: Option<f64>,
    pub bars: Option<u32>,
    pub duration_ms: u64,
    pub language: Option<String>,
    pub vocal_channel: Option<u8>,
    pub year: Option<i32>,
    pub creator: Option<String>,
    pub lyr_preview: Option<String>,
    pub file_size: u64,
    pub modified_at: i64,
    pub missing: bool,
}

/// One song whose lyrics match the search, with the matching line.
#[derive(Debug, Clone)]
pub struct LyricHit {
    pub code: String,
    pub format: String,
    pub path: PathBuf,
    pub title: String,
    pub artist: String,
    pub key: Option<String>,
    pub tempo: Option<f64>,
    pub bars: Option<u32>,
    /// About five lyric lines joined into one row for the search result.
    pub preview: String,
}

/// Result of scanning the configured folder roots.
#[derive(Debug, Default)]
pub struct ScanOutcome {
    pub songs: Vec<SongRecord>,
    pub skipped: usize,
    pub duplicates: usize,
    /// Files whose stored row was reused because size and mtime matched.
    pub unchanged: usize,
    pub errors: Vec<String>,
}

/// Open (and create if needed) the per-user SQLite library.
pub struct Library {
    conn: Connection,
    /// Database file, or `None` for an in-memory library.
    path: Option<PathBuf>,
    /// True when the FTS5 lyric index exists and can answer substring queries.
    fts: bool,
}

impl Library {
    /// `%APPDATA%\DontreeCrab\DontreeCrab\library.sqlite` on Windows.
    pub fn default_path() -> Option<PathBuf> {
        directories::ProjectDirs::from("dev", "DontreeCrab", "DontreeCrab")
            .map(|dirs| dirs.config_dir().join("library.sqlite"))
    }

    pub fn open_default() -> Result<Self> {
        let path = Self::default_path()
            .ok_or_else(|| anyhow::anyhow!("no per-user config directory available"))?;
        Self::open(&path)
    }

    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("opening library {}", path.display()))?;
        let mut library = Self {
            conn,
            path: Some(path.to_path_buf()),
            fts: false,
        };
        library.init_schema()?;
        Ok(library)
    }

    #[cfg(test)]
    fn open_in_memory() -> Result<Self> {
        let mut library = Self {
            conn: Connection::open_in_memory()?,
            path: None,
            fts: false,
        };
        library.init_schema()?;
        Ok(library)
    }

    fn init_schema(&mut self) -> Result<()> {
        self.conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA busy_timeout = 5000;
             CREATE TABLE IF NOT EXISTS meta (
                 key TEXT PRIMARY KEY,
                 value TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS songs (
                 id INTEGER PRIMARY KEY,
                 code TEXT NOT NULL,
                 format TEXT NOT NULL,
                 path TEXT NOT NULL,
                 file_name TEXT NOT NULL,
                 title TEXT NOT NULL DEFAULT '',
                 artist TEXT NOT NULL DEFAULT '',
                 key TEXT,
                 tempo REAL,
                 bars INTEGER,
                 duration_ms INTEGER NOT NULL DEFAULT 0,
                 language TEXT,
                 vocal_channel INTEGER,
                 year INTEGER,
                 creator TEXT,
                 lyr_preview TEXT,
                 file_size INTEGER NOT NULL DEFAULT 0,
                 modified_at INTEGER NOT NULL DEFAULT 0,
                 indexed_at INTEGER NOT NULL DEFAULT 0,
                 missing INTEGER NOT NULL DEFAULT 0,
                 origin TEXT NOT NULL DEFAULT 'scan',
                 UNIQUE(code, format),
                 UNIQUE(path)
             );
             CREATE INDEX IF NOT EXISTS songs_title ON songs(title);
             CREATE INDEX IF NOT EXISTS songs_artist ON songs(artist);
             CREATE TABLE IF NOT EXISTS folders (
                 kind TEXT PRIMARY KEY,
                 path TEXT NOT NULL
             );",
        )?;
        let version = self.schema_version()?;
        if version < 2 {
            self.migrate_to_v2()?;
        }
        if version < 3 {
            self.migrate_to_v3()?;
        }
        if version < 4 {
            self.migrate_to_v4()?;
        }
        self.write_schema_version()?;
        self.fts = self.has_fts()?;
        Ok(())
    }

    fn schema_version(&self) -> Result<i64> {
        let value: Option<String> = self
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        Ok(value
            .and_then(|value| value.trim().parse::<i64>().ok())
            .unwrap_or(0))
    }

    fn write_schema_version(&self) -> Result<()> {
        self.conn.execute(
            "INSERT INTO meta(key, value) VALUES ('schema_version', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![SCHEMA_VERSION.to_string()],
        )?;
        self.conn
            .execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION};"))?;
        Ok(())
    }

    fn has_fts(&self) -> Result<bool> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name = 'songs_fts'",
            [],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    /// Add the FTS5 lyric index. The trigram tokenizer lets it answer
    /// substrings for scripts without spaces, such as Thai.
    ///
    /// Builds of SQLite without FTS5 or trigram support keep working: the
    /// tables stay absent and lyric search falls back to `LIKE`.
    fn migrate_to_v2(&mut self) -> Result<()> {
        // Old rows may predate the default key.
        self.conn
            .execute_batch("UPDATE songs SET key = 'C' WHERE key IS NULL OR TRIM(key) = '';")?;
        let outcome = self.conn.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS songs_fts USING fts5(
                 code, file_name, title, artist,
                 content = 'songs',
                 content_rowid = 'id',
                 tokenize = 'trigram'
             );
             CREATE TRIGGER IF NOT EXISTS songs_fts_insert AFTER INSERT ON songs BEGIN
                 INSERT INTO songs_fts(rowid, code, file_name, title, artist)
                 VALUES (new.id, new.code, new.file_name, new.title, new.artist);
             END;
             CREATE TRIGGER IF NOT EXISTS songs_fts_delete AFTER DELETE ON songs BEGIN
                 INSERT INTO songs_fts(songs_fts, rowid, code, file_name, title, artist)
                 VALUES ('delete', old.id, old.code, old.file_name, old.title, old.artist);
             END;
             CREATE TRIGGER IF NOT EXISTS songs_fts_update
                 AFTER UPDATE OF code, file_name, title, artist ON songs BEGIN
                 INSERT INTO songs_fts(songs_fts, rowid, code, file_name, title, artist)
                 VALUES ('delete', old.id, old.code, old.file_name, old.title, old.artist);
                 INSERT INTO songs_fts(rowid, code, file_name, title, artist)
                 VALUES (new.id, new.code, new.file_name, new.title, new.artist);
             END;
             INSERT INTO songs_fts(songs_fts) VALUES('rebuild');",
        );
        if let Err(err) = outcome {
            eprintln!("search index unavailable, using LIKE search: {err:#}");
        }
        Ok(())
    }

    /// Drop the lyric column and rebuild the index without it: songs are found
    /// by code, file name, title and artist only.
    fn migrate_to_v4(&mut self) -> Result<()> {
        let has_lyrics: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('songs') WHERE name = 'lyrics'",
            [],
            |row| row.get(0),
        )?;
        if has_lyrics > 0 {
            self.conn.execute_batch(
                "DROP TRIGGER IF EXISTS songs_fts_insert;
                 DROP TRIGGER IF EXISTS songs_fts_delete;
                 DROP TRIGGER IF EXISTS songs_fts_update;
                 DROP TABLE IF EXISTS songs_fts;
                 ALTER TABLE songs DROP COLUMN lyrics;",
            )?;
        }
        self.migrate_to_v2()?;
        // Dropping the lyric text leaves a lot of free pages: a 24k-song file
        // shrinks from ~170 MB to ~20 MB. Runs once, on the upgrade only.
        self.conn.execute_batch("VACUUM;")?;
        Ok(())
    }

    /// Index the browse order so moving through a large library never sorts
    /// the whole table. Browsing a 20k-song library used to take ~80 ms per
    /// arrow-key press; the covering index brings it to a few milliseconds.
    ///
    /// The old boolean `songs_missing` index is dropped: it has almost no
    /// selectivity and made the planner sort the table instead.
    fn migrate_to_v3(&mut self) -> Result<()> {
        self.conn.execute_batch(
            "DROP INDEX IF EXISTS songs_missing;
             CREATE INDEX IF NOT EXISTS songs_browse
                 ON songs(missing, code COLLATE NOCASE, format);
             CREATE INDEX IF NOT EXISTS songs_code_nocase
                 ON songs(code COLLATE NOCASE, format);",
        )?;
        Ok(())
    }

    /// Replace scanned rows while keeping manual rows and marking vanished
    /// files as missing.
    pub fn upsert_scan(&mut self, songs: &[SongRecord], formats: &[&str]) -> Result<()> {
        let tx = self.conn.transaction()?;
        for format in formats {
            tx.execute(
                "UPDATE songs SET missing = 1 WHERE origin = 'scan' AND format = ?1",
                [format],
            )?;
        }
        write_records(&tx, songs, "scan")?;
        tx.commit()?;
        Ok(())
    }

    /// Replace every scanned row with a fresh scan.
    ///
    /// "Create a database" rebuilds the library from the selected folders, so
    /// rows of formats that were not scanned must disappear too. Songs added
    /// with the importer (`origin = "manual"`) are kept.
    pub fn rebuild_scan(&mut self, songs: &[SongRecord]) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM songs WHERE origin = 'scan'", [])?;
        write_records(&tx, songs, "scan")?;
        tx.commit()?;
        Ok(())
    }

    /// Add or update specific songs without marking anything else as missing.
    /// Used by the Add a song importer, which does not scan a whole folder.
    pub fn insert_records(&mut self, songs: &[SongRecord]) -> Result<()> {
        let tx = self.conn.transaction()?;
        write_records(&tx, songs, "manual")?;
        tx.commit()?;
        Ok(())
    }

    /// Every row in browse order.
    pub fn songs(&self) -> Result<Vec<SongRecord>> {
        self.load_songs()
    }

    fn load_songs(&self) -> Result<Vec<SongRecord>> {
        let mut statement = self.conn.prepare(
            "SELECT code, format, path, file_name, title, artist, key, tempo, bars,
                    duration_ms, language, vocal_channel, year, creator, lyr_preview,
                    file_size, modified_at, missing
             FROM songs
             ORDER BY code COLLATE NOCASE, format",
        )?;
        let rows = statement.query_map([], song_row)?;
        let mut songs = Vec::new();
        for row in rows {
            songs.push(row?);
        }
        Ok(songs)
    }

    #[allow(dead_code)]
    pub fn count(&self) -> Result<usize> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM songs", [], |row| row.get(0))?;
        Ok(count as usize)
    }

    /// Find songs by code, file name, title or artist.
    ///
    /// Queries of three characters or more use the trigram index. Shorter ones
    /// cannot use it and a full scan costs about a quarter of a second in a
    /// 100k-song library, so they return nothing while the user types.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<LyricHit>> {
        let query = query.trim();
        if query.is_empty() || limit == 0 || query.chars().count() < 3 {
            return Ok(Vec::new());
        }
        let Some(matched) = self.fts_match(query) else {
            return self.search_with_like(query, limit);
        };
        // Bounding the candidates keeps a very common word cheap even in a
        // 100k-song library.
        let candidates = limit.saturating_mul(20).clamp(200, 4_000);
        let mut statement = self
            .conn
            .prepare("SELECT rowid FROM songs_fts WHERE songs_fts MATCH ?1 LIMIT ?2")?;
        let ids: Vec<i64> = statement
            .query_map(params![matched, candidates as i64], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        if ids.is_empty() {
            return Ok(Vec::new());
        }

        let list = join_ids(&ids);
        let mut statement = self.conn.prepare(&format!(
            "SELECT id FROM songs
             WHERE id IN ({list}) AND missing = 0
             ORDER BY code COLLATE NOCASE, format
             LIMIT ?1"
        ))?;
        let ordered: Vec<i64> = statement
            .query_map(params![limit as i64], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        if ordered.is_empty() {
            return Ok(Vec::new());
        }

        let list = join_ids(&ordered);
        let mut statement = self.conn.prepare(&format!(
            "SELECT code, format, path, title, artist, key, tempo, bars,
                    COALESCE(lyr_preview, '')
             FROM songs
             WHERE id IN ({list})
             ORDER BY code COLLATE NOCASE, format"
        ))?;
        let mut hits = Vec::new();
        for row in statement.query_map([], hit_row)? {
            hits.push(row?);
        }
        Ok(hits)
    }

    /// The plain `LIKE` scan, used for short queries and libraries without an
    /// FTS index.
    fn search_with_like(&self, query: &str, limit: usize) -> Result<Vec<LyricHit>> {
        let pattern = format!("%{}%", escape_like(&query.to_lowercase()));
        let mut statement = self.conn.prepare(
            "SELECT code, format, path, title, artist, key, tempo, bars,
                    COALESCE(lyr_preview, '')
             FROM songs
             WHERE missing = 0
               AND (LOWER(title) LIKE ?1 ESCAPE '\\'
                    OR LOWER(artist) LIKE ?1 ESCAPE '\\'
                    OR LOWER(code) LIKE ?1 ESCAPE '\\'
                    OR LOWER(file_name) LIKE ?1 ESCAPE '\\')
             ORDER BY code COLLATE NOCASE
             LIMIT ?2",
        )?;
        let mut hits = Vec::new();
        for row in statement.query_map(params![pattern, limit as i64], hit_row)? {
            hits.push(row?);
        }
        Ok(hits)
    }

    /// A quoted FTS5 phrase for the trigram index, or `None` when the index
    /// cannot answer the query and `LIKE` must be used.
    fn fts_match(&self, query: &str) -> Option<String> {
        if !self.fts || query.chars().count() < 3 {
            return None;
        }
        if !query.chars().any(char::is_alphanumeric) {
            return None;
        }
        // A quote would need FTS5 escaping; the plain scan is safer there.
        if query.contains('"') {
            return None;
        }
        Some(format!("\"{query}\""))
    }

    /// Number of songs that can be browsed (not missing).
    pub fn active_song_count(&self) -> Result<usize> {
        let count: i64 =
            self.conn
                .query_row("SELECT COUNT(*) FROM songs WHERE missing = 0", [], |row| {
                    row.get(0)
                })?;
        Ok(count.max(0) as usize)
    }

    /// One song by its position in the library, used by arrow-key browsing.
    pub fn song_hit_at(&self, offset: usize) -> Result<Option<LyricHit>> {
        let mut statement = self.conn.prepare(
            "SELECT code, format, path, title, artist, key, tempo, bars,
                    COALESCE(lyr_preview, '')
             FROM songs
             WHERE missing = 0
             ORDER BY code COLLATE NOCASE, format
             LIMIT 1 OFFSET ?1",
        )?;
        let mut rows = statement.query(params![offset as i64])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        let code = row.get::<_, String>(0)?;
        let format = row.get::<_, String>(1)?;
        let path = row.get::<_, String>(2)?;
        let title = row.get::<_, String>(3)?;
        let artist = row.get::<_, String>(4)?;
        let key = row.get::<_, Option<String>>(5)?;
        let tempo = row.get::<_, Option<f64>>(6)?;
        let bars = row.get::<_, Option<i64>>(7)?.map(|value| value as u32);
        let preview = row.get::<_, String>(8)?;
        Ok(Some(LyricHit {
            code,
            format,
            path: PathBuf::from(path),
            title,
            artist,
            key,
            tempo,
            bars,
            preview,
        }))
    }

    /// Run SQLite's fast consistency check. `Ok(None)` means healthy;
    /// `Ok(Some(message))` reports the first problem found.
    pub fn quick_check(&self) -> Result<Option<String>> {
        let result: String = self
            .conn
            .query_row("PRAGMA quick_check(1)", [], |row| row.get(0))?;
        Ok((result != "ok").then_some(result))
    }

    /// Rebuild the database file and the lyric index so deletions and
    /// rescans give their space back. Returns `(before, after)` in bytes.
    pub fn compact(&mut self) -> Result<(u64, u64)> {
        // Fold the WAL back into the file so the "before" size is the real one.
        self.conn
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        let before = self.file_size();
        if self.fts {
            self.conn
                .execute_batch("INSERT INTO songs_fts(songs_fts) VALUES('optimize');")?;
        }
        // VACUUM refuses to run inside a transaction, so checkpoint again
        // afterwards so the reported size is the final one.
        self.conn
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE); VACUUM;")?;
        self.conn
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        Ok((before, self.file_size()))
    }

    /// Write a consistent, compacted copy of the library to `target`.
    /// Returns the size of the written file in bytes.
    ///
    /// Uses `VACUUM INTO`, which is safe while the library is open and never
    /// leaves a half-written backup behind.
    pub fn backup_to(&self, target: &Path) -> Result<u64> {
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        if target.exists() {
            anyhow::bail!("backup already exists: {}", target.display());
        }
        let quoted = target.to_string_lossy().replace('\'', "''");
        self.conn
            .execute_batch(&format!("VACUUM INTO '{quoted}';"))?;
        Ok(std::fs::metadata(target)
            .map(|meta| meta.len())
            .unwrap_or(0))
    }

    fn file_size(&self) -> u64 {
        self.path
            .as_ref()
            .and_then(|path| std::fs::metadata(path).ok())
            .map(|meta| meta.len())
            .unwrap_or(0)
    }

    /// Check every row against the filesystem and update its `missing` flag.
    /// Returns `(newly_missing, restored)`.
    pub fn mark_missing_files(&mut self) -> Result<(usize, usize)> {
        let mut statement = self.conn.prepare("SELECT id, path, missing FROM songs")?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)? != 0,
            ))
        })?;
        let mut changes = Vec::new();
        let mut missing = 0usize;
        let mut restored = 0usize;
        for row in rows {
            let (id, path, was_missing) = row?;
            let exists = Path::new(&path).is_file();
            if !exists && !was_missing {
                changes.push((id, 1));
                missing += 1;
            } else if exists && was_missing {
                changes.push((id, 0));
                restored += 1;
            }
        }
        drop(statement);
        let tx = self.conn.transaction()?;
        for (id, value) in changes {
            tx.execute(
                "UPDATE songs SET missing = ?1 WHERE id = ?2",
                params![value, id],
            )?;
        }
        tx.commit()?;
        Ok((missing, restored))
    }
}

fn song_row(row: &Row<'_>) -> rusqlite::Result<SongRecord> {
    Ok(SongRecord {
        code: row.get(0)?,
        format: row.get(1)?,
        path: PathBuf::from(row.get::<_, String>(2)?),
        file_name: row.get(3)?,
        title: row.get(4)?,
        artist: row.get(5)?,
        key: row.get(6)?,
        tempo: row.get(7)?,
        bars: row.get::<_, Option<i64>>(8)?.map(|value| value as u32),
        duration_ms: row.get::<_, i64>(9)?.max(0) as u64,
        language: row.get(10)?,
        vocal_channel: row.get(11)?,
        year: row.get(12)?,
        creator: row.get(13)?,
        lyr_preview: row.get(14)?,
        file_size: row.get::<_, i64>(15)?.max(0) as u64,
        modified_at: row.get(16)?,
        missing: row.get::<_, i64>(17)? != 0,
    })
}

/// One search result row.
fn hit_row(row: &Row<'_>) -> rusqlite::Result<LyricHit> {
    Ok(LyricHit {
        code: row.get(0)?,
        format: row.get(1)?,
        path: PathBuf::from(row.get::<_, String>(2)?),
        title: row.get(3)?,
        artist: row.get(4)?,
        key: row.get(5)?,
        tempo: row.get(6)?,
        bars: row.get::<_, Option<i64>>(7)?.map(|value| value as u32),
        preview: row.get(8)?,
    })
}

/// Comma-separated row ids for an `IN (...)` list. The values are integers
/// from SQLite, so they need no quoting.
fn join_ids(ids: &[i64]) -> String {
    ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",")
}

fn write_records(tx: &Transaction<'_>, songs: &[SongRecord], origin: &str) -> Result<()> {
    let indexed_at = unix_now();
    for song in songs {
        let key = normalise_key(song.key.as_deref());
        let path = song.path.to_string_lossy();
        // Keep the Add a song provenance when a folder scan rewrites the row.
        let previous_origin: Option<String> = tx
            .query_row(
                "SELECT origin FROM songs
                 WHERE path = ?1 OR (code = ?2 AND format = ?3)
                 LIMIT 1",
                params![path, song.code, song.format],
                |row| row.get(0),
            )
            .optional()?;
        let origin = match previous_origin.as_deref() {
            Some("manual") => "manual",
            _ => origin,
        };
        // Keep (code, format) and path unique even when a file moved.
        tx.execute(
            "DELETE FROM songs WHERE (code = ?1 AND format = ?2) OR path = ?3",
            params![song.code, song.format, path],
        )?;
        tx.execute(
            "INSERT INTO songs (
                code, format, path, file_name, title, artist, key, tempo, bars,
                duration_ms, language, vocal_channel, year, creator, lyr_preview,
                file_size, modified_at, indexed_at, missing, origin
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9,
                ?10, ?11, ?12, ?13, ?14, ?15,
                ?16, ?17, ?18, 0, ?19
             )",
            params![
                song.code,
                song.format,
                path,
                song.file_name,
                song.title,
                song.artist,
                key,
                song.tempo,
                song.bars,
                song.duration_ms as i64,
                song.language,
                song.vocal_channel,
                song.year,
                song.creator,
                song.lyr_preview,
                song.file_size as i64,
                song.modified_at,
                indexed_at,
                origin,
            ],
        )?;
    }
    Ok(())
}

/// Scan all configured folders and parse metadata into records.
///
/// `folders` pairs each root with its kind (`ncn`, `kmid`, `kar` or `all`).
/// A specific kind decides the stored format, so the folder a song lives in
/// wins over what the file itself looks like. `progress(done, total)` is called
/// after every file.
pub fn scan_folders(
    folders: &[(String, PathBuf)],
    existing: &[SongRecord],
    progress: &(dyn Fn(usize, usize) + Sync),
) -> ScanOutcome {
    let mut paths = Vec::new();
    let mut seen_paths = HashSet::new();
    let mut folder_errors = Vec::new();
    for (kind, root) in folders {
        if !root.is_dir() {
            folder_errors.push(format!("missing folder: {}", root.display()));
            continue;
        }
        let mut found = Vec::new();
        collect_files(root, kind, 0, &mut found);
        for path in found {
            if seen_paths.insert(path.clone()) {
                paths.push((path, kind.clone()));
            }
        }
    }
    paths.sort_by(|a, b| a.0.cmp(&b.0));
    let total = paths.len();
    let existing_by_path: HashMap<PathBuf, &SongRecord> = existing
        .iter()
        .map(|song| {
            (
                std::path::absolute(&song.path).unwrap_or_else(|_| song.path.clone()),
                song,
            )
        })
        .collect();
    progress(0, total);
    if total == 0 {
        return ScanOutcome {
            errors: folder_errors,
            ..ScanOutcome::default()
        };
    }

    let workers = std::thread::available_parallelism()
        .map(|value| value.get())
        .unwrap_or(1)
        .clamp(1, 8)
        .min(total);
    let done = AtomicUsize::new(0);
    let skipped = AtomicUsize::new(0);
    let unchanged = AtomicUsize::new(0);
    let records = Mutex::new(Vec::new());
    let errors = Mutex::new(Vec::new());
    let chunk_size = total.div_ceil(workers);

    std::thread::scope(|scope| {
        for chunk in paths.chunks(chunk_size) {
            let done = &done;
            let skipped = &skipped;
            let unchanged = &unchanged;
            let records = &records;
            let errors = &errors;
            let existing_by_path = &existing_by_path;
            scope.spawn(move || {
                for (path, kind) in chunk {
                    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.clone());
                    let reused = existing_by_path.get(&absolute).and_then(|record| {
                        if record.missing {
                            return None;
                        }
                        // The folder decides the type, so a row scanned with
                        // another type must be parsed again.
                        if matches!(kind.as_str(), "ncn" | "kmid" | "kar") && record.format != *kind
                        {
                            return None;
                        }
                        let (modified_at, file_size) = file_stamp(path)?;
                        (record.modified_at == modified_at && record.file_size == file_size)
                            .then(|| (*record).clone())
                    });
                    if let Some(record) = reused {
                        records.lock().unwrap().push(record);
                        unchanged.fetch_add(1, Ordering::Relaxed);
                    } else {
                        match load(path, &LoadOptions::default()) {
                            Ok(song) => {
                                records
                                    .lock()
                                    .unwrap()
                                    .push(record_from_song(path, &song, kind));
                            }
                            Err(err) => {
                                skipped.fetch_add(1, Ordering::Relaxed);
                                let mut errors = errors.lock().unwrap();
                                if errors.len() < 20 {
                                    errors.push(format!("{}: {err:#}", path.display()));
                                }
                            }
                        }
                    }
                    let current = done.fetch_add(1, Ordering::Relaxed) + 1;
                    progress(current, total);
                }
            });
        }
    });

    let mut records = records.into_inner().unwrap();
    records.sort_by(|a, b| a.path.cmp(&b.path));
    let mut seen_keys = HashSet::new();
    let mut songs = Vec::new();
    let mut duplicates = 0;
    for record in records {
        let key = (record.code.clone(), record.format.clone());
        if seen_keys.insert(key) {
            songs.push(record);
        } else {
            duplicates += 1;
        }
    }

    let mut errors = errors.into_inner().unwrap();
    errors.extend(folder_errors);
    ScanOutcome {
        songs,
        skipped: skipped.load(Ordering::Relaxed),
        duplicates,
        unchanged: unchanged.load(Ordering::Relaxed),
        errors,
    }
}

fn collect_files(root: &Path, kind: &str, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > 6 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, kind, depth + 1, out);
        } else if is_song_for_kind(&path, kind) {
            out.push(path);
        }
    }
}

fn is_song_for_kind(path: &Path, kind: &str) -> bool {
    let Some(extension) = path.extension().and_then(|ext| ext.to_str()) else {
        return false;
    };
    let extension = extension.to_ascii_lowercase();
    match kind {
        "kar" => extension == "kar",
        "ncn" | "kmid" => extension == "mid" || extension == "midi",
        _ => matches!(extension.as_str(), "kar" | "mid" | "midi"),
    }
}

fn file_stamp(path: &Path) -> Option<(i64, u64)> {
    let metadata = std::fs::metadata(path).ok()?;
    let modified_at = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);
    Some((modified_at, metadata.len()))
}

fn record_from_song(path: &Path, song: &Song, kind: &str) -> SongRecord {
    let code = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    // The folder a file came from decides its type: a `.mid` inside the NCN
    // folder is an NCN song even if its contents look like another format.
    let detected = match song.format {
        SourceFormat::Kar => "kar",
        SourceFormat::Ncn => "ncn",
        SourceFormat::Kmid => "kmid",
        SourceFormat::Midi => "midi",
    };
    let format = match kind {
        "ncn" | "kmid" | "kar" => kind,
        _ => detected,
    };
    let (language, vocal_channel) = match format {
        "ncn" | "kar" => (Some("th".to_owned()), Some(9)),
        "kmid" => (song.language.clone(), song.vocal_channel),
        _ => (None, None),
    };
    let metadata = std::fs::metadata(path).ok();
    let file_size = metadata.as_ref().map(|meta| meta.len()).unwrap_or(0);
    let modified_at = metadata
        .and_then(|meta| meta.modified().ok())
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);

    SongRecord {
        code,
        format: format.to_owned(),
        path: std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf()),
        file_name,
        title: song.title.clone().unwrap_or_default(),
        artist: song.artist.clone().unwrap_or_default(),
        key: Some(normalise_key(song.key.as_deref())),
        tempo: Some(song.bpm),
        bars: bar_count(song),
        duration_ms: song.duration_us / 1000,
        language,
        vocal_channel,
        year: song.year,
        creator: song.creator.clone(),
        lyr_preview: lyric_preview(song),
        file_size,
        modified_at,
        missing: false,
    }
}

fn normalise_key(key: Option<&str>) -> String {
    key.map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("C")
        .to_owned()
}

pub fn bar_count(song: &Song) -> Option<u32> {
    let last_tick = song
        .events
        .iter()
        .filter(|event| {
            matches!(
                event.kind,
                EventKind::NoteOn { .. }
                    | EventKind::NoteOff { .. }
                    | EventKind::ProgramChange { .. }
                    | EventKind::ControlChange { .. }
                    | EventKind::PitchBend { .. }
            )
        })
        .map(|event| event.tick)
        .max()?;
    let meter = MeterMap::new(song.ticks_per_quarter, song.time_signatures.clone());
    Some(meter.position_at_tick(last_tick).bar)
}

fn escape_like(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

fn lyric_preview(song: &Song) -> Option<String> {
    let mut preview = String::new();
    for line in song.lyric_lines.iter().take(5) {
        let text = line.text.trim();
        if text.is_empty() {
            continue;
        }
        if !preview.is_empty() {
            preview.push(' ');
        }
        preview.push_str(text);
    }
    if preview.is_empty() {
        None
    } else {
        Some(preview.chars().take(200).collect())
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(code: &str) -> SongRecord {
        SongRecord {
            code: code.to_owned(),
            format: "ncn".to_owned(),
            path: PathBuf::from(format!("C:/Songs/{code}.mid")),
            file_name: format!("{code}.mid"),
            title: "Title".to_owned(),
            artist: "Artist".to_owned(),
            key: Some("G".to_owned()),
            tempo: Some(120.0),
            bars: Some(64),
            duration_ms: 1000,
            language: Some("th".to_owned()),
            vocal_channel: Some(9),
            year: None,
            creator: None,
            lyr_preview: Some("preview".to_owned()),
            file_size: 10,
            modified_at: 1,
            missing: false,
        }
    }

    #[test]
    fn finds_songs_by_code_name_title_and_artist() {
        let mut library = Library::open_in_memory().unwrap();
        library.insert_records(&[record("1001")]).unwrap();
        for query in ["1001", "1001.mid", "title", "artist"] {
            let hits = library.search(query, 10).unwrap();
            assert_eq!(hits.len(), 1, "{query}");
            assert_eq!(hits[0].code, "1001");
            assert_eq!(hits[0].preview, "preview");
        }
    }

    #[test]
    fn searches_quoted_text_with_the_like_fallback() {
        let mut library = Library::open_in_memory().unwrap();
        let mut song = record("1001");
        song.title = "Say \"Hi\"".to_owned();
        library.insert_records(&[song]).unwrap();
        let hits = library.search("\"Hi\"", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].code, "1001");
    }

    #[test]
    fn finds_a_phrase_inside_a_long_thai_title() {
        // Thai writes without spaces, so only a substring index can find this.
        let mut library = Library::open_in_memory().unwrap();
        let mut song = record("1001");
        song.title = "โอ้ยอดรักของพี่นี้ภักดี".to_owned();
        library.insert_records(&[song]).unwrap();
        let hits = library.search("ยอดรัก", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].code, "1001");
    }

    #[test]
    fn lyrics_are_not_searched() {
        let mut library = Library::open_in_memory().unwrap();
        let mut song = record("1001");
        song.lyr_preview = Some("เนื้อเพลงที่ไม่มีในชื่อ".to_owned());
        library.insert_records(&[song]).unwrap();
        assert!(library.search("เนื้อเพลงที่ไม่มี", 10).unwrap().is_empty());
    }

    #[test]
    fn upgrades_a_version_one_database() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("dontreecrab-migrate-{unique}"));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("library.sqlite");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 CREATE TABLE songs (
                     id INTEGER PRIMARY KEY,
                     code TEXT NOT NULL,
                     format TEXT NOT NULL,
                     path TEXT NOT NULL,
                     file_name TEXT NOT NULL,
                     title TEXT NOT NULL DEFAULT '',
                     artist TEXT NOT NULL DEFAULT '',
                     key TEXT,
                     tempo REAL,
                     bars INTEGER,
                     duration_ms INTEGER NOT NULL DEFAULT 0,
                     language TEXT,
                     vocal_channel INTEGER,
                     year INTEGER,
                     creator TEXT,
                     lyr_preview TEXT,
                     lyrics TEXT,
                     file_size INTEGER NOT NULL DEFAULT 0,
                     modified_at INTEGER NOT NULL DEFAULT 0,
                     indexed_at INTEGER NOT NULL DEFAULT 0,
                     missing INTEGER NOT NULL DEFAULT 0,
                     origin TEXT NOT NULL DEFAULT 'scan',
                     UNIQUE(code, format),
                     UNIQUE(path)
                 );
                 INSERT INTO meta(key, value) VALUES ('schema_version', '1');
                 INSERT INTO songs (code, format, path, file_name, title, artist, lyrics)
                 VALUES ('2001', 'ncn', 'C:/Songs/2001.mid', '2001.mid',
                         'เพลงเก่า', 'นักร้องเก่า', 'เนื้อเพลงเก่าที่เก็บไว้');",
            )
            .unwrap();
        }

        let library = Library::open(&path).unwrap();
        assert_eq!(library.schema_version().unwrap(), SCHEMA_VERSION);
        assert!(library.fts);
        let hits = library.search("เพลงเก่า", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].code, "2001");
        let columns: i64 = library
            .conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('songs') WHERE name = 'lyrics'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(columns, 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn keeps_manual_origin_across_a_scan() {
        let mut library = Library::open_in_memory().unwrap();
        let song = record("1001");
        library.insert_records(&[song.clone()]).unwrap();

        let mut scanned = song;
        scanned.title = "Scanned".to_owned();
        library.upsert_scan(&[scanned], &["ncn"]).unwrap();

        let rows = library.songs().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].title, "Scanned");
        let origin: String = library
            .conn
            .query_row("SELECT origin FROM songs", [], |row| row.get(0))
            .unwrap();
        assert_eq!(origin, "manual");
        assert_eq!(library.search("Scanned", 10).unwrap().len(), 1);
    }

    #[test]
    fn marks_missing_files_and_restores_them() {
        let mut library = Library::open_in_memory().unwrap();
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("dontreecrab-missing-{unique}"));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("1001.mid");
        std::fs::write(&file, b"x").unwrap();

        let mut song = record("1001");
        song.path = file.clone();
        song.file_size = 1;
        library.insert_records(&[song]).unwrap();

        assert_eq!(library.mark_missing_files().unwrap(), (0, 0));
        std::fs::remove_file(&file).unwrap();
        assert_eq!(library.mark_missing_files().unwrap(), (1, 0));
        assert!(library.songs().unwrap()[0].missing);
        std::fs::write(&file, b"x").unwrap();
        assert_eq!(library.mark_missing_files().unwrap(), (0, 1));
        assert!(!library.songs().unwrap()[0].missing);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn incremental_scan_reuses_unchanged_files() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("dontreecrab-incremental-{unique}"));
        std::fs::create_dir_all(&dir).unwrap();
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/meter34.mid");
        let target = dir.join("meter34.mid");
        std::fs::copy(&fixture, &target).unwrap();

        let folders = vec![("all".to_owned(), dir.clone())];
        let progress = |_: usize, _: usize| {};
        let first = scan_folders(&folders, &[], &progress);
        assert_eq!(first.songs.len(), 1);
        assert_eq!(first.unchanged, 0);

        let second = scan_folders(&folders, &first.songs, &progress);
        assert_eq!(second.songs.len(), 1);
        assert_eq!(second.unchanged, 1);
        assert_eq!(second.skipped, 0);

        std::thread::sleep(std::time::Duration::from_millis(1100));
        std::fs::write(&target, std::fs::read(&target).unwrap()).unwrap();
        let third = scan_folders(&folders, &second.songs, &progress);
        assert_eq!(third.unchanged, 0);
        assert_eq!(third.songs.len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn browses_the_first_and_last_song() {
        let mut library = Library::open_in_memory().unwrap();
        library
            .insert_records(&[record("1001"), record("2002")])
            .unwrap();
        assert_eq!(library.active_song_count().unwrap(), 2);
        assert_eq!(library.song_hit_at(0).unwrap().unwrap().code, "1001");
        assert_eq!(library.song_hit_at(1).unwrap().unwrap().code, "2002");
        assert!(library.song_hit_at(2).unwrap().is_none());
    }

    #[test]
    fn stores_and_lists_songs() {
        let mut library = Library::open_in_memory().unwrap();
        library.upsert_scan(&[record("1001")], &["ncn"]).unwrap();
        let songs = library.songs().unwrap();
        assert_eq!(songs.len(), 1);
        assert_eq!(songs[0].code, "1001");
        assert_eq!(songs[0].title, "Title");
        assert_eq!(library.count().unwrap(), 1);
    }

    #[test]
    fn missing_keys_default_to_c() {
        let mut library = Library::open_in_memory().unwrap();
        let mut song = record("1001");
        song.key = None;
        library.upsert_scan(&[song], &["ncn"]).unwrap();
        assert_eq!(library.songs().unwrap()[0].key.as_deref(), Some("C"));
    }

    #[test]
    fn existing_empty_keys_are_migrated_to_c() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("dontreecrab-key-{unique}"));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("library.sqlite");

        let library = Library::open(&path).unwrap();
        library
            .conn
            .execute(
                "INSERT INTO songs (code, format, path, file_name, key)
                 VALUES ('2002', 'ncn', 'C:/Songs/2002.mid', '2002.mid', NULL)",
                [],
            )
            .unwrap();
        // Pretend the row predates the schema that filled in empty keys.
        library
            .conn
            .execute(
                "UPDATE meta SET value = '1' WHERE key = 'schema_version'",
                [],
            )
            .unwrap();
        drop(library);

        let library = Library::open(&path).unwrap();
        let key: Option<String> = library
            .conn
            .query_row("SELECT key FROM songs WHERE code = '2002'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(key.as_deref(), Some("C"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn insert_records_keeps_existing_songs() {
        let mut library = Library::open_in_memory().unwrap();
        library.upsert_scan(&[record("1001")], &["ncn"]).unwrap();
        library.insert_records(&[record("2002")]).unwrap();
        let songs = library.songs().unwrap();
        assert_eq!(songs.len(), 2);
        assert!(songs.iter().all(|song| !song.missing));
    }

    #[test]
    fn rescan_replaces_the_same_code_and_format() {
        let mut library = Library::open_in_memory().unwrap();
        library.upsert_scan(&[record("1001")], &["ncn"]).unwrap();
        let mut updated = record("1001");
        updated.title = "New title".to_owned();
        updated.path = PathBuf::from("C:/Songs/moved/1001.mid");
        library.upsert_scan(&[updated], &["ncn"]).unwrap();
        let songs = library.songs().unwrap();
        assert_eq!(songs.len(), 1);
        assert_eq!(songs[0].title, "New title");
    }

    #[test]
    fn the_folder_kind_decides_the_song_format() {
        let dir = std::env::temp_dir().join(format!("dontreecrab-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // A plain MIDI file placed in the NCN folder.
        let track: &[u8] = &[
            0x00, 0xFF, 0x51, 0x03, 0x07, 0xA1, 0x20, 0x00, 0x90, 0x3C, 0x64, 0x83, 0x60, 0x80,
            0x3C, 0x00, 0x00, 0xFF, 0x2F, 0x00,
        ];
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"MThd");
        bytes.extend_from_slice(&6u32.to_be_bytes());
        bytes.extend_from_slice(&0u16.to_be_bytes());
        bytes.extend_from_slice(&1u16.to_be_bytes());
        bytes.extend_from_slice(&384u16.to_be_bytes());
        bytes.extend_from_slice(b"MTrk");
        bytes.extend_from_slice(&(track.len() as u32).to_be_bytes());
        bytes.extend_from_slice(track);
        std::fs::write(dir.join("TEST01.mid"), &bytes).unwrap();

        let outcome = scan_folders(&[("ncn".to_owned(), dir.clone())], &[], &|_, _| {});
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(outcome.songs.len(), 1, "scan errors: {:?}", outcome.errors);
        assert_eq!(outcome.songs[0].format, "ncn");
    }

    #[test]
    fn rebuilding_replaces_every_scanned_format() {
        let mut library = Library::open_in_memory().unwrap();
        let ncn = record("A001");
        let mut kar = record("A002");
        kar.format = "kar".to_owned();
        library
            .upsert_scan(&[ncn.clone(), kar], &["ncn", "kar"])
            .unwrap();
        assert_eq!(library.songs().unwrap().len(), 2);

        // A rebuild that only scanned NCN drops the KAR row.
        library.rebuild_scan(&[ncn]).unwrap();
        let songs = library.songs().unwrap();
        assert_eq!(songs.len(), 1, "other formats should disappear: {songs:?}");
        assert_eq!(songs[0].format, "ncn");
    }

    #[test]
    fn compacts_the_database_and_keeps_the_songs() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("dontreecrab-compact-{unique}"));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("library.sqlite");
        let mut library = Library::open(&path).unwrap();
        let songs: Vec<SongRecord> = (0..300)
            .map(|index| record(&format!("{index:04}")))
            .collect();
        library.insert_records(&songs).unwrap();
        library
            .conn
            .execute("DELETE FROM songs WHERE code > '0099'", [])
            .unwrap();

        let (before, after) = library.compact().unwrap();
        assert_eq!(library.songs().unwrap().len(), 100);
        assert!(after <= before, "{before} -> {after}");
        assert_eq!(library.quick_check().unwrap(), None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn backs_up_the_library_to_a_new_file() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("dontreecrab-backup-{unique}"));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("library.sqlite");
        let mut library = Library::open(&path).unwrap();
        library
            .insert_records(&[record("1001"), record("1002")])
            .unwrap();

        let target = dir.join("backups").join("library-copy.sqlite");
        let size = library.backup_to(&target).unwrap();
        assert!(size > 0);
        assert!(target.is_file());

        let copy = Library::open(&target).unwrap();
        assert_eq!(copy.songs().unwrap().len(), 2);
        assert_eq!(copy.search("title", 10).unwrap().len(), 2);

        // Never overwrite an existing backup.
        assert!(library.backup_to(&target).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[ignore = "slow: builds twenty thousand rows"]
    fn browses_twenty_thousand_songs() {
        let mut library = Library::open_in_memory().unwrap();
        let songs: Vec<SongRecord> = (0..20_000)
            .map(|index| {
                let mut song = record(&format!("{index:05}"));
                song.title = format!("เพลงทดสอบหมายเลข {index}");
                song.artist = "นักร้องทดสอบ".to_owned();
                song
            })
            .collect();
        library.insert_records(&songs).unwrap();

        for offset in [0usize, 1, 1_000, 10_000, 19_999] {
            let started = std::time::Instant::now();
            let hit = library.song_hit_at(offset).unwrap();
            let elapsed = started.elapsed();
            assert_eq!(hit.unwrap().code, format!("{offset:05}"));
            eprintln!("song_hit_at({offset}) {elapsed:?}");
        }

        let started = std::time::Instant::now();
        let count = library.active_song_count().unwrap();
        eprintln!("active_song_count {count} {:?}", started.elapsed());

        let started = std::time::Instant::now();
        let all = library.songs().unwrap();
        eprintln!("songs_summary() {} {:?}", all.len(), started.elapsed());

        let started = std::time::Instant::now();
        let hits = library.search("รักเธอเสมอ", 50).unwrap();
        eprintln!("search_lyrics {} {:?}", hits.len(), started.elapsed());
    }

    #[test]
    #[ignore = "slow: builds one hundred thousand rows"]
    fn browses_one_hundred_thousand_songs() {
        let mut library = Library::open_in_memory().unwrap();
        let songs: Vec<SongRecord> = (0..100_000)
            .map(|index| {
                let mut song = record(&format!("{index:06}"));
                song.title = format!("เพลงทดสอบหมายเลข {index}");
                song.artist = "นักร้องทดสอบ".to_owned();
                song
            })
            .collect();

        let started = std::time::Instant::now();
        library.insert_records(&songs).unwrap();
        eprintln!("100k insert {:?}", started.elapsed());

        for offset in [0usize, 1, 25_000, 50_000, 75_000, 99_999] {
            let started = std::time::Instant::now();
            let hit = library.song_hit_at(offset).unwrap();
            let elapsed = started.elapsed();
            assert_eq!(hit.unwrap().code, format!("{offset:06}"));
            eprintln!("song_hit_at({offset}) {elapsed:?}");
        }

        let started = std::time::Instant::now();
        let count = library.active_song_count().unwrap();
        eprintln!("active_song_count {count} {:?}", started.elapsed());

        let started = std::time::Instant::now();
        let all = library.songs().unwrap();
        eprintln!("songs_summary() {} {:?}", all.len(), started.elapsed());

        let started = std::time::Instant::now();
        let hits = library.search("รักเธอเสมอ", 50).unwrap();
        eprintln!("search_lyrics {} {:?}", hits.len(), started.elapsed());

        let started = std::time::Instant::now();
        let hits = library.search("097777", 50).unwrap();
        eprintln!("search code {} {:?}", hits.len(), started.elapsed());

        let started = std::time::Instant::now();
        let hits = library.search("รัก", 50).unwrap();
        eprintln!("search 2 chars {} {:?}", hits.len(), started.elapsed());

        let started = std::time::Instant::now();
        let hits = library.search("ซฤ", 50).unwrap();
        eprintln!("search 2 chars rare {} {:?}", hits.len(), started.elapsed());
    }

    #[test]
    #[ignore = "slow: builds ten thousand rows"]
    fn searches_ten_thousand_songs() {
        let mut library = Library::open_in_memory().unwrap();
        let songs: Vec<SongRecord> = (0..10_000)
            .map(|index| {
                let mut song = record(&format!("{index:05}"));
                song.title = format!("เพลงทดสอบหมายเลข {index}");
                song.artist = "นักร้องทดสอบ".to_owned();
                song
            })
            .collect();

        let started = std::time::Instant::now();
        library.insert_records(&songs).unwrap();
        let inserted = started.elapsed();

        let started = std::time::Instant::now();
        let hits = library.search("เพลงทดสอบ", 50).unwrap();
        let searched = started.elapsed();

        let started = std::time::Instant::now();
        let loaded = library.songs().unwrap();
        let browsing = started.elapsed();

        assert_eq!(loaded.len(), 10_000);
        assert_eq!(hits.len(), 50);
        eprintln!("10k insert {inserted:?} · search {searched:?} · load {browsing:?}");
    }
}
