use crate::measurement::{self, IntoWriteQuery};
use crate::subscribe::{Subscription, Topic};
use chrono::{DateTime, Utc};
use ckb_suite_rpc::{
    ckb_jsonrpc_types::{PoolTransactionEntry, Status},
    Jsonrpc,
};
use ckb_types::{packed::Byte32, prelude::*};
use crossbeam::channel::{bounded, Sender};
use influxdb::{Timestamp, WriteQuery};
use jsonrpc_core::{futures::Stream, serde_from_str};
use jsonrpc_server_utils::tokio::prelude::*;
use std::collections::HashMap;
use std::time::Instant;

/// This module aims to the pool transactions
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
///   - "remove", remove with unknown reason;
///
/// The serie may be like below. Some of events are used to visualize in heatmap, some in table:
/// ```json
/// {
///   "query_name": "tx_transition",
///
///   "txhash": h256,
///
///   "event": "pending"/"pending_long"/"propose"/"propose_long"/"commit"/"remove",
///
///   "time": time instant;
///         if event is "pending", assign it `now`;
///         if event is "pending_long", keeps this field the same as "pending";
///         if event is "propose", assign it proposed block timestamp;
///         if event is "propose_long", keeps this field the same as "propose";
///         if event is "commit", assign it committed block timestamp;
///         if event is "remove", assign it `now`;
///
///   "elapsed": duration;
///         if event is "pending", assign it 0;
///         if event is "pending_long", assign `now - pending.time`;
///         if event is "propose", assign `proposed.time - pending.time`;
///         if event is "propose_long", assign `now - proposed.time`;
///         if event is "commit", assign `committed.time - proposed.time";
///         if event is "remove", assign it `now - pending.time`;
/// }
/// ```
///
/// * maintaining fields:
///   - pending
///   - proposed
///
/// * Information source:
///   - Subscription of NewTransaction
///     Retrieve transaction status via `get_transaction`, if status is pending, then report a
///     "pending" event, otherwise do nothing.
///
/// * Periodically checking
///   Travels transactions in self.pending and self.proposed, check the transaction status
///   transition.
///   As for transaction in self.pending,
///     - if its current status is unknown, remove out and report "remove" event;
///     - if its current status is pending and elapses within threshold, discard;
///     - if its current status is pending and elapses beyond threshold, remove out and report
///       "pending_long" event;
///     - if its current status is proposed, move into self.proposed and report "propose" event;
///     - if its current status is committed, remove out;
///   As for transaction in self.proposed,
///     - if its current status is unknown, remove out and report "remove" event;
///     - if its current status is pending, move into self.pending and report
///       "propose_back_pending" event;
///     - if its current status is proposed and elapses within threshold, discard
///     - if its current status is proposed and elapses beyond threshold, remove out and report
///       "propose_long" event;
///     - if its current status is committed, remove out and report "commit" event;
///
/// * TODO
///   - Retrieve all pool transactions via RPC `get_raw_tx_pool` at the start.

const WAITING_THRESHOLD_MILLIS: u64 = ::std::time::Duration::from_secs(5 * 60).as_millis() as u64;

pub(crate) struct Handler {
    // waiting: HashMap<Byte32, DateTime<Utc>>,
    pending: HashMap<Byte32, Timestamp>,
    proposed: HashMap<Byte32, Timestamp>,

    new_tx_subscriber: jsonrpc_server_utils::tokio::sync::mpsc::Receiver<String>,

    jsonrpc: Jsonrpc,
    query_sender: Sender<WriteQuery>,
    last_checking_at: Instant,
}

impl Handler {
    pub(crate) fn new(
        ckb_rpc_url: String,
        ckb_subscribe_url: String,
        query_sender: Sender<WriteQuery>,
    ) -> (Self, Subscription) {
        let subscribe = |topic| {
            let (notifier, subscriber) = jsonrpc_server_utils::tokio::sync::mpsc::channel(100);
            let subscription = Subscription::new(ckb_subscribe_url.clone(), topic, notifier);
            (subscriber, subscription)
        };
        let (new_tx_subscriber, new_tx_subscription) = subscribe(Topic::NewTransaction);
        let jsonrpc = Jsonrpc::connect(&ckb_rpc_url);
        (
            Self {
                jsonrpc,
                query_sender,
                new_tx_subscriber,
                pending: Default::default(),
                proposed: Default::default(),
                last_checking_at: Instant::now(),
            },
            new_tx_subscription,
        )
    }

