use ckb_app_config::NetworkConfig;
use crossbeam::channel::Sender;
use influxdb::{Client as Influx, WriteQuery};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

mod canonical_chain;
mod logs;
mod network_propagation;
mod network_topology;
mod reorganization;
mod tx_transition;

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "topic", content = "args")]
pub enum Topic {
    CanonicalChain {
        ckb_rpc_url: String,
    },
    Reorganization {
        ckb_rpc_url: String,
        ckb_subscribe_url: String,
    },
    TxTransition {
        ckb_rpc_url: String,
        ckb_subscribe_url: String,
    },
    Logs {
        filepath: String,
        patterns: HashMap<String, logs::Regex>,
    },
    NetworkPropagation {
        ckb_network_identifier: String,
        ckb_network_config: NetworkConfig,
    },
    NetworkTopology {
        ckb_rpc_urls: Vec<String>,
    },
}

impl Topic {
    pub async fn run(
        self,
        topic_name: String,
        ckb_network_name: String,
        influx: Influx,
        query_sender: Sender<WriteQuery>,
    ) {
        log::info!("{} starting ...", topic_name);
        match self {
            Self::CanonicalChain { ckb_rpc_url } => {
                let last_number = canonical_chain::select_last_block_number_in_influxdb(
                    &influx,
                    &ckb_network_name,
                )
                .await;
                canonical_chain::Handler::new(&ckb_rpc_url, query_sender, last_number)
                    .run()
                    .await
            }

            Self::NetworkPropagation {
                ckb_network_config,
                ckb_network_identifier,
            } => {
                network_propagation::Handler::new(
                    ckb_network_config,
                    ckb_network_identifier,
                    query_sender,
                )
                .run()
                .await
            }

            Self::NetworkTopology { ckb_rpc_urls } => {
                network_topology::Handler::new(ckb_rpc_urls).run().await
            }

            Self::Reorganization {
                ckb_rpc_url,
                ckb_subscribe_url,
            } => {
                let (handler, subscription) =
                    reorganization::Handler::new(ckb_rpc_url, ckb_subscribe_url, query_sender);

                // IMPORTANT: Use tokio 1.0 to run subscription. Since jsonrpc has not support 2.0 yet
                ::std::thread::spawn(move || {
                    jsonrpc_server_utils::tokio::run(subscription.run());
                });

                // // PROBLEM: With delaying a while, both tasks subscription and reorganization will run;
                // // But without delaying, only the task reorganization will run.
                // tokio::spawn(async { subscription.run().await });
                // tokio::time::delay_for(::std::time::Duration::from_secs(3)).await;

                handler.run().await;
            }

            Self::TxTransition {
                ckb_rpc_url,
                ckb_subscribe_url,
            } => {
                let (handler, subscription) = tx_transition::Handler::new(
                    ckb_rpc_url,
                    ckb_subscribe_url,
                    query_sender.clone(),
                );

                // IMPORTANT: Use tokio 1.0 to run subscription. Since jsonrpc has not support 2.0 yet
                ::std::thread::spawn(move || {
                    jsonrpc_server_utils::tokio::run(subscription.run());
                });
                handler.run().await;
            }

            Self::Logs { filepath, patterns } => {
                let mut handler = logs::Handler::new(filepath, patterns, query_sender);
                ::std::thread::spawn(move || handler.run());
            }
        }
    }
}
