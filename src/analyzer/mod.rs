use crossbeam::channel::Sender;
use influxdb::{Client as Influx, WriteQuery};
use serde::{Deserialize, Serialize};

mod fork;
mod main_chain;
mod network_probe;
mod network_topology;

pub use fork::{Fork, ForkConfig};
pub use main_chain::{select_last_block_number_in_influxdb, MainChain, MainChainConfig};
pub use network_probe::NetworkProbe;
pub use network_topology::{NetworkTopology, NetworkTopologyConfig};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum Analyzer {
    MainChain(MainChainConfig),
    NetworkProbe,
    NetworkTopology(NetworkTopologyConfig),
    Fork(ForkConfig),
}

impl Analyzer {
    pub async fn run(self, influx: Influx, query_sender: Sender<WriteQuery>) {
        match self {
            Self::MainChain(config) => {
                let last_number = select_last_block_number_in_influxdb(&influx).await;
                MainChain::new(config, query_sender, last_number)
                    .run()
                    .await
            }
            Self::NetworkProbe => NetworkProbe::new(query_sender).run().await,
            Self::NetworkTopology(config) => NetworkTopology::new(config).run().await,
            Self::Fork(config) => {
                let (fork, subscription) = Fork::init(config);
                tokio::spawn(subscription.run());
                fork.run().await
            }
        }
    }
}
