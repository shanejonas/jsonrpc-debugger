//! Bounded resident payloads with stable, lightweight rows and an ephemeral SQLite spill file.
use crate::app::{JsonRpcExchange, JsonRpcMessage, TransportType};
use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use std::{borrow::Cow, collections::VecDeque, time::SystemTime};

const RESIDENT_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct ExchangeSummary {
    pub id: Option<serde_json::Value>,
    pub method: Option<String>,
    pub transport: TransportType,
    pub request_timestamp: Option<SystemTime>,
    pub response_timestamp: Option<SystemTime>,
    pub has_error: bool,
    notification: bool,
}

impl ExchangeSummary {
    pub fn is_notification(&self) -> bool {
        self.notification
    }
}

impl From<&JsonRpcExchange> for ExchangeSummary {
    fn from(exchange: &JsonRpcExchange) -> Self {
        Self {
            id: exchange.id.clone(),
            method: exchange.method.clone(),
            transport: exchange.transport,
            request_timestamp: exchange.request.as_ref().map(|message| message.timestamp),
            response_timestamp: exchange.response.as_ref().map(|message| message.timestamp),
            has_error: exchange
                .response
                .as_ref()
                .is_some_and(|message| message.error.is_some()),
            notification: exchange.is_notification(),
        }
    }
}

#[derive(Default)]
pub struct ExchangeStore {
    summaries: Vec<ExchangeSummary>,
    payloads: Vec<Option<Box<JsonRpcExchange>>>,
    sizes: Vec<usize>,
    resident_bytes: usize,
    resident: VecDeque<usize>,
    spill: Option<Connection>,
}

impl ExchangeStore {
    pub fn len(&self) -> usize {
        self.summaries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.summaries.is_empty()
    }

    pub fn summaries(&self) -> &[ExchangeSummary] {
        &self.summaries
    }

    pub fn get(&self, index: usize) -> Result<Option<Cow<'_, JsonRpcExchange>>> {
        let Some(payload) = self.payloads.get(index) else {
            return Ok(None);
        };
        if let Some(exchange) = payload {
            return Ok(Some(Cow::Borrowed(exchange.as_ref())));
        }
        let json: String = self
            .spill
            .as_ref()
            .context("missing exchange cache")?
            .query_row(
                "SELECT payload FROM payloads WHERE id = ?1",
                [index as i64],
                |row| row.get(0),
            )
            .context("read cached exchange")?;
        Ok(Some(Cow::Owned(
            serde_json::from_str(&json).context("decode cached exchange")?,
        )))
    }

    #[allow(dead_code)] // Library convenience API.
    pub fn last(&self) -> Result<Option<Cow<'_, JsonRpcExchange>>> {
        match self.len().checked_sub(1) {
            Some(index) => self.get(index),
            None => Ok(None),
        }
    }

    #[allow(dead_code)] // Library convenience API.
    pub fn iter(&self) -> impl Iterator<Item = Result<Cow<'_, JsonRpcExchange>>> {
        (0..self.len()).map(|index| {
            self.get(index)
                .map(|exchange| exchange.expect("index is in bounds"))
        })
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub(crate) fn push(&mut self, exchange: JsonRpcExchange) -> Result<()> {
        let size = exchange_size(&exchange);
        self.resident_bytes += size;
        self.resident.push_back(self.len());
        self.summaries.push(ExchangeSummary::from(&exchange));
        self.sizes.push(size);
        self.payloads.push(Some(Box::new(exchange)));
        self.evict()
    }

    pub(crate) fn replace(&mut self, index: usize, exchange: JsonRpcExchange) -> Result<()> {
        self.summaries[index] = ExchangeSummary::from(&exchange);
        if self.payloads[index].is_none() {
            self.resident.push_back(index);
        } else {
            self.resident_bytes -= self.sizes[index];
        }
        self.sizes[index] = exchange_size(&exchange);
        self.resident_bytes += self.sizes[index];
        self.payloads[index] = Some(Box::new(exchange));
        self.evict()
    }

    pub(crate) fn load(&mut self, index: usize) -> Result<()> {
        if self.payloads[index].is_some() {
            return Ok(());
        }
        let exchange = self
            .get(index)?
            .expect("pending index is in bounds")
            .into_owned();
        self.resident_bytes += self.sizes[index];
        self.resident.push_back(index);
        self.payloads[index] = Some(Box::new(exchange));
        Ok(())
    }

    pub(crate) fn set_response(&mut self, index: usize, message: JsonRpcMessage) -> Result<()> {
        let size = message_size(&message);
        let exchange = self.payloads[index]
            .as_mut()
            .expect("load before updating response");
        debug_assert!(exchange.response.is_none());
        exchange.response = Some(message);
        self.summaries[index] = ExchangeSummary::from(exchange.as_ref());
        self.sizes[index] += size;
        self.resident_bytes += size;
        self.evict()
    }

    fn evict(&mut self) -> Result<()> {
        if self.resident_bytes <= RESIDENT_BYTES {
            return Ok(());
        }
        if self.spill.is_none() {
            // SQLite owns and deletes the temporary file on close. Keep its page cache small.
            let connection = Connection::open("").context("open temporary exchange cache")?;
            connection.execute_batch("PRAGMA cache_size = -512; PRAGMA journal_mode = MEMORY; PRAGMA synchronous = OFF; CREATE TABLE payloads (id INTEGER PRIMARY KEY, payload TEXT NOT NULL);")?;
            self.spill = Some(connection);
        }
        while self.resident_bytes > RESIDENT_BYTES {
            let index = self.resident[0];
            let json =
                serde_json::to_string(self.payloads[index].as_ref().expect("resident payload"))?;
            self.spill
                .as_ref()
                .unwrap()
                .execute(
                    "INSERT OR REPLACE INTO payloads (id, payload) VALUES (?1, ?2)",
                    params![index as i64, json],
                )
                .context("write cached exchange")?;
            // Never discard a payload until its write succeeds.
            self.payloads[index] = None;
            self.resident_bytes -= self.sizes[index];
            self.resident.pop_front();
        }
        Ok(())
    }
}

