use crate::{
    app::{
        DetailTab, Focus, JsonRpcExchange, JsonRpcMessage, LineAnnotation, MessageDirection,
        SessionSummary,
    },
    control::{Session, SessionExchange, SessionMessage},
};
use anyhow::{anyhow, bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

const SCHEMA_VERSION: i64 = 2;

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

        connection.execute_batch(
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
            CREATE INDEX IF NOT EXISTS exchanges_session_sequence
                ON exchanges(session_id, sequence);
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
            PRAGMA user_version = 2;
            ",
        )?;

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
            "SELECT id, exchange_index, panel, tab, start_line, end_line, message, text_json
             FROM annotations
             WHERE session_id = ?1
             ORDER BY created_at_ms, id",
        )?;
        let rows = statement.query_map([session_id], |row| {
            let panel = match row.get::<_, String>(2)?.as_str() {
                "request" => Focus::RequestSection,
                "response" => Focus::ResponseSection,
                value => return Err(invalid_annotation_column(2, value)),
            };
            let tab = match row.get::<_, String>(3)?.as_str() {
                "headers" => DetailTab::Headers,
                "body" => DetailTab::Body,
                value => return Err(invalid_annotation_column(3, value)),
            };
            let text_json = row.get::<_, String>(7)?;
            let text = serde_json::from_str(&text_json).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    7,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?;
            Ok(LineAnnotation {
                id: row.get(0)?,
                exchange_index: row.get::<_, i64>(1)?.max(0) as usize,
                panel,
                tab,
                start_line: row.get::<_, i64>(4)?.max(1) as usize,
                end_line: row.get::<_, i64>(5)?.max(1) as usize,
                message: row.get(6)?,
                text,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn add_annotation(&self, session_id: &str, annotation: &LineAnnotation) -> Result<()> {
        let panel = annotation_panel(annotation.panel)?;
        self.connection.execute(
            "INSERT INTO annotations (
                 id, session_id, exchange_index, panel, tab, start_line, end_line,
                 message, text_json, created_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
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
                database_timestamp_ms(SystemTime::now()),
            ],
        )?;
        self.touch_session(session_id)?;
        Ok(())
    }

    pub fn remove_annotation(&self, session_id: &str, id: &str) -> Result<bool> {
        let removed = self.connection.execute(
            "DELETE FROM annotations WHERE session_id = ?1 AND id = ?2",
            params![session_id, id],
        )?;
        if removed > 0 {
            self.touch_session(session_id)?;
        }
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

    pub fn record_message(
        &mut self,
        active_session_id: &str,
        message: &JsonRpcMessage,
    ) -> Result<String> {
        let transaction = self.connection.transaction()?;
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
        transaction.commit()?;
        Ok(session_id)
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
        let mut sequence = next_sequence(&transaction, session_id)?;
        for exchange in exchanges {
            insert_exchange(&transaction, session_id, sequence, exchange)?;
            sequence += 1;
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
        transport: message.transport.clone(),
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
            transport: message.transport.clone(),
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
            i64::from(exchange.response.is_some()),
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

    #[test]
    fn records_and_pages_session_history() {
        let mut store = HistoryStore::in_memory().unwrap();
        let session = store.create_session(Some("chain"), "http://node").unwrap();
        for id in 1..=3 {
            store.record_message(&session.id, &request(id)).unwrap();
            store.record_message(&session.id, &response(id)).unwrap();
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
        store.record_message(&first.id, &request(1)).unwrap();

        let recorded_session = store.record_message(&second.id, &response(1)).unwrap();

        assert_eq!(recorded_session, first.id);
        assert!(store.load_session(&first.id).unwrap().1[0]
            .response
            .is_some());
        assert!(store.load_session(&second.id).unwrap().1.is_empty());
    }

    #[test]
    fn survives_reopening_the_database() {
        let path =
            std::env::temp_dir().join(format!("jsonrpc-debugger-{}.sqlite3", Uuid::new_v4()));
        let session_id = {
            let mut store = HistoryStore::open(&path).unwrap();
            let session = store.create_session(Some("saved"), "http://node").unwrap();
            store.record_message(&session.id, &request(1)).unwrap();
            store
                .add_annotation(&session.id, &annotation("saved-note", 0))
                .unwrap();
            session.id
        };

        let store = HistoryStore::open(&path).unwrap();
        assert_eq!(store.load_session(&session_id).unwrap().1.len(), 1);
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
    fn renames_a_session() {
        let mut store = HistoryStore::in_memory().unwrap();
        let session = store.create_session(Some("old"), "").unwrap();

        assert!(store.rename_session(&session.id, "Refunds").unwrap());
        assert_eq!(store.session(&session.id).unwrap().unwrap().name, "Refunds");
        assert!(store.rename_session(&session.id, "").is_err());
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
}
