use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Hash)]
pub struct Config {
    pub chain: ChainConfig,
    pub network: NetworkConfig,
    pub topology: TopologyConfig,
    pub fork: ForkConfig,
    pub influxdb: InfluxdbConfig,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Hash)]
pub struct ForkConfig {
    #[serde(default)]
    pub enabled: bool,
    pub ckb_subscription_url: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Hash)]
pub struct ChainConfig {
    #[serde(default)]
    pub enabled: bool,
    pub ckb_url: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Hash)]
pub struct NetworkConfig {
    #[serde(default)]
    pub enabled: bool,
    pub ckb_network_name: String,
    pub ckb_network_identifier: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Hash)]
pub struct TopologyConfig {
    #[serde(default)]
    pub enabled: bool,
    pub ckb_urls: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Hash)]
pub struct InfluxdbConfig {
    #[serde(default)]
    pub enabled: bool,
    pub database: String,
    pub url: String,
}

pub fn init_config<P: AsRef<Path>>(filepath: P) -> Config {
    let bytes =
        fs::read_to_string(filepath).unwrap_or_else(|err| panic!("fs::read error: {:?}", err));
    toml::from_str(&bytes).unwrap_or_else(|err| panic!("toml::from_str error: {:?}", err))
}
