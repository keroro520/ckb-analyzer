//! ckb-analyzer is an agent for collecting metrics from ckb, then writing the processed metrics
//! info InfluxDB. We can visualize these metrics on Grafana or other visualization tools.
//!
//! ckb-analyzer is still working in progress rapidly.
//!
//! # Install
//!
//! Download from [releases](https://github.com/keroro520/ckb-analyzer/releases) or
//!
//! ```shell
//! cargo install ckb-analyzer
//! ```
//!
//! # Usage
//!
//! ckb-analyzer reads several environment variables:
//!
//! * `CKB_ANALYZER_CONFIG` specify the configuration file path
//! * `CKB_RPC_USERNAME` specify the authorization username to ckb rpc service, default is `""`
//! * `CKB_RPC_PASSWORD` specify the authorization password to ckb rpc service, default is `""`
//! * `INFLUXDB_USERNAME` specify the influxdb username, default is `""`
//! * `INFLUXDB_PASSWORD` specify the influxdb password, default is `""`
//!
//! Command example:
//!
//! ```shell
//! CKB_ANALYZER_CONFIG=config/test.toml ckb-analyzer
//! ```
//!
//! # Topics and measurements
//!
//! * [ ] canonical chain
//!   - [ ] chain growth
//!   - [ ] block committed transactions and proposed transactions
//!   - [ ] transactions per seconds
//!   - [ ] block time interval
//!   - [ ] epoch uncles count and uncles rate
//!   - [ ] epoch duration and length
//!   - [ ] epoch adjustment
//!
//! * [ ] canonical chain reorganization
//!   * [ ] traffic
//!   * [ ] scale
//!
//! * [ ] node's uncle blocks (some may not be included in canonical chain uncles)
//!   - [ ] traffic
//!
//! * [ ] miner
//!   - [ ] the miner node of a specified block (ip or lock args)
//!
//! * [ ] transaction transition (mainly focus the transaction traffic and state transition latency)
//!   - [ ] pending
//!   - [ ] pending too long
//!   - [ ] propose
//!   - [ ] propose too long
//!   - [ ] commit
//!   - [ ] remove (with reason, reject, conflict, and so forth)
//!   - [ ] reorganize
//!
//! * [ ] processed cost
//!   - [ ] verify block
//!   - [ ] verify transaction
//!
//! * [ ] transaction and block propagation across the network
//!   - [ ] the number of connected peers
//!   - [ ] propagation elapsed
//!   - [ ] high latency propagation
//!
//! * [ ] logged events
//!   - [ ] error and warning events
//!   - [ ] sufficient events, recognize via regex patterning; better structure these logs
//!
//! # Debug suites
//!
//! * [ ] persist recent transactions (debug suite)
//!
//! * [ ] reproduce context
//!
//! # Monitoring alerts
//!
//! * [ ] datasource issues
//!   - [ ] no update for a long time
//!
//! * [ ] miner issues
//!   - [ ] chain does not grow up for too long
//!   - [ ] node receives too many uncle blocks
//!
//! * [ ] chain growth issues
//!   - [ ] block time interval is shorter/longer then threshold
//!   - [ ] a big epoch adjustment
//!
//! * [ ] transaction transition issues
//!   - [ ] too many transactions at a certain state
//!
//! * [ ] logged issues
//!
//! # Dashboards
//!
//! Please reference our Grafana dashboard files at [`dashboards`](https://github.com/keroro520/ckb-analyzer/tree/main/dashboards)
//!
//! # FAQ
//!
//! * ckb itself exposes metrics. Then why create ckb-analyzer?
//!
//!   Some metrics are not convenient to expose from ckb, like historical chain metrics and complex
//!   analyzing tasks. With ckb-analyzer, we can display historical chain information by extracting
//!   the historical blocks and do some complexity tasks outside ckb, which prevent adding too much
//!   complexity into ckb.
//!
//! * Why use InfluxDB?
//!
//!   Pushing metrics actively via HTTP to InfluxDB is much useful!

use crossbeam::channel::bounded;
use influxdb::Client;
use std::env::var;

mod config;
mod dashboard;
mod get_version;
mod measurement;
mod subscribe;
mod topic;

#[tokio::main]
async fn main() {
    let config = {
        let config_path = var("CKB_ANALYZER_CONFIG").unwrap_or_else(|_| {
            panic!("please specify config path via environment variable CKB_ANALYZER_CONFIG")
        });
        config::init_config(config_path)
    };
    let influx = {
        let username = var("INFLUXDB_USERNAME").unwrap_or_else(|_| "".to_string());
        let password = var("INFLUXDB_PASSWORD").unwrap_or_else(|_| "".to_string());
        if username.is_empty() {
            Client::new(
                config.influxdb.url.as_str(),
                config.influxdb.database.as_str(),
            )
        } else {
            Client::new(
                config.influxdb.url.as_str(),
                config.influxdb.database.as_str(),
            )
            .with_auth(&username, &password)
        }
    };
    let (query_sender, query_receiver) = bounded(5000);
    for (topic_name, topic) in config.topics.iter() {
        tokio::spawn(topic.clone().run(
            topic_name.clone(),
            config.ckb_network_name.clone(),
            influx.clone(),
            query_sender.clone(),
        ));
    }

    let hostname = var("HOSTNAME")
        .unwrap_or_else(|_| gethostname::gethostname().to_string_lossy().to_string());
    for mut query in query_receiver {
        // Attach built-in tags
        query = query
            .add_tag("network", config.ckb_network_name.clone())
            .add_tag("hostname", hostname.clone());

        // Writes asynchronously
        let asynchronize = true;
        if asynchronize {
            let influx_ = influx.clone();
            tokio::spawn(async move {
                if let Err(err) = influx_.query(&query).await {
                    log::error!("influxdb.query, error: {}", err);
                }
            });
        } else if let Err(err) = influx.query(&query).await {
            log::error!("influxdb.query, error: {}", err);
        }
    }
}
