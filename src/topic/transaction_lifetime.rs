use crate::measurement::{self, IntoWriteQuery};
use crate::subscribe::{Subscription, Topic};
use chrono::{DateTime, Utc};
use ckb_suite_rpc::{ckb_jsonrpc_types::PoolTransactionEntry, Jsonrpc};
use ckb_types::{core::TransactionView, packed::Byte32, prelude::*};
use crossbeam::channel::Sender;
use influxdb::{Timestamp, WriteQuery};
use jsonrpc_core::{futures::Stream, serde_from_str};
use jsonrpc_server_utils::tokio::prelude::*;
use std::collections::HashMap;
use std::time::Instant;

/// This module aims to the pool transactions
/// This module produces the below measures:
///
/// ### How it works?
///
/// We dont care about chain reorganization at present, which means it will not observe orphan transactions.
///
/// ### Why measure it?
///
/// Know the transaction traffic and estimate the mean waiting time for a pending transaction become from pending to committed.
///
/// ### Note
///
/// Consider a dashboard that tells a story about transaction events.
///
/// List known transaction events here:
///   - "pending", start pending in pool;
///   - "pending_long", being pending for too long;
///   - "propose", transit from pending to proposed, with the delay duration, proposed block number;
///   - "propose_long", expire proposal window, with the delay duration;
///   - "commit", transit from proposed to committed, with the delay duration, delay block count, proposed block number, committed block number;
///   - "reject", reject by pool via subscribing "RejectTransaction"; it always contains explicit rejecting reason;
///   - "remove", remove with known or unknown reason;
///
/// The serie may be like below. Some of events are used to visualize in heatmap, some in table:
/// ```json
/// {
///   "query_name": "transaction_lifetime",
///
///   "transaction_hash": h256,
///
///   "event": "pending"/"pending_long"/"propose"/"propose_long"/"commit"/"reject"/"remove",
///
///   "extra": message, default is empty;
///         if event is "propose"/"commit", this field is corresponding block number and hash;
///         if event is "reject"/"remove", this field is corresponding reason;
///
///   "time": time instant;
///         if event is "pending", assign it `now`;
///         if event is "pending_long", keeps this field the same as "pending";
///         if event is "propose", assign it proposed block timestamp;
///         if event is "propose_long", keeps this field the same as "propose";
///         if event is "commit", assign it committed block timestamp;
///         if event is "reject", assign it `now`;
///         if event is "remove", assign it `now`;
///
///   "elapsed": duration;
///         if event is "pending", assign it 0;
///         if event is "pending_long", assign `now - pending.time`;
///         if event is "propose", assign `proposed.time - pending.time`;
///         if event is "propose_long", assign `now - proposed.time`;
///         if event is "commit", assign `committed.time - proposed.time";
///         if event is "reject", assign it 0;
///         if event is "remove", assign it `now - pending.time`;
/// }
/// ```
///
/// * maintaining fields:
///   - pending
///   - proposed
///
/// * Information source:
///   - Subscription of RejectTransaction
///     Pop out the transaction; produce a "reject" event
///   - Subscription of NewTransaction
///     Retrieve transaction status via `get_transaction`, then put into self.pending or
///     self.proposed; if status is pending, then produce a "pending" event, otherwise produce no
///     any events.
///
/// * Periodically checking
///   Travels transactions in self.pending and self.proposed, check the transaction status
///   transition;
///
/// * TODO
///   - Retrieve all pool transactions via RPC `get_raw_tx_pool` at the start.
///   - Use ticker to trigger checking
///   - Unify the time type
///   - unify the `time` and `waiting_duration` definition for vary pool events
///   - filter duplicated notifications. i am not sure that ckb handle duplicated

pub(crate) struct Handler {
    waiting: HashMap<Byte32, DateTime<Utc>>,
    pending: HashMap<Byte32, Timestamp>,
    proposed: HashMap<Byte32, Timestamp>,

    jsonrpc: Jsonrpc,
    new_tx_subscriber: jsonrpc_server_utils::tokio::sync::mpsc::Receiver<String>,
    query_sender: Sender<WriteQuery>,
    last_checking_at: Instant,
}

impl Handler {
    pub(crate) fn new(
        ckb_rpc_url: String,
        ckb_subscribe_url: String,
        query_sender: Sender<WriteQuery>,
    ) -> (Self, Subscription) {
        let jsonrpc = Jsonrpc::connect(&ckb_rpc_url);
        let (new_tx_notifier, new_tx_subscriber) =
            jsonrpc_server_utils::tokio::sync::mpsc::channel(100);
        let subscription =
            Subscription::new(ckb_subscribe_url, Topic::NewTransaction, new_tx_notifier);
        (
            Self {
                new_tx_subscriber,
                jsonrpc,
                query_sender,
                waiting: Default::default(),
                pending: Default::default(),
                proposed: Default::default(),
                last_checking_at: Instant::now(),
            },
            subscription,
        )
    }

