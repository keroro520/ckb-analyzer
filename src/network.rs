use crate::app_config::app_config;
use crate::get_version::get_version;
use crate::CKB_NETWORK_IDENTIFIER;
use chrono::Utc;
use ckb_network::{
    bytes::Bytes, CKBProtocol, CKBProtocolContext, CKBProtocolHandler, DefaultExitHandler,
    NetworkService, NetworkState, PeerIndex, SupportProtocols,
};
use ckb_types::packed::{
    Byte32, CompactBlock, RelayMessage, RelayMessageUnion, SendBlock, SyncMessage, SyncMessageUnion,
};
use ckb_types::prelude::*;
use crossbeam::channel::Sender;
use influxdb::{InfluxDbWriteable, Timestamp, WriteQuery};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::thread::spawn;
use std::time::Instant;

// TODO --sync-historical-uncles
// TODO don't send query unless connected peers are over 50
// TODO query consists of peers_total
// TODO this program should be deployed onto all machines, so that retrieve realtime info
// TODO network topological graph
// TODO query attaches node_id

#[derive(InfluxDbWriteable)]
pub struct PropagationSerie {
    time: Timestamp,

    time_interval: u64, // ms

    #[tag]
    percentile: u32,
    #[tag]
    message_type: String,
}

type PropagationHashes = Arc<Mutex<HashMap<Byte32, (Instant, HashSet<PeerIndex>)>>>;

#[derive(Clone)]
pub struct Handler {
    peers: Arc<Mutex<HashMap<PeerIndex, bool>>>,
    compact_blocks: PropagationHashes,
    transaction_hashes: PropagationHashes,
    query_sender: Sender<WriteQuery>,
}

impl Handler {
    pub fn new(query_sender: Sender<WriteQuery>) -> Self {
        Self {
            peers: Default::default(),
            compact_blocks: Default::default(),
            transaction_hashes: Default::default(),
            query_sender,
        }
    }

    #[allow(clippy::single_match)]
    fn received_relay(
        &mut self,
        nc: Arc<dyn CKBProtocolContext + Sync>,
        peer_index: PeerIndex,
        data: Bytes,
    ) {
        let relay_message = RelayMessage::from_slice(&data).unwrap().to_enum();
        match relay_message {
            RelayMessageUnion::CompactBlock(compact_block) => {
                self.received_compact_block(nc, peer_index, compact_block)
            }
            RelayMessageUnion::RelayTransactions(transactions) => {
                for transaction in transactions.transactions() {
                    let txhash = transaction.transaction().calc_tx_hash();
                    self.received_transaction_hash(nc.clone(), peer_index, txhash)
                }
            }
            RelayMessageUnion::RelayTransactionHashes(transaction_hashes) => {
                for txhash in transaction_hashes.tx_hashes() {
                    self.received_transaction_hash(nc.clone(), peer_index, txhash)
                }
            }
            _ => {}
        }
    }

    #[allow(clippy::single_match)]
    fn received_sync(
        &mut self,
        nc: Arc<dyn CKBProtocolContext + Sync>,
        peer_index: PeerIndex,
        data: Bytes,
    ) {
        let sync_message = SyncMessage::from_slice(&data).unwrap().to_enum();
        match sync_message {
            SyncMessageUnion::SendBlock(send_block) => {
                self.received_send_block(nc, peer_index, send_block)
            }
            _ => {}
        }
    }

    fn received_compact_block(
        &mut self,
        _nc: Arc<dyn CKBProtocolContext + Sync>,
        peer_index: PeerIndex,
        compact_block: CompactBlock,
    ) {
        let hash = compact_block.calc_header_hash();
        let (peers_received, first_received, newly_inserted) = {
            let mut compact_blocks = self.compact_blocks.lock().unwrap();
            let (first_received, peers) = compact_blocks
                .entry(hash)
                .or_insert_with(|| (Instant::now(), HashSet::default()));
            let newly_inserted = peers.insert(peer_index);
            (peers.len() as u32, *first_received, newly_inserted)
        };
        if newly_inserted {
            self.send_propagation_query("compact_block", peers_received, first_received)
        }
    }

