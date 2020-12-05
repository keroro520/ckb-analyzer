use crate::analyzer::{Analyzer, MainChainConfig, NetworkTopologyConfig, Fork, ForkConfig};
use crate::subscribe::Topic;
pub use config::{init_config, Config};
use crossbeam::channel::bounded;
use influxdb::Client;
use lazy_static::lazy_static;
use std::env::var;

mod analyzer;
mod app_config;
mod config;
mod get_version;
mod serie;
mod subscribe;

lazy_static! {
    static ref LOG_LEVEL: String = var("LOG_LEVEL").unwrap_or_else(|_| "ERROR".to_string());
    static ref CONFIG: Config = {
        let config_path = var("CKB_ANALYZER_CONFIG").unwrap_or_else(|_| {
            panic!("please specify config path via environment variable CKB_ANALYZER_CONFIG")
        });
        init_config(config_path)
    };
    static ref HOSTNAME: String = var("HOSTNAME")
        .unwrap_or_else(|_| gethostname::gethostname().to_string_lossy().to_string());
    static ref INFLUXDB_USERNAME: String =
        var("INFLUXDB_USERNAME").unwrap_or_else(|_| "".to_string());
    static ref INFLUXDB_PASSWORD: String =
        var("INFLUXDB_PASSWORD").unwrap_or_else(|_| "".to_string());
}

#[tokio::main]
async fn main() {
    let influx = if INFLUXDB_USERNAME.is_empty() {
        Client::new(
            CONFIG.influxdb.url.as_str(),
            CONFIG.influxdb.database.as_str(),
        )
    } else {
        Client::new(
            CONFIG.influxdb.url.as_str(),
            CONFIG.influxdb.database.as_str(),
        )
        .with_auth(INFLUXDB_USERNAME.as_str(), INFLUXDB_PASSWORD.as_str())
    };
    let (query_sender, query_receiver) = bounded(5000);

    if CONFIG.fork.enabled {
        let config = ForkConfig {
            ckb_subscribe_url: CONFIG.fork.ckb_subscription_url.clone(),
        };
        let (mut fork, subscription) = Fork::init(config);
        tokio::spawn(fork.run());
        tokio::spawn(subscription.run());
    }
    if CONFIG.chain.enabled {
        let config = MainChainConfig {
            ckb_rpc_url: CONFIG.chain.ckb_url.clone(),
        };
        let analyzer = Analyzer::MainChain(config);
        tokio::spawn( analyzer.run(influx.clone(), query_sender.clone()) );
    }
    if CONFIG.network.enabled {
        let future = Analyzer::NetworkProbe.run(influx.clone(), query_sender.clone());
        tokio::spawn(future);
    }
    if CONFIG.topology.enabled {
        let config = NetworkTopologyConfig {
            ckb_rpc_urls: CONFIG.topology.ckb_urls.clone(),
        };
        let future = Analyzer::NetworkTopology(config).run(influx.clone(), query_sender);
        tokio::spawn(future);
    }

    for mut query in query_receiver {
        // Attach built-in tags
        query = query
            .add_tag("network", CONFIG.network.ckb_network_name.clone())
            .add_tag("hostname", HOSTNAME.clone());

        // Writes asynchronously
        let influx_ = influx.clone();
        tokio::spawn(async move { influx_.query(&query).await });

        // Writes synchronously
        // let write_result = influx.query(&query).await;
        // if let Err(err) = write_result {
        //     eprintln!("influxdb.query, error: {}", err);
        // }
    }
}