    pub(crate) async fn run(mut self) {
        // Take out the tx_receiver to pass Rust borrow rule
        let new_tx_subscriber = {
            let (_, mut dummy) = jsonrpc_server_utils::tokio::sync::mpsc::channel(100);
            ::std::mem::swap(&mut self.new_tx_subscriber, &mut dummy);
            dummy
        };
        new_tx_subscriber
            .for_each(|message| {
                let transaction = serde_from_str::<PoolTransactionEntry>(&message)
                    .map(|pt| Into::<ckb_types::packed::Transaction>::into(pt.transaction.inner))
                    .map(|tx| tx.into_view())
                    .unwrap();
                self.receive_new(&transaction);

                if self.last_checking_at.elapsed() >= ::std::time::Duration::from_secs(1 * 60) {
                    self.last_checking_at = Instant::now();
                    self.report_waiting_total();
                    self.travel();
                }
                Ok(())
            })
            .wait()
            .unwrap();
    }

    fn receive_new(&mut self, transaction: &TransactionView) {
        // RPC `get_transaction` to acquire the transaction status,
        // if it is none status, discard and produce a serie with "event" = "remove", "extra" = "unknown reason";
        // if it is pending status, insert into self.pending;
        // if it is proposed status, insert into self.proposed;
        // if it is committed status, discard;
    }

    fn travel(&mut self) {
        #[allow(clippy::mutable_key_type)]
        let txs_status: HashMap<_, _> = self
            .waiting
            .keys()
            .map(|txhash| (txhash.clone(), self.jsonrpc.get_transaction(txhash.clone())))
            .collect();
        for (txhash, status) in txs_status {
            match status {
                None => {
                    let instant = self.waiting.remove(&txhash).expect("must be some");
                    self.report_disappear(&txhash, instant);
                }
                Some(txstatus) => match txstatus.tx_status.status {
                    ckb_suite_rpc::ckb_jsonrpc_types::Status::Pending
                    | ckb_suite_rpc::ckb_jsonrpc_types::Status::Proposed => {
                        let instant = *self.waiting.get(&txhash).expect("must be some");
                        if Utc::now() - instant > chrono::Duration::seconds(5 * 60) {
                            self.waiting.remove(&txhash);
                            self.report_await(&txhash, instant);
                        }
                    }
                    ckb_suite_rpc::ckb_jsonrpc_types::Status::Committed => {
                        let block_hash = txstatus
                            .tx_status
                            .block_hash
                            .expect("committed transaction must be some");
                        if let Some(header) = self.jsonrpc.get_header(block_hash.pack()) {
                            let instant = self.waiting.remove(&txhash).expect("must be some");
                            self.report_commit(
                                &txhash,
                                instant.timestamp_millis() as u64,
                                header.inner.timestamp.value(),
                            );
                        }
                    }
                },
            }
        }
    }

    fn report_waiting_total(&self) {
        let query = measurement::PoolWaitingTransactionsTotal {
            time: Utc::now().into(),
            transactions_total: self.waiting.len() as u32,
        }
        .into_write_query();
        self.query_sender.send(query).unwrap();
    }

    fn report_commit(&self, _txhash: &Byte32, entering_timestamp: u64, committed_timestamp: u64) {
        let query = measurement::TransactionState {
            time: Timestamp::Milliseconds(committed_timestamp as u128),
            waiting_duration: committed_timestamp.saturating_sub(entering_timestamp),
            pool_event: "commit".to_string(),
            // transaction_hash: format!("{:#x}", txhash),
        }
        .into_write_query();
        self.query_sender.send(query).unwrap();
    }

    fn report_await(&self, _txhash: &Byte32, instant: DateTime<Utc>) {
        let query = measurement::TransactionState {
            time: instant.into(),
            waiting_duration: (Utc::now().timestamp_millis() - instant.timestamp_millis()) as u64,
            pool_event: "await".to_string(),
            // transaction_hash: format!("{:#x}", txhash),
        }
        .into_write_query();
        self.query_sender.send(query).unwrap();
    }

    fn report_disappear(&self, _txhash: &Byte32, instant: DateTime<Utc>) {
        let query = measurement::TransactionState {
            time: instant.into(),
            waiting_duration: (Utc::now().timestamp_millis() - instant.timestamp_millis()) as u64,
            pool_event: "disappear".to_string(),
            // transaction_hash: format!("{:#x}", txhash),
        }
        .into_write_query();
        self.query_sender.send(query).unwrap();
    }
}
