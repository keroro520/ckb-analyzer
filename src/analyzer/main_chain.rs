use crate::serie::{Block, Epoch, IntoWriteQuery, Transaction, Uncle};
use crate::CONFIG;
use crate::LOG_LEVEL;
use ckb_suite_rpc::Jsonrpc;
use ckb_types::core::{BlockNumber, HeaderView};
use ckb_types::core::{BlockView, EpochNumber};
use ckb_types::packed::{CellbaseWitness, ProposalShortId, Script};
use ckb_types::prelude::*;
use crossbeam::channel::Sender;
use influxdb::{Client as Influx, ReadQuery, Timestamp, WriteQuery};
use serde::{Deserialize, Serialize};
use std::cmp::max;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

pub const PROPOSAL_WINDOW: (u64, u64) = (2, 10);

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct MainChainConfig {
    pub ckb_rpc_url: String,
}

pub struct MainChain {
    // config: MainChainConfig,
    query_sender: Sender<WriteQuery>,
    last_number: BlockNumber,
    rpc: Jsonrpc,
    proposals_zones: HashMap<BlockNumber, HashSet<ProposalShortId>>,
}

impl MainChain {
    pub fn new(
        config: MainChainConfig,
        query_sender: Sender<WriteQuery>,
        last_number: BlockNumber,
    ) -> Self {
        let rpc = Jsonrpc::connect(config.ckb_rpc_url.as_str());
        Self {
            // config,
            query_sender,
            last_number,
            rpc,
            proposals_zones: Default::default(),
        }
    }

    pub async fn run(&mut self) {
        println!("run {}", ::std::any::type_name::<Self>());
        self.analyze_blocks().await
    }

    async fn analyze_blocks(&mut self) {
        let mut number = max(1, self.last_number + 1);
        let mut tip = self.rpc.get_tip_block_number();
        let mut parent: BlockView = self
            .rpc
            .get_block_by_number(number.saturating_sub(1))
            .unwrap()
            .into();
        let (mut current_epoch_number, mut current_epoch_uncles_total) =
            (parent.epoch().number(), 0);
        loop {
            // Assume 10 is block confirmation
            if number >= tip - 10 {
                tokio::time::delay_for(Duration::from_secs(1)).await;
                tip = self.rpc.get_tip_block_number();
                continue;
            }

            if let Some(json_block) = self.rpc.get_block_by_number(number) {
                let block: BlockView = json_block.into();
                self.analyze_block(&block, &parent).await;
                self.analyze_block_uncles(&block).await;
                self.analyze_block_transactions(&block).await;
                self.analyze_epoch(
                    &mut current_epoch_number,
                    &mut current_epoch_uncles_total,
                    &block,
                )
                .await;

                number += 1;
                parent = block;
            }
        }
    }

    async fn analyze_block(&self, block: &BlockView, parent: &BlockView) {
        let time = Timestamp::Milliseconds(block.timestamp() as u128);
        let number = block.number();
        let time_interval = block.timestamp().saturating_sub(parent.timestamp()); // ms
        let transactions_count = block.transactions().len() as u32;
        let uncles_count = block.uncles().hashes().len() as u32;
        let proposals_count = block.union_proposal_ids().len() as u32;
        let version = block.version();
        let miner_lock_args = extract_miner_lock(&block).args().to_string();
        let query = Block {
            time,
            number,
            time_interval,
            transactions_count,
            uncles_count,
            proposals_count,
            version,
            miner_lock_args,
        }
        .into_write_query();
        if LOG_LEVEL.as_str() != "ERROR" {
            println!(
                "[DEBUG] block #{}, timestamp: {}",
                number,
                block.timestamp(),
            );
        }
        self.query_sender.send(query).unwrap();
    }

    async fn analyze_block_uncles(&self, block: &BlockView) {
        for uncle_hash in block.uncle_hashes() {
            if let Some(json_uncle) = self.rpc.get_fork_block(uncle_hash) {
                self.analyze_block_uncle(&json_uncle.into()).await
            }
        }
    }

    async fn analyze_block_uncle(&self, uncle: &BlockView) {
        let uncle_number = uncle.number();
        let time = Timestamp::Milliseconds(uncle.timestamp() as u128);
        let proposals_count = uncle.union_proposal_ids().len() as u32;
        let transactions_count = uncle.transactions().len() as u32;
        let version = uncle.version();
        let miner_lock_args = format!("{:#x}", extract_miner_lock(&uncle).args());
        let slower_than_cousin = {
            let cousin = self.rpc.get_header_by_number(uncle_number).unwrap().inner;
            cousin.timestamp.value() as i64 - uncle.timestamp() as i64
        };
        let query = Uncle {
            time,
            number: uncle_number,
            proposals_count,
            transactions_count,
            version,
            miner_lock_args,
            slower_than_cousin,
        }
        .into_write_query();
        if LOG_LEVEL.as_str() != "ERROR" {
            println!(
                "[DEBUG] uncle #{}({:#x}), timestamp: {}, slower_than_cousin: {}",
                uncle_number,
                uncle.hash(),
                uncle.timestamp(),
                slower_than_cousin,
            );
        }
        self.query_sender.send(query).unwrap();
    }

