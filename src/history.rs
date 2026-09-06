use crate::{
    app::{
        AnnotationAuthor, DetailTab, Focus, JsonRpcExchange, JsonRpcMessage, LineAnnotation,
        MessageDirection, SessionSummary,
    },
    control::{
        FindResults, FoundAnnotation, FoundExchange, Session, SessionExchange, SessionMessage,
    },
};
use anyhow::{anyhow, bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

const SCHEMA_VERSION: i64 = 4;

pub struct HistoryStore {
    connection: Connection,
}

impl HistoryStore {
    pub fn open_default() -> Result<Self> {
        let path = history_path()?;
        let parent = path
            .parent()
            .ok_or_else(|| anyhow!("history path has no parent"))?;
        fs::create_dir_all(parent).context("create history directory")?;

        let store = Self::open(&path)?;
        set_private_permissions(parent, &path)?;
        Ok(store)
    }

    pub fn open(path: &Path) -> Result<Self> {
        let connection = Connection::open(path).context("open history database")?;
        Self::from_connection(connection)
    }

    #[cfg(test)]
    pub fn in_memory() -> Result<Self> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    fn from_connection(connection: Connection) -> Result<Self> {
        connection.execute_batch("PRAGMA foreign_keys = ON;")?;
        let version =
            connection.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))?;
        if version > SCHEMA_VERSION {
            bail!("history database version {version} is newer than this debugger supports");
        }

        let transaction = connection.unchecked_transaction()?;
        transaction.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                target TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS exchanges (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                sequence INTEGER NOT NULL,
                rpc_id TEXT NOT NULL,
                method TEXT,
                complete INTEGER NOT NULL,
                exchange_json TEXT NOT NULL,
                UNIQUE(session_id, sequence)
            );
            DROP INDEX IF EXISTS exchanges_session_sequence;
            CREATE INDEX IF NOT EXISTS exchanges_pending_rpc_id
                ON exchanges(rpc_id, complete, id DESC);
            CREATE TABLE IF NOT EXISTS annotations (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                exchange_index INTEGER NOT NULL,
                panel TEXT NOT NULL,
                tab TEXT NOT NULL,
                start_line INTEGER NOT NULL,
                end_line INTEGER NOT NULL,
                message TEXT NOT NULL,
                text_json TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS annotations_session_exchange
                ON annotations(session_id, exchange_index, created_at_ms);
            ",
        )?;

        if version < 4 {
            transaction.execute_batch(
                "ALTER TABLE annotations ADD COLUMN parent_id TEXT REFERENCES annotations(id);
                 ALTER TABLE annotations ADD COLUMN author TEXT NOT NULL DEFAULT 'unknown'
                    CHECK (author IN ('user', 'agent', 'unknown'));
                 CREATE INDEX annotations_parent ON annotations(parent_id);",
            )?;
        }
        transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        transaction.commit()?;

        Ok(Self { connection })
    }

    pub fn create_session(&mut self, name: Option<&str>, target: &str) -> Result<SessionSummary> {
        let now = database_timestamp_ms(SystemTime::now());
        let count = self
            .connection
            .query_row("SELECT COUNT(*) FROM sessions", [], |row| {
                row.get::<_, i64>(0)
            })?;
        let name = name.map(str::trim).filter(|name| !name.is_empty());
        if let Some(name) = name {
            validate_session_name(name)?;
        }
        let name = name
            .map(str::to_string)
            .unwrap_or_else(|| format!("Session {}", count + 1));
        let id = Uuid::new_v4().to_string();

        self.connection.execute(
            "INSERT INTO sessions (id, name, target, created_at_ms, updated_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?4)",
            params![id, name, target, now],
        )?;

        self.session(&id)?
            .ok_or_else(|| anyhow!("created session disappeared"))
    }

    pub fn session(&self, id: &str) -> Result<Option<SessionSummary>> {
        self.connection
            .query_row(
                "SELECT s.id, s.name, s.target, s.created_at_ms, s.updated_at_ms,
                        COUNT(e.id)
                 FROM sessions s
                 LEFT JOIN exchanges e ON e.session_id = s.id
                 WHERE s.id = ?1
                 GROUP BY s.id",
                [id],
                session_summary,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn list_sessions(&self, limit: usize) -> Result<Vec<SessionSummary>> {
        let mut statement = self.connection.prepare(
            "SELECT s.id, s.name, s.target, s.created_at_ms, s.updated_at_ms,
                    COUNT(e.id)
             FROM sessions s
             LEFT JOIN exchanges e ON e.session_id = s.id
             GROUP BY s.id
             ORDER BY s.updated_at_ms DESC
             LIMIT ?1",
        )?;
        let rows = statement.query_map([sqlite_limit(limit)], session_summary)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn load_session(
        &self,
        id: &str,
    ) -> Result<(SessionSummary, Vec<JsonRpcExchange>, Vec<LineAnnotation>)> {
        let session = self
            .session(id)?
            .ok_or_else(|| anyhow!("session not found: {id}"))?;
        let exchanges = self
            .history(id, usize::MAX, None)?
            .into_iter()
            .map(|(_, exchange)| exchange)
            .collect();
        Ok((session, exchanges, self.annotations(id)?))
    }

    pub fn annotations(&self, session_id: &str) -> Result<Vec<LineAnnotation>> {
        let mut statement = self.connection.prepare(
            "SELECT id, exchange_index, panel, tab, start_line, end_line, message, text_json,
                    parent_id, author, created_at_ms
             FROM annotations
             WHERE session_id = ?1
             ORDER BY created_at_ms, id",
        )?;
        let rows = statement.query_map([session_id], |row| line_annotation(row, 0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn add_annotation(&self, session_id: &str, annotation: &LineAnnotation) -> Result<()> {
        let panel = annotation_panel(annotation.panel)?;
        if let Some(parent_id) = &annotation.parent_id {
            let valid_parent: bool = self.connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM annotations WHERE id = ?1 AND session_id = ?2
                 AND exchange_index = ?3 AND panel = ?4 AND tab = ?5
                 AND start_line = ?6 AND end_line = ?7)",
                params![
                    parent_id,
                    session_id,
                    sqlite_index(annotation.exchange_index),
                    panel,
                    annotation_tab(annotation.tab),
                    sqlite_index(annotation.start_line),
                    sqlite_index(annotation.end_line)
                ],
                |row| row.get(0),
            )?;
            if !valid_parent {
                bail!("reply parent must exist in the same session and line reference");
            }
        }
        self.connection.execute(
            "INSERT INTO annotations (
                 id, session_id, exchange_index, panel, tab, start_line, end_line,
                 message, text_json, created_at_ms, parent_id, author
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                annotation.id,
                session_id,
                sqlite_index(annotation.exchange_index),
                panel,
                annotation_tab(annotation.tab),
                sqlite_index(annotation.start_line),
                sqlite_index(annotation.end_line),
                annotation.message,
                serde_json::to_string(&annotation.text)?,
                i64::try_from(annotation.created_at_ms)
                    .context("annotation timestamp exceeds SQLite range")?,
                annotation.parent_id,
                annotation.author.name(),
            ],
        )?;
        self.touch_session(session_id)?;
        Ok(())
    }

    pub fn update_annotation(&self, session_id: &str, id: &str, message: &str) -> Result<bool> {
        let updated = self.connection.execute(
            "UPDATE annotations SET message = ?3 WHERE session_id = ?1 AND id = ?2",
            params![session_id, id, message],
        )?;
        if updated > 0 {
            self.touch_session(session_id)?;
        }
        Ok(updated > 0)
    }

    pub fn remove_annotation(&self, session_id: &str, id: &str) -> Result<bool> {
        let transaction = self.connection.unchecked_transaction()?;
        transaction.execute(
            "UPDATE annotations SET parent_id =
                (SELECT parent_id FROM annotations WHERE session_id = ?1 AND id = ?2)
             WHERE session_id = ?1 AND parent_id = ?2",
            params![session_id, id],
        )?;
        let removed = transaction.execute(
            "DELETE FROM annotations WHERE session_id = ?1 AND id = ?2",
            params![session_id, id],
        )?;
        if removed > 0 {
            transaction.execute(
                "UPDATE sessions SET updated_at_ms = ?2 WHERE id = ?1",
                params![session_id, database_timestamp_ms(SystemTime::now())],
            )?;
        }
        transaction.commit()?;
        Ok(removed > 0)
    }

    fn touch_session(&self, session_id: &str) -> Result<()> {
        self.connection.execute(
            "UPDATE sessions SET updated_at_ms = ?2 WHERE id = ?1",
            params![session_id, database_timestamp_ms(SystemTime::now())],
        )?;
        Ok(())
    }

    pub fn history(
        &self,
        session_id: &str,
        limit: usize,
        before_index: Option<usize>,
    ) -> Result<Vec<(usize, JsonRpcExchange)>> {
        let mut rows = if let Some(before_index) = before_index {
            let mut statement = self.connection.prepare(
                "SELECT sequence, exchange_json
                 FROM exchanges
                 WHERE session_id = ?1 AND sequence <= ?2
                 ORDER BY sequence DESC
                 LIMIT ?3",
            )?;
            collect_exchanges(
                &mut statement,
                params![session_id, sqlite_index(before_index), sqlite_limit(limit)],
            )?
        } else {
            let mut statement = self.connection.prepare(
                "SELECT sequence, exchange_json
                 FROM exchanges
                 WHERE session_id = ?1
                 ORDER BY sequence DESC
                 LIMIT ?2",
            )?;
            collect_exchanges(&mut statement, params![session_id, sqlite_limit(limit)])?
        };
        rows.reverse();
        Ok(rows)
    }

    pub fn find(&self, query: &str, limit: usize, session_id: Option<&str>) -> Result<FindResults> {
        let sessions = self.find_sessions(query, limit, session_id)?;
        let exchanges = self.find_exchanges(query, limit, session_id)?;
        let annotations = self.find_annotations(query, limit, session_id)?;
        Ok(FindResults {
            query: query.to_string(),
            sessions,
            exchanges,
            annotations,
        })
    }

    fn find_sessions(
        &self,
        query: &str,
        limit: usize,
        session_id: Option<&str>,
    ) -> Result<Vec<SessionSummary>> {
        let mut statement = self.connection.prepare(
            "SELECT s.id, s.name, s.target, s.created_at_ms, s.updated_at_ms,
                    COUNT(e.id)
             FROM sessions s
             LEFT JOIN exchanges e ON e.session_id = s.id
             WHERE (?2 IS NULL OR s.id = ?2)
               AND (instr(lower(s.id), lower(?1)) > 0
                 OR instr(lower(s.name), lower(?1)) > 0
                 OR instr(lower(s.target), lower(?1)) > 0)
             GROUP BY s.id
             ORDER BY s.updated_at_ms DESC
             LIMIT ?3",
        )?;
        let rows = statement.query_map(
            params![query, session_id, sqlite_limit(limit)],
            session_summary,
        )?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    fn find_exchanges(
        &self,
        query: &str,
        limit: usize,
        session_id: Option<&str>,
    ) -> Result<Vec<FoundExchange>> {
        let mut statement = self.connection.prepare(
            "SELECT s.id, s.name, s.target, s.created_at_ms, s.updated_at_ms,
                    (SELECT COUNT(*) FROM exchanges count
                     WHERE count.session_id = s.id),
                    e.sequence, e.exchange_json
             FROM exchanges e
             JOIN sessions s ON s.id = e.session_id
             WHERE (?2 IS NULL OR s.id = ?2)
               AND instr(lower(e.exchange_json), lower(?1)) > 0
             ORDER BY s.updated_at_ms DESC, e.sequence DESC
             LIMIT ?3",
        )?;
        let rows = statement.query_map(params![query, session_id, sqlite_limit(limit)], |row| {
            let session = session_summary(row)?;
            let sequence = row.get::<_, i64>(6)?;
            let json = row.get::<_, String>(7)?;
            Ok((session, sequence, json))
        })?;
        rows.map(|row| {
            let (session, sequence, json) = row?;
            let exchange: SessionExchange = serde_json::from_str(&json)?;
            let exchange = exchange
                .try_into()
                .map_err(|error: String| anyhow!(error))?;
            Ok(FoundExchange {
                session,
                index: sequence.saturating_sub(1) as usize,
                exchange,
            })
        })
        .collect()
    }

    fn find_annotations(
        &self,
        query: &str,
        limit: usize,
        session_id: Option<&str>,
    ) -> Result<Vec<FoundAnnotation>> {
        let mut statement = self.connection.prepare(
            "SELECT s.id, s.name, s.target, s.created_at_ms, s.updated_at_ms,
                    (SELECT COUNT(*) FROM exchanges count
                     WHERE count.session_id = s.id),
                    a.id, a.exchange_index, a.panel, a.tab, a.start_line, a.end_line,
                    a.message, a.text_json, a.parent_id, a.author, a.created_at_ms
             FROM annotations a
             JOIN sessions s ON s.id = a.session_id
             WHERE (?2 IS NULL OR s.id = ?2)
               AND (instr(lower(a.id), lower(?1)) > 0
                 OR instr(lower(a.panel), lower(?1)) > 0
                 OR instr(lower(a.tab), lower(?1)) > 0
                 OR instr(lower(a.message), lower(?1)) > 0
                 OR instr(lower(a.text_json), lower(?1)) > 0)
             ORDER BY a.created_at_ms DESC
             LIMIT ?3",
        )?;
        let rows = statement.query_map(params![query, session_id, sqlite_limit(limit)], |row| {
            Ok(FoundAnnotation {
                session: session_summary(row)?,
                annotation: line_annotation(row, 6)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn record_messages(
        &mut self,
        active_session_id: &str,
        messages: &[JsonRpcMessage],
    ) -> Result<Vec<String>> {
        if messages.is_empty() {
            return Ok(Vec::new());
        }

        let transaction = self.connection.transaction()?;
        let mut session_ids = Vec::with_capacity(messages.len());
        for message in messages {
            let session_id = match message.direction {
                MessageDirection::Request => active_session_id.to_string(),
                MessageDirection::Response => pending_session(&transaction, message)?
                    .unwrap_or_else(|| active_session_id.to_string()),
            };

            match message.direction {
                MessageDirection::Request => insert_message(&transaction, &session_id, message)?,
                MessageDirection::Response => update_response(&transaction, &session_id, message)?,
            }
            transaction.execute(
                "UPDATE sessions SET updated_at_ms = ?2 WHERE id = ?1",
                params![session_id, database_timestamp_ms(message.timestamp)],
            )?;
            session_ids.push(session_id);
        }
        transaction.commit()?;
        Ok(session_ids)
    }

    pub fn append_exchanges(
        &mut self,
        session_id: &str,
        exchanges: &[JsonRpcExchange],
    ) -> Result<()> {
        if exchanges.is_empty() {
            return Ok(());
        }

        let transaction = self.connection.transaction()?;
        for (sequence, exchange) in
            (next_sequence(&transaction, session_id)?..).zip(exchanges.iter())
        {
            insert_exchange(&transaction, session_id, sequence, exchange)?;
        }
        transaction.execute(
            "UPDATE sessions SET updated_at_ms = ?2 WHERE id = ?1",
            params![session_id, database_timestamp_ms(SystemTime::now())],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn update_target(&self, session_id: &str, target: &str) -> Result<()> {
        self.connection.execute(
            "UPDATE sessions SET target = ?2, updated_at_ms = ?3 WHERE id = ?1",
            params![session_id, target, database_timestamp_ms(SystemTime::now())],
        )?;
        Ok(())
    }

    pub fn rename_session(&self, session_id: &str, name: &str) -> Result<bool> {
        let name = name.trim();
        validate_session_name(name)?;
        let renamed = self.connection.execute(
            "UPDATE sessions SET name = ?2, updated_at_ms = ?3 WHERE id = ?1",
            params![session_id, name, database_timestamp_ms(SystemTime::now())],
        )?;
        Ok(renamed > 0)
    }

    pub fn export_session(&self, session_id: &str) -> Result<Session> {
        let session = self
            .session(session_id)?
            .ok_or_else(|| anyhow!("session not found: {session_id}"))?;
        let exchanges = self
            .history(session_id, usize::MAX, None)?
            .into_iter()
            .map(|(_, exchange)| SessionExchange::from(&exchange))
            .collect();

        Ok(Session {
            schema_version: 1,
            exported_at_ms: timestamp_ms(SystemTime::now()),
            target: session.target,
            exchanges,
        })
    }
}

fn history_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("JSONRPC_DEBUGGER_CONFIG_DIR") {
        return Ok(PathBuf::from(path).join("sqlite.db"));
    }
    if let Some(path) = std::env::var_os("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(path)
            .join("jsonrpc-debugger")
            .join("sqlite.db"));
    }
    if let Some(path) = std::env::var_os("HOME") {
        return Ok(PathBuf::from(path).join(".config/jsonrpc-debugger/sqlite.db"));
    }

    bail!("HOME is not set; set JSONRPC_DEBUGGER_CONFIG_DIR for session history")
}

#[cfg(unix)]
fn set_private_permissions(directory: &Path, database: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
    fs::set_permissions(database, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private_permissions(_directory: &Path, _database: &Path) -> Result<()> {
    Ok(())
}

fn session_summary(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionSummary> {
    Ok(SessionSummary {
        id: row.get(0)?,
        name: row.get(1)?,
        target: row.get(2)?,
        created_at_ms: row.get::<_, i64>(3)?.max(0) as u64,
        updated_at_ms: row.get::<_, i64>(4)?.max(0) as u64,
        exchange_count: row.get::<_, i64>(5)?.max(0) as usize,
    })
}

fn line_annotation(row: &rusqlite::Row<'_>, offset: usize) -> rusqlite::Result<LineAnnotation> {
    let panel = match row.get::<_, String>(offset + 2)?.as_str() {
        "request" => Focus::RequestSection,
        "response" => Focus::ResponseSection,
        value => return Err(invalid_annotation_column(offset + 2, value)),
    };
    let tab = match row.get::<_, String>(offset + 3)?.as_str() {
        "headers" => DetailTab::Headers,
        "body" => DetailTab::Body,
        value => return Err(invalid_annotation_column(offset + 3, value)),
    };
    let text_json = row.get::<_, String>(offset + 7)?;
    let text = serde_json::from_str(&text_json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            offset + 7,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })?;
    Ok(LineAnnotation {
        parent_id: row.get(offset + 8)?,
        author: match row.get::<_, String>(offset + 9)?.as_str() {
            "user" => AnnotationAuthor::User,
            "agent" => AnnotationAuthor::Agent,
            "unknown" => AnnotationAuthor::Unknown,
            value => return Err(invalid_annotation_column(offset + 9, value)),
        },
        created_at_ms: row.get::<_, i64>(offset + 10)?.max(0) as u64,
        id: row.get(offset)?,
        exchange_index: row.get::<_, i64>(offset + 1)?.max(0) as usize,
        panel,
        tab,
        start_line: row.get::<_, i64>(offset + 4)?.max(1) as usize,
        end_line: row.get::<_, i64>(offset + 5)?.max(1) as usize,
        message: row.get(offset + 6)?,
        text,
    })
}

fn collect_exchanges<P>(
    statement: &mut rusqlite::Statement<'_>,
    params: P,
) -> Result<Vec<(usize, JsonRpcExchange)>>
where
    P: rusqlite::Params,
{
    let rows = statement.query_map(params, |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    })?;
    rows.map(|row| {
        let (sequence, json) = row?;
        let exchange: SessionExchange = serde_json::from_str(&json)?;
        let exchange = exchange
            .try_into()
            .map_err(|error: String| anyhow!(error))?;
        Ok((sequence.saturating_sub(1) as usize, exchange))
    })
    .collect()
}

fn pending_session(
    transaction: &Transaction<'_>,
    message: &JsonRpcMessage,
) -> Result<Option<String>> {
    transaction
        .query_row(
            "SELECT session_id
             FROM exchanges
             WHERE rpc_id = ?1 AND complete = 0
             ORDER BY id DESC
             LIMIT 1",
            [rpc_id(message)],
            |row| row.get(0),
        )
        .optional()
        .map_err(Into::into)
}

fn insert_message(
    transaction: &Transaction<'_>,
    session_id: &str,
    message: &JsonRpcMessage,
) -> Result<()> {
    let exchange = JsonRpcExchange {
        id: message.id.clone(),
        method: message.method.clone(),
        request: Some(message.clone()),
        response: None,
        timestamp: message.timestamp,
        transport: message.transport,
    };
    let sequence = next_sequence(transaction, session_id)?;
    insert_exchange(transaction, session_id, sequence, &exchange)
}

fn update_response(
    transaction: &Transaction<'_>,
    session_id: &str,
    message: &JsonRpcMessage,
) -> Result<()> {
    let pending = transaction
        .query_row(
            "SELECT id, exchange_json
             FROM exchanges
             WHERE session_id = ?1 AND rpc_id = ?2 AND complete = 0
             ORDER BY id DESC
             LIMIT 1",
            params![session_id, rpc_id(message)],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;

    let Some((id, json)) = pending else {
        let exchange = JsonRpcExchange {
            id: message.id.clone(),
            method: None,
            request: None,
            response: Some(message.clone()),
            timestamp: message.timestamp,
            transport: message.transport,
        };
        let sequence = next_sequence(transaction, session_id)?;
        return insert_exchange(transaction, session_id, sequence, &exchange);
    };

    let mut exchange: SessionExchange = serde_json::from_str(&json)?;
    exchange.response = Some(SessionMessage::from(message));
    transaction.execute(
        "UPDATE exchanges SET complete = 1, exchange_json = ?2 WHERE id = ?1",
        params![id, serde_json::to_string(&exchange)?],
    )?;
    Ok(())
}

fn next_sequence(transaction: &Transaction<'_>, session_id: &str) -> Result<i64> {
    let sequence = transaction.query_row(
        "SELECT COALESCE(MAX(sequence), 0) + 1 FROM exchanges WHERE session_id = ?1",
        [session_id],
        |row| row.get(0),
    )?;
    Ok(sequence)
}

fn insert_exchange(
    transaction: &Transaction<'_>,
    session_id: &str,
    sequence: i64,
    exchange: &JsonRpcExchange,
) -> Result<()> {
    let value = SessionExchange::from(exchange);
    transaction.execute(
        "INSERT INTO exchanges
         (session_id, sequence, rpc_id, method, complete, exchange_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            session_id,
            sequence,
            exchange
                .id
                .as_ref()
                .map(serde_json::to_string)
                .transpose()?
                .unwrap_or_else(|| "null".to_string()),
            exchange.method,
            i64::from(exchange.response.is_some() || exchange.is_notification()),
            serde_json::to_string(&value)?,
        ],
    )?;
    Ok(())
}

fn rpc_id(message: &JsonRpcMessage) -> String {
    message
        .id
        .as_ref()
        .map(serde_json::Value::to_string)
        .unwrap_or_else(|| "null".to_string())
}

fn annotation_panel(panel: Focus) -> Result<&'static str> {
    match panel {
        Focus::RequestSection => Ok("request"),
        Focus::ResponseSection => Ok("response"),
        Focus::MessageList | Focus::StatusHeader => bail!("annotation panel must show details"),
    }
}

fn validate_session_name(name: &str) -> Result<()> {
    if name.is_empty() || name.chars().count() > 80 || name.chars().any(char::is_control) {
        bail!("session name must be one line containing 1 to 80 characters");
    }
    Ok(())
}

fn annotation_tab(tab: DetailTab) -> &'static str {
    match tab {
        DetailTab::Headers => "headers",
        DetailTab::Body => "body",
    }
}

fn invalid_annotation_column(index: usize, value: &str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        index,
        rusqlite::types::Type::Text,
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("invalid annotation value: {value}"),
        )
        .into(),
    )
}

fn database_timestamp_ms(timestamp: SystemTime) -> i64 {
    i64::try_from(timestamp_ms(timestamp)).unwrap_or(i64::MAX)
}

fn sqlite_limit(limit: usize) -> i64 {
    i64::try_from(limit).unwrap_or(i64::MAX)
}

fn sqlite_index(index: usize) -> i64 {
    i64::try_from(index).unwrap_or(i64::MAX)
}

fn timestamp_ms(timestamp: SystemTime) -> u64 {
    timestamp
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::TransportType;
    use serde_json::json;
    use std::{
        collections::HashMap,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
    };

    fn request(id: u64) -> JsonRpcMessage {
        JsonRpcMessage {
            id: Some(json!(id)),
            method: Some("eth_chainId".to_string()),
            params: Some(json!([])),
            result: None,
            error: None,
            timestamp: UNIX_EPOCH + std::time::Duration::from_millis(id),
            direction: MessageDirection::Request,
            transport: TransportType::Http,
            headers: None,
        }
    }

    fn response(id: u64) -> JsonRpcMessage {
        JsonRpcMessage {
            id: Some(json!(id)),
            method: None,
            params: None,
            result: Some(json!("0x1")),
            error: None,
            timestamp: UNIX_EPOCH + std::time::Duration::from_millis(id + 1),
            direction: MessageDirection::Response,
            transport: TransportType::Http,
            headers: None,
        }
    }

    fn annotation(id: &str, exchange_index: usize) -> LineAnnotation {
        LineAnnotation {
            parent_id: None,
            author: crate::app::AnnotationAuthor::Unknown,
            created_at_ms: 0,
            id: id.to_string(),
            exchange_index,
            panel: Focus::ResponseSection,
            tab: DetailTab::Body,
            start_line: 2,
            end_line: 3,
            message: format!("annotation {id}"),
            text: vec!["first".to_string(), "second".to_string()],
        }
    }

    fn count_commits(store: &HistoryStore) -> Arc<AtomicUsize> {
        let commits = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&commits);
        store
            .connection
            .commit_hook(Some(move || {
                observed.fetch_add(1, Ordering::Relaxed);
                false
            }))
            .unwrap();
        commits
    }

    #[test]
    fn recording_request_and_response_separately_commits_twice() {
        let mut store = HistoryStore::in_memory().unwrap();
        let session = store.create_session(Some("chain"), "http://node").unwrap();
        let commits = count_commits(&store);

        store.record_messages(&session.id, &[request(1)]).unwrap();
        store.record_messages(&session.id, &[response(1)]).unwrap();

        assert_eq!(commits.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn recording_request_and_response_together_commits_once() {
        let mut store = HistoryStore::in_memory().unwrap();
        let session = store.create_session(Some("chain"), "http://node").unwrap();
        let commits = count_commits(&store);

        let recorded_sessions = store
            .record_messages(
                &session.id,
                &[request(1), request(2), response(2), response(1)],
            )
            .unwrap();

        assert_eq!(recorded_sessions, vec![session.id.clone(); 4]);
        assert_eq!(commits.load(Ordering::Relaxed), 1);
        let exchanges = store.load_session(&session.id).unwrap().1;
        assert_eq!(exchanges.len(), 2);
        assert!(exchanges.iter().all(|exchange| exchange.response.is_some()));
    }

    #[test]
    fn records_and_pages_session_history() {
        let mut store = HistoryStore::in_memory().unwrap();
        let session = store.create_session(Some("chain"), "http://node").unwrap();
        for id in 1..=3 {
            store.record_messages(&session.id, &[request(id)]).unwrap();
            store.record_messages(&session.id, &[response(id)]).unwrap();
        }

        let recent = store.history(&session.id, 2, None).unwrap();
        let older = store.history(&session.id, 2, Some(recent[0].0)).unwrap();

        assert_eq!(
            recent.iter().map(|(index, _)| *index).collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(
            older.iter().map(|(index, _)| *index).collect::<Vec<_>>(),
            vec![0]
        );
        assert!(recent[1].1.response.is_some());
    }

    #[test]
    fn keeps_late_responses_in_their_original_session() {
        let mut store = HistoryStore::in_memory().unwrap();
        let first = store.create_session(Some("first"), "").unwrap();
        let second = store.create_session(Some("second"), "").unwrap();
        store.record_messages(&first.id, &[request(1)]).unwrap();

        let recorded_sessions = store
            .record_messages(&second.id, &[request(2), response(1)])
            .unwrap();

        assert_eq!(recorded_sessions, vec![second.id.clone(), first.id.clone()]);
        assert!(store.load_session(&first.id).unwrap().1[0]
            .response
            .is_some());
        assert!(store.load_session(&second.id).unwrap().1[0]
            .response
            .is_none());
    }

    #[test]
    fn notifications_are_complete_exchanges() {
        let mut store = HistoryStore::in_memory().unwrap();
        let session = store.create_session(Some("notifications"), "").unwrap();
        let mut notification = request(1);
        notification.id = None;
        store.record_messages(&session.id, &[notification]).unwrap();

        let exchanges = store.load_session(&session.id).unwrap().1;
        assert_eq!(exchanges.len(), 1);
        assert!(exchanges[0].is_notification());
    }

    #[test]
    fn survives_reopening_the_database() {
        let path =
            std::env::temp_dir().join(format!("jsonrpc-debugger-{}.sqlite3", Uuid::new_v4()));
        let session_id = {
            let mut store = HistoryStore::open(&path).unwrap();
            let session = store.create_session(Some("saved"), "http://node").unwrap();
            store
                .record_messages(&session.id, &[request(1), response(1)])
                .unwrap();
            store
                .add_annotation(&session.id, &annotation("saved-note", 0))
                .unwrap();
            session.id
        };

        let store = HistoryStore::open(&path).unwrap();
        let exchanges = store.load_session(&session_id).unwrap().1;
        assert_eq!(exchanges.len(), 1);
        assert!(exchanges[0].response.is_some());
        assert_eq!(
            store.load_session(&session_id).unwrap().2[0].id,
            "saved-note"
        );
        drop(store);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn removes_annotations_individually() {
        let mut store = HistoryStore::in_memory().unwrap();
        let session = store.create_session(Some("notes"), "").unwrap();
        store
            .add_annotation(&session.id, &annotation("first", 0))
            .unwrap();
        store
            .add_annotation(&session.id, &annotation("second", 0))
            .unwrap();
        store
            .add_annotation(&session.id, &annotation("other-exchange", 1))
            .unwrap();

        assert!(store.remove_annotation(&session.id, "first").unwrap());
        assert!(store.remove_annotation(&session.id, "second").unwrap());
        assert_eq!(
            store.annotations(&session.id).unwrap()[0].id,
            "other-exchange"
        );
    }

    #[test]
    fn updates_an_annotation_message_in_place() {
        let mut store = HistoryStore::in_memory().unwrap();
        let session = store.create_session(Some("notes"), "").unwrap();
        store
            .add_annotation(&session.id, &annotation("note", 0))
            .unwrap();

        assert!(store
            .update_annotation(&session.id, "note", "Edited note")
            .unwrap());
        let saved = store.annotations(&session.id).unwrap();
        assert_eq!(saved[0].id, "note");
        assert_eq!(saved[0].message, "Edited note");
        assert!(!store
            .update_annotation(&session.id, "missing", "No note")
            .unwrap());
    }

    #[test]
    fn renames_a_session() {
        let mut store = HistoryStore::in_memory().unwrap();
        let session = store.create_session(Some("old"), "").unwrap();

        assert!(store.rename_session(&session.id, "Refunds").unwrap());
        assert_eq!(store.session(&session.id).unwrap().unwrap().name, "Refunds");
        assert!(store.rename_session(&session.id, "").is_err());
    }

    #[test]
    fn finds_sessions_exchanges_and_annotations_without_changing_sessions() {
        let mut store = HistoryStore::in_memory().unwrap();
        let first = store
            .create_session(Some("Refund investigation"), "http://builder.example")
            .unwrap();
        let second = store.create_session(Some("Other"), "http://node").unwrap();
        let mut matching_request = request(1);
        matching_request.method = Some("mev_getRefunds".to_string());
        matching_request.params = Some(json!({"recipient": "0xDeadBeef", "share": "100%"}));
        matching_request.headers = Some(HashMap::from([(
            "x-trace".to_string(),
            "Trace-Needle".to_string(),
        )]));
        store
            .record_messages(&first.id, &[matching_request, response(1)])
            .unwrap();
        store.record_messages(&second.id, &[request(2)]).unwrap();
        let mut note = annotation("refund-note", 0);
        note.message = "Compare payout recipient".to_string();
        note.text = vec!["0xDeadBeef receives the refund".to_string()];
        store.add_annotation(&first.id, &note).unwrap();

        let session_results = store.find("REFUND INVESTIGATION", 10, None).unwrap();
        assert_eq!(
            session_results.sessions,
            vec![store.session(&first.id).unwrap().unwrap()]
        );
        assert!(session_results.exchanges.is_empty());
        assert!(session_results.annotations.is_empty());

        let exchange_results = store.find("trace-needle", 10, None).unwrap();
        assert_eq!(exchange_results.exchanges.len(), 1);
        assert_eq!(exchange_results.exchanges[0].session.id, first.id);
        assert_eq!(exchange_results.exchanges[0].index, 0);
        let exchange_results = crate::control::find_results(exchange_results);
        assert!(exchange_results["exchanges"][0]["references"]
            .as_array()
            .unwrap()
            .iter()
            .any(|reference| reference["text"] == "  x-trace: Trace-Needle"));

        let literal_results = store.find("100%", 10, None).unwrap();
        assert_eq!(literal_results.exchanges.len(), 1);

        let annotation_results = store.find("PAYOUT RECIPIENT", 10, None).unwrap();
        assert_eq!(annotation_results.annotations.len(), 1);
        assert_eq!(
            annotation_results.annotations[0].annotation.id,
            "refund-note"
        );
        let annotation_results = crate::control::find_results(annotation_results);
        assert_eq!(
            annotation_results["annotations"][0]["reference"]["sessionId"],
            first.id
        );
        assert_eq!(
            annotation_results["annotations"][0]["reference"]["exchangeIndex"],
            0
        );

        let scoped_results = store.find("eth_chainId", 10, Some(&first.id)).unwrap();
        assert!(scoped_results.exchanges.is_empty());

        let empty_results = store.find("refund", 0, None).unwrap();
        assert!(empty_results.sessions.is_empty());
        assert!(empty_results.exchanges.is_empty());
        assert!(empty_results.annotations.is_empty());
    }

    #[test]
    fn migrates_the_redundant_index_without_losing_history_or_uniqueness() {
        let mut store = HistoryStore::in_memory().unwrap();
        let session = store
            .create_session(Some("migrate"), "http://node")
            .unwrap();
        store
            .record_messages(&session.id, &[request(1), response(1), request(2)])
            .unwrap();
        store
            .connection
            .execute_batch(
                "DROP TABLE annotations;
             CREATE INDEX exchanges_session_sequence ON exchanges(session_id, sequence);
             PRAGMA user_version = 2;",
            )
            .unwrap();
        let store = HistoryStore::from_connection(store.connection).unwrap();
        assert_eq!(store.history(&session.id, 10, None).unwrap().len(), 2);
        assert!(store.history(&session.id, 1, Some(1)).unwrap()[0]
            .1
            .response
            .is_some());
        let index_count: i64 = store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name='exchanges_session_sequence'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(index_count, 0);
        assert!(store
            .connection
            .execute(
                "INSERT INTO exchanges (session_id, sequence, rpc_id, complete, exchange_json)
             SELECT session_id, sequence, rpc_id, complete, exchange_json FROM exchanges LIMIT 1",
                []
            )
            .is_err());
        let version: i64 = store
            .connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn expands_an_existing_history_database_without_losing_sessions() {
        let path =
            std::env::temp_dir().join(format!("jsonrpc-debugger-{}.sqlite3", Uuid::new_v4()));
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE sessions (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    target TEXT NOT NULL,
                    created_at_ms INTEGER NOT NULL,
                    updated_at_ms INTEGER NOT NULL
                );
                CREATE TABLE exchanges (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                    sequence INTEGER NOT NULL,
                    rpc_id TEXT NOT NULL,
                    method TEXT,
                    complete INTEGER NOT NULL,
                    exchange_json TEXT NOT NULL,
                    UNIQUE(session_id, sequence)
                );
                INSERT INTO sessions VALUES ('existing', 'Existing', 'http://node', 1, 1);
                PRAGMA user_version = 1;",
            )
            .unwrap();
        drop(connection);

        let store = HistoryStore::open(&path).unwrap();
        assert_eq!(store.session("existing").unwrap().unwrap().name, "Existing");
        store
            .add_annotation("existing", &annotation("new-note", 0))
            .unwrap();
        assert_eq!(store.annotations("existing").unwrap().len(), 1);
        drop(store);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn migrates_legacy_notes_with_their_original_times_and_text() {
        let mut store = HistoryStore::in_memory().unwrap();
        let session = store.create_session(Some("legacy"), "http://node").unwrap();
        let mut note = annotation("legacy-note", 0);
        note.created_at_ms = 1_700_000_123_456;
        store.add_annotation(&session.id, &note).unwrap();
        store
            .connection
            .execute_batch(
                "DROP INDEX annotations_parent;
             ALTER TABLE annotations DROP COLUMN parent_id;
             ALTER TABLE annotations DROP COLUMN author;
             PRAGMA user_version = 3;",
            )
            .unwrap();
        let store = HistoryStore::from_connection(store.connection).unwrap();
        assert_eq!(store.annotations(&session.id).unwrap(), vec![note.clone()]);
        let mut reply = annotation("reply", 0);
        reply.parent_id = Some(note.id.clone());
        reply.author = AnnotationAuthor::User;
        reply.created_at_ms = note.created_at_ms + 1;
        store.add_annotation(&session.id, &reply).unwrap();
        // Opening an already-migrated database must be idempotent.
        let store = HistoryStore::from_connection(store.connection).unwrap();
        assert_eq!(store.annotations(&session.id).unwrap(), vec![note, reply]);
    }

    #[test]
    fn replies_require_the_same_session_and_source_reference() {
        let mut store = HistoryStore::in_memory().unwrap();
        let session = store.create_session(Some("one"), "http://node").unwrap();
        let other = store.create_session(Some("two"), "http://node").unwrap();
        store
            .add_annotation(&session.id, &annotation("root", 0))
            .unwrap();
        let mut reply = annotation("reply", 0);
        reply.parent_id = Some("missing".to_string());
        assert!(store.add_annotation(&session.id, &reply).is_err());
        reply.parent_id = Some("root".to_string());
        assert!(store.add_annotation(&other.id, &reply).is_err());
        for mismatched in [
            LineAnnotation {
                exchange_index: 1,
                ..reply.clone()
            },
            LineAnnotation {
                panel: Focus::RequestSection,
                ..reply.clone()
            },
            LineAnnotation {
                tab: DetailTab::Headers,
                ..reply.clone()
            },
            LineAnnotation {
                start_line: 1,
                ..reply.clone()
            },
            LineAnnotation {
                end_line: 4,
                ..reply.clone()
            },
        ] {
            assert!(store.add_annotation(&session.id, &mismatched).is_err());
        }
        assert_eq!(store.annotations(&session.id).unwrap().len(), 1);
        store.add_annotation(&session.id, &reply).unwrap();
        assert_eq!(store.annotations(&session.id).unwrap().len(), 2);
    }

    #[test]
    fn deleting_a_note_reparents_children_and_keeps_descendants_after_reopening() {
        let mut store = HistoryStore::in_memory().unwrap();
        let session = store.create_session(Some("thread"), "http://node").unwrap();
        for (id, parent, time) in [
            ("root", None, 1),
            ("reply", Some("root"), 2),
            ("nested", Some("reply"), 3),
        ] {
            let mut note = annotation(id, 0);
            note.parent_id = parent.map(str::to_string);
            note.created_at_ms = time;
            note.author = AnnotationAuthor::Agent;
            store.add_annotation(&session.id, &note).unwrap();
        }
        assert!(store.remove_annotation(&session.id, "reply").unwrap());
        let notes = store.annotations(&session.id).unwrap();
        assert_eq!(notes.len(), 2);
        assert_eq!(notes[1].parent_id.as_deref(), Some("root"));
        let original = notes[1].clone();
        assert!(store
            .update_annotation(&session.id, "nested", "Edited")
            .unwrap());
        assert!(store.remove_annotation(&session.id, "root").unwrap());
        let store = HistoryStore::from_connection(store.connection).unwrap();
        assert_eq!(
            store.annotations(&session.id).unwrap(),
            vec![LineAnnotation {
                parent_id: None,
                message: "Edited".to_string(),
                ..original
            }]
        );
    }
}