    pub(crate) async fn run(mut self) {
        // Take out the subscriber to pass Rust borrow rule
        let new_tx_subscriber = {
            let (_, mut dummy) = jsonrpc_server_utils::tokio::sync::mpsc::channel(100);
            ::std::mem::swap(&mut self.new_tx_subscriber, &mut dummy);
            dummy
        };
        let (forward_sender, forward_receiver) = bounded(1000);
        forward(new_tx_subscriber, forward_sender);
        loop {
            match forward_receiver.recv_timeout(::std::time::Duration::from_secs(60)) {
                Ok((Topic::NewTransaction, message)) => {
                    let tx = serde_from_str::<PoolTransactionEntry>(&message)
                        .map(|pt| {
                            Into::<ckb_types::packed::Transaction>::into(pt.transaction.inner)
                        })
                        .map(|tx| tx.into_view())
                        .unwrap();
                    if self.get_tx_status(&tx.hash()).is_pending() {
                        #[allow(clippy::map_entry)]
                        if !self.pending.contains_key(&tx.hash()) {
                            let now = Utc::now().into();
                            self.pending.insert(tx.hash(), now);
                            self.report_pending(&tx.hash(), now);
                        }
                    } else {
                        // Discard the newly subscribed transaction for unknown, proposed or committed status
                    }
                }
                Ok((_topic, _message)) => unimplemented!(),
                Err(crossbeam::channel::RecvTimeoutError::Timeout) => continue,
                Err(crossbeam::channel::RecvTimeoutError::Disconnected) => break,
            }

            if self.last_checking_at.elapsed() >= ::std::time::Duration::from_secs(60) {
                self.check_pending();
                self.check_proposed();
                self.last_checking_at = Instant::now();
            }
        }
    }

    //   As for transaction in self.pending,
    //     - if its current status is unknown, remove out and report "remove" event;
    //     - if its current status is pending and elapses within threshold, discard;
    //     - if its current status is pending and elapses beyond threshold, remove out and report
    //       "pending_long" event;
    //     - if its current status is proposed, move into self.proposed and report "propose" event;
    //     - if its current status is committed, remove out;
    fn check_pending(&mut self) {
        let mut to_remove = Vec::new();
        for (txhash, pending_timestamp) in self.pending.iter() {
            match self.get_tx_status(txhash) {
                TxStatus::Unknown => {
                    to_remove.push(txhash.clone());
                    self.report_remove(txhash, *pending_timestamp);
                }
                TxStatus::Pending => {
                    if elapsed_millis(*pending_timestamp) < WAITING_THRESHOLD_MILLIS {
                        // discard
                    } else {
                        to_remove.push(txhash.clone());
                        self.report_pending_long(txhash, *pending_timestamp);
                    }
                }
                TxStatus::Proposed => {
                    to_remove.push(txhash.clone());

                    let now = Utc::now().into();
                    if !self.proposed.contains_key(&txhash) {
                        self.proposed.insert(txhash.clone(), now);
                        self.report_propose(txhash, *pending_timestamp, now);
                    }
                }
                TxStatus::Committed(_block_hash) => {
                    to_remove.push(txhash.clone());
                }
            }
        }

        for txhash in to_remove.iter() {
            self.pending.remove(txhash);
        }
    }

    //   As for transaction in self.proposed,
    //     - if its current status is unknown, remove out and report "remove" event;
    //     - if its current status is pending, move into self.pending;
    //     - if its current status is proposed and elapses within threshold, discard
    //     - if its current status is proposed and elapses beyond threshold, remove out and report
    //       "propose_long" event;
    //     - if its current status is committed, remove out and report "commit" event;
    fn check_proposed(&mut self) {
        let mut to_remove = Vec::new();
        for (txhash, proposed_timestamp) in self.proposed.iter() {
            match self.get_tx_status(txhash) {
                TxStatus::Unknown => {
                    to_remove.push(txhash.clone());
                    self.report_remove(txhash, *proposed_timestamp);
                }
                TxStatus::Pending => {
                    to_remove.push(txhash.clone());
                    // Move back to self.pending.
                    // Although reusing proposed timestamp is not good, it is not bad as well
                    if !self.pending.contains_key(txhash) {
                        self.pending.insert(txhash.clone(), *proposed_timestamp);
                    }
                }
                TxStatus::Proposed => {
                    if elapsed_millis(*proposed_timestamp) < WAITING_THRESHOLD_MILLIS {
                        // discard
                    } else {
                        to_remove.push(txhash.clone());
                        self.report_propose_long(txhash, *proposed_timestamp);
                    }
                }
                TxStatus::Committed(block_hash) => {
                    if let Some(committed_timestamp) = self.get_block_timestamp(block_hash) {
                        to_remove.push(txhash.clone());
                        self.report_commit(txhash, *proposed_timestamp, committed_timestamp);
                    } else {
                        log::error!(
                            "get_block_timestamp with committed tx returns unknown; discard"
                        );
                        // discard
                    }
                }
            }
        }

        for txhash in to_remove.iter() {
            self.proposed.remove(txhash);
        }
    }

    fn report_pending(&self, txhash: &Byte32, timestamp: Timestamp) {
        let query = measurement::TxTransition {
            time: timestamp,
            elapsed: 0,
            event: TxEvent::Pending.to_string(),
            txhash: format!("{:?}", txhash),
        }
        .into_write_query();
        self.query_sender.send(query).unwrap();
    }