    fn received_transaction_hash(
        &mut self,
        _nc: Arc<dyn CKBProtocolContext + Sync>,
        peer_index: PeerIndex,
        hash: Byte32,
    ) {
        let (peers_received, first_received, newly_inserted) = {
            let mut transaction_hashes = self.transaction_hashes.lock().unwrap();
            let (first_received, peers) = transaction_hashes
                .entry(hash)
                .or_insert_with(|| (Instant::now(), HashSet::default()));
            let newly_inserted = peers.insert(peer_index);
            (peers.len() as u32, *first_received, newly_inserted)
        };
        if newly_inserted {
            self.send_propagation_query("transaction_hash", peers_received, first_received)
        }
    }

    fn received_send_block(
        &mut self,
        _nc: Arc<dyn CKBProtocolContext + Sync>,
        _peer_index: PeerIndex,
        _send_block: SendBlock,
    ) {
    }

    fn send_propagation_query<M>(
        &self,
        message_type: M,
        peers_received: u32,
        first_received: Instant,
    ) where
        M: ToString,
    {
        static QUERY_NAME: &str = "propagation";

        let peers_total = self.peers.lock().unwrap().len();
        let last_percentile = (peers_received - 1) as f32 * 100.0 / peers_total as f32;
        let current_percentile = peers_received as f32 * 100.0 / peers_total as f32;
        let time_interval = first_received.elapsed().as_millis() as u64;
        let percentile = if last_percentile < 99.0 && current_percentile >= 99.0 {
            Some(99)
        } else if last_percentile < 95.0 && current_percentile >= 95.0 {
            Some(95)
        } else if last_percentile < 80.0 && current_percentile >= 80.0 {
            Some(80)
        } else {
            None
        };

        if let Some(percentile) = percentile {
            let query = PropagationSerie {
                time: Utc::now().into(),
                time_interval,
                percentile,
                message_type: message_type.to_string(),
            }
            .into_query(QUERY_NAME);
            self.query_sender.send(query).unwrap();
        }
    }
}

impl CKBProtocolHandler for Handler {
    fn init(&mut self, _nc: Arc<dyn CKBProtocolContext + Sync>) {}

    fn connected(
        &mut self,
        _nc: Arc<dyn CKBProtocolContext + Sync>,
        peer_index: PeerIndex,
        _version: &str,
    ) {
        let mut peers = self.peers.lock().unwrap();
        if *peers.entry(peer_index).or_insert(true) && crate::LOG_LEVEL.as_str() != "ERROR" {
            if let Some(peer) = _nc.get_peer(peer_index) {
                println!("connect with #{}({:?})", peer_index, peer.connected_addr);
            }
        }
    }

    fn disconnected(&mut self, _nc: Arc<dyn CKBProtocolContext + Sync>, peer_index: PeerIndex) {
        if self.peers.lock().unwrap().remove(&peer_index).is_some()
            && crate::LOG_LEVEL.as_str() != "ERROR"
        {
            if let Some(peer) = _nc.get_peer(peer_index) {
                println!("disconnect with #{}({:?})", peer_index, peer.connected_addr);
            }
        }
    }

    fn received(
        &mut self,
        nc: Arc<dyn CKBProtocolContext + Sync>,
        peer_index: PeerIndex,
        data: Bytes,
    ) {
        if nc.protocol_id() == SupportProtocols::Relay.protocol_id() {
            self.received_relay(nc, peer_index, data);
        } else if nc.protocol_id() == SupportProtocols::Sync.protocol_id() {
            self.received_sync(nc, peer_index, data);
        }
    }
}

pub(crate) fn spawn_analyze(query_sender: Sender<WriteQuery>) {
    let handler = Handler::new(query_sender);
    spawn(move || run_network_service(handler));
}

pub(crate) fn run_network_service(handler: Handler) {
    let config = app_config().network;
    let network_state = Arc::new(NetworkState::from_config(config).unwrap());
    let exit_handler = DefaultExitHandler::default();
    let version = get_version();
    let protocols = vec![SupportProtocols::Sync, SupportProtocols::Relay];
    let required_protocol_ids = protocols
        .iter()
        .map(|protocol| protocol.protocol_id())
        .collect();
    let ckb_protocols = protocols
        .into_iter()
        .map(|protocol| {
            CKBProtocol::new_with_support_protocol(
                protocol,
                Box::new(handler.clone()),
                Arc::clone(&network_state),
            )
        })
        .collect();
    let _network_controller = NetworkService::new(
        network_state,
        ckb_protocols,
        required_protocol_ids,
        CKB_NETWORK_IDENTIFIER.clone(),
        version.to_string(),
        exit_handler.clone(),
    )
    .start(Some("ckb-analyzer::network"))
    .unwrap();

    exit_handler.wait_for_exit();
}