    async fn analyze_block_transactions(&mut self, block: &BlockView) {
        let number = block.number();
        for transaction in block.transactions() {
            let proposal_id = transaction.proposal_short_id();
            for proposed in number - PROPOSAL_WINDOW.1..=number - PROPOSAL_WINDOW.0 {
                let removed = self
                    .proposals_zones
                    .get_mut(&proposed)
                    .map(|proposals_zone| proposals_zone.remove(&proposal_id))
                    .unwrap_or(false);
                if removed {
                    let time = Timestamp::Milliseconds(block.timestamp() as u128);
                    let pc_delay = (number - proposed) as u32;
                    let query = Transaction {
                        time,
                        number,
                        pc_delay,
                    }
                    .into_write_query();
                    self.query_sender.send(query).unwrap();
                    break;
                }
            }
        }

        // Prune outdated proposals zone
        self.proposals_zones.remove(&(number - PROPOSAL_WINDOW.1));
        self.proposals_zones
            .insert(number, block.union_proposal_ids());
    }

    async fn analyze_epoch(
        &self,
        current_epoch_number: &mut EpochNumber,
        current_epoch_uncles_total: &mut u32,
        block: &BlockView,
    ) {
        if *current_epoch_number != block.epoch().number() {
            let epoch = self.rpc.get_epoch_by_number(*current_epoch_number).unwrap();
            let start_number = epoch.start_number.value();
            let length = epoch.length.value();
            let end_number = start_number + length - 1;
            let start_header: HeaderView =
                self.rpc.get_header_by_number(start_number).unwrap().into();
            let end_header: HeaderView = self.rpc.get_header_by_number(end_number).unwrap().into();
            let duration = end_header
                .timestamp()
                .saturating_sub(start_header.timestamp());
            let query = Epoch {
                time: Timestamp::Milliseconds(end_header.timestamp() as u128),
                number: *current_epoch_number,
                length,
                duration,
                uncles_count: *current_epoch_uncles_total,
            }
            .into_write_query();
            self.query_sender.send(query).unwrap();

            *current_epoch_number = block.epoch().number();
            *current_epoch_uncles_total = 0;
        }

        // Sum epoch uncles total
        *current_epoch_uncles_total += block.uncle_hashes().len() as u32;
    }
}

#[allow(unused)]
fn prompt_progress(total: u64, processed: u64, start: Instant) {
    const PROMPT_STEP: u64 = 10000;

    if processed % PROMPT_STEP == 0 && processed > 0 {
        let left = total - processed;
        let processed_duration = start.elapsed();
        let left_duration = processed_duration
            .mul_f64(left as f64)
            .div_f64(processed as f64);
        let processed_percent = processed as f64 / total as f64;

        println!(
            "Progress {:.2}, left {}s ...",
            processed_percent,
            left_duration.as_secs()
        );
    }
}

fn extract_miner_lock(block: &BlockView) -> Script {
    let cellbase = block.transaction(0).unwrap();
    let witness = cellbase.witnesses().get(0).unwrap().raw_data();
    let cellbase_witness = CellbaseWitness::from_slice(witness.as_ref()).unwrap();
    cellbase_witness.lock()
}

pub async fn select_last_block_number_in_influxdb(influx: &Influx) -> BlockNumber {
    let sql = format!(
        "SELECT last(number) FROM {query_name} WHERE network = '{network}'",
        query_name = "block",
        network = CONFIG.network.ckb_network_name
    );
    let query_last_number = ReadQuery::new(&sql);
    match influx.query(&query_last_number).await {
        Err(err) => {
            eprintln!("influxdb.query(\"{}\"), error: {}", sql, err);
            ::std::process::exit(1);
        }
        Ok(results) => {
            let json: HashMap<String, serde_json::Value> = serde_json::from_str(&results).unwrap();
            let results = json.get("results").unwrap().as_array().unwrap();
            let result = results.get(0).unwrap().as_object().unwrap();
            if let Some(series) = result.get("series") {
                let series = series.as_array().unwrap();
                let serie = series.get(0).unwrap().as_object().unwrap();
                let values = serie.get("values").unwrap().as_array().unwrap();
                let value = values.get(0).unwrap().as_array().unwrap();
                value.get(1).unwrap().as_u64().unwrap()
            } else {
                1
            }
        }
    }
}