// Approximate heap occupancy, including Value containers rather than only their JSON bytes.
fn value_size(value: &serde_json::Value) -> usize {
    use serde_json::Value;
    std::mem::size_of::<Value>()
        + match value {
            Value::String(text) => text.capacity(),
            Value::Array(values) => values.iter().map(value_size).sum(),
            Value::Object(values) => values
                .iter()
                .map(|(key, value)| key.capacity() + 64 + value_size(value))
                .sum(),
            _ => 0,
        }
}

fn message_size(message: &JsonRpcMessage) -> usize {
    message.method.as_ref().map_or(0, String::capacity)
        + [
            &message.id,
            &message.params,
            &message.result,
            &message.error,
        ]
        .into_iter()
        .flatten()
        .map(value_size)
        .sum::<usize>()
        + message.headers.as_ref().map_or(0, |headers| {
            headers
                .iter()
                .map(|(name, value)| 64 + name.capacity() + value.capacity())
                .sum()
        })
}

fn exchange_size(exchange: &JsonRpcExchange) -> usize {
    std::mem::size_of::<JsonRpcExchange>()
        + [&exchange.request, &exchange.response]
            .into_iter()
            .flatten()
            .map(message_size)
            .sum::<usize>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{App, JsonRpcMessage, MessageDirection};
    use serde_json::json;

    fn request(id: usize) -> JsonRpcExchange {
        let timestamp = SystemTime::now();
        JsonRpcExchange {
            id: Some(id.into()),
            method: Some("test/archive".into()),
            timestamp,
            transport: TransportType::Http,
            response: None,
            request: Some(JsonRpcMessage {
                id: Some(id.into()),
                method: Some("test/archive".into()),
                params: Some(json!({"blob": "x".repeat(5 * 1024 * 1024)})),
                result: None,
                error: None,
                timestamp,
                direction: MessageDirection::Request,
                transport: TransportType::Http,
                headers: None,
            }),
        }
    }

    #[test]
    fn evicts_payloads_and_reads_without_growing_the_resident_cache() {
        let mut store = ExchangeStore::default();
        let first = request(1);
        let expected = serde_json::to_value(&first).unwrap();
        store.push(first).unwrap();
        store.push(request(2)).unwrap();
        assert!(store.payloads[0].is_none());
        assert!(store.resident_bytes <= RESIDENT_BYTES);
        assert_eq!(store.summaries()[0].method.as_deref(), Some("test/archive"));
        for _ in 0..2 {
            assert_eq!(
                serde_json::to_value(store.get(0).unwrap().unwrap()).unwrap(),
                expected
            );
            assert!(store.payloads[0].is_none());
            assert_eq!(store.resident.len(), 1);
        }
        store.clear();
        assert!(store.is_empty());
        assert!(store.spill.is_none());
    }

    #[test]
    fn late_response_completes_the_original_evicted_request() {
        let mut app = App::new();
        app.add_message(request(1).request.unwrap());
        app.add_message(request(2).request.unwrap());
        assert!(app.exchanges().payloads[0].is_none());
        let mut response = request(1).request.unwrap();
        response.direction = MessageDirection::Response;
        response.method = None;
        response.params = None;
        response.result = Some(json!({"done": true}));
        app.add_message(response);
        assert_eq!(app.exchanges().len(), 2);
        let exchange = app.exchanges().get(0).unwrap().unwrap();
        assert_eq!(
            exchange.request.as_ref().unwrap().params.as_ref().unwrap()["blob"]
                .as_str()
                .unwrap()
                .len(),
            5 * 1024 * 1024
        );
        assert_eq!(
            exchange.response.as_ref().unwrap().result,
            Some(json!({"done": true}))
        );
        assert_eq!(app.pending_exchange_indices().collect::<Vec<_>>(), [1]);
        assert!(app.exchanges().resident_bytes <= RESIDENT_BYTES);
    }

    #[test]
    fn failed_spill_keeps_the_payload_in_memory() {
        let mut store = ExchangeStore::default();
        store.push(request(1)).unwrap();
        store.push(request(2)).unwrap();
        store.spill.as_ref().unwrap().execute_batch(
            "CREATE TRIGGER reject_writes BEFORE INSERT ON payloads BEGIN SELECT RAISE(FAIL, 'disk failure'); END;"
        ).unwrap();
        assert!(store.push(request(3)).is_err());
        assert_eq!(store.len(), 3);
        assert!(store.payloads[1].is_some());
        assert!(store.payloads[2].is_some());
        assert_eq!(store.get(2).unwrap().unwrap().id, Some(json!(3)));
        assert_eq!(store.get(0).unwrap().unwrap().id, Some(json!(1)));
    }
}