    fn report_propose(
        &self,
        txhash: &Byte32,
        pending_timestamp: Timestamp,
        proposed_timestamp: Timestamp,
    ) {
        let query = measurement::TxTransition {
            time: proposed_timestamp,
            elapsed: diff_millis(proposed_timestamp, pending_timestamp),
            event: TxEvent::Propose.to_string(),
            txhash: format!("{:?}", txhash),
        }
        .into_write_query();
        self.query_sender.send(query).unwrap();
    }

    fn report_commit(
        &self,
        txhash: &Byte32,
        proposed_timestamp: Timestamp,
        committed_timestamp: Timestamp,
    ) {
        let query = measurement::TxTransition {
            time: committed_timestamp,
            elapsed: diff_millis(committed_timestamp, proposed_timestamp),
            event: TxEvent::Commit.to_string(),
            txhash: format!("{:?}", txhash),
        }
        .into_write_query();
        self.query_sender.send(query).unwrap();
    }

    fn report_pending_long(&self, txhash: &Byte32, timestamp: Timestamp) {
        let query = measurement::TxTransition {
            time: timestamp,
            elapsed: elapsed_millis(timestamp),
            event: TxEvent::PendingLong.to_string(),
            txhash: format!("{:?}", txhash),
        }
        .into_write_query();
        self.query_sender.send(query).unwrap();
    }

    fn report_propose_long(&self, txhash: &Byte32, timestamp: Timestamp) {
        let query = measurement::TxTransition {
            time: timestamp,
            elapsed: elapsed_millis(timestamp),
            event: TxEvent::ProposeLong.to_string(),
            txhash: format!("{:?}", txhash),
        }
        .into_write_query();
        self.query_sender.send(query).unwrap();
    }

    fn report_remove(&self, txhash: &Byte32, timestamp: Timestamp) {
        let query = measurement::TxTransition {
            time: timestamp,
            elapsed: elapsed_millis(timestamp),
            event: TxEvent::Remove.to_string(),
            txhash: format!("{:?}", txhash),
        }
        .into_write_query();
        self.query_sender.send(query).unwrap();
    }

    fn get_tx_status(&self, txhash: &Byte32) -> TxStatus {
        if let Some(txstatus) = self.jsonrpc.get_transaction(txhash.clone()) {
            match txstatus.tx_status.status {
                Status::Pending => TxStatus::Pending,
                Status::Proposed => TxStatus::Proposed,
                Status::Committed => {
                    TxStatus::Committed(txstatus.tx_status.block_hash.unwrap().pack())
                }
            }
        } else {
            TxStatus::Unknown
        }
    }

    fn get_block_timestamp(&self, block_hash: Byte32) -> Option<Timestamp> {
        if let Some(header) = self.jsonrpc.get_header(block_hash) {
            return Some(Timestamp::Milliseconds(
                header.inner.timestamp.value() as u128
            ));
        }
        None
    }
}

fn forward(
    new_tx_subscriber: jsonrpc_server_utils::tokio::sync::mpsc::Receiver<String>,
    forward_sender: Sender<(Topic, String)>,
) {
    ::std::thread::spawn(move || {
        new_tx_subscriber
            .for_each(|message| {
                forward_sender
                    .send((Topic::NewTransaction, message))
                    .unwrap();
                Ok(())
            })
            .wait()
            .unwrap();
    });
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
enum TxStatus {
    Unknown,
    Pending,
    Proposed,
    Committed(Byte32),
}

#[allow(dead_code)]
impl TxStatus {
    fn is_unknown(&self) -> bool {
        self == &TxStatus::Unknown
    }
    fn is_pending(&self) -> bool {
        self == &TxStatus::Pending
    }
    fn is_proposed(&self) -> bool {
        self == &TxStatus::Proposed
    }
    fn is_committed(&self) -> bool {
        if let Self::Committed(_) = self {
            true
        } else {
            false
        }
    }
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Copy)]
enum TxEvent {
    Pending,
    PendingLong,
    Propose,
    ProposeLong,
    Commit,
    Remove,
}

impl ::std::fmt::Display for TxEvent {
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        match self {
            Self::Pending => write!(f, "pending"),
            Self::PendingLong => write!(f, "pending_long"),
            Self::Propose => write!(f, "propose"),
            Self::ProposeLong => write!(f, "propose_long"),
            Self::Commit => write!(f, "commit"),
            Self::Remove => write!(f, "remove"),
        }
    }
}

fn elapsed_millis(instant: Timestamp) -> u64 {
    let instant: DateTime<Utc> = instant.into();
    let now = Utc::now();
    now.timestamp_millis()
        .saturating_sub(instant.timestamp_millis()) as u64
}

fn diff_millis(minuend: Timestamp, minus: Timestamp) -> u64 {
    let minuend: DateTime<Utc> = minuend.into();
    let minus: DateTime<Utc> = minus.into();
    minuend
        .timestamp_millis()
        .saturating_sub(minus.timestamp_millis()) as u64
}
