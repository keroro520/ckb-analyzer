use ckb_network::{multiaddr::MultiAddr, MultiaddrExt};
use ckb_suite_rpc::Jsonrpc;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::time::Duration;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct NetworkTopologyConfig {
    pub ckb_rpc_urls: Vec<String>,
}

pub struct NetworkTopology {
    config: NetworkTopologyConfig,
    rpcs: Vec<Jsonrpc>,
}

impl NetworkTopology {
    pub fn new(config: NetworkTopologyConfig) -> Self {
        let rpcs: Vec<_> = config
            .ckb_rpc_urls
            .iter()
            .map(|url| Jsonrpc::connect(url.as_ref()))
            .collect();
        Self { config, rpcs }
    }

    pub async fn run(&self) {
        println!("run {}", ::std::any::type_name::<Self>());
        loop {
            self.analyze().await;
            tokio::time::delay_for(Duration::from_secs(60 * 10)).await;
        }
    }

    pub async fn analyze(&self) {
        let mut connections = HashSet::new();
        for rpc in self.rpcs.iter() {
            let local = rpc.local_node_info();
            let local_ip = extract_ip(&local.addresses[0].address);
            for remote in rpc.get_peers() {
                let remote_ip = extract_ip(&remote.addresses[0].address);
                if local_ip > remote_ip {
                    connections.insert((local_ip.clone(), remote_ip));
                } else {
                    connections.insert((remote_ip, local_ip.clone()));
                }
            }
        }

        println!("graph Topology {{");

        let known_ips = self
            .config
            .ckb_rpc_urls
            .iter()
            .map(|ckb_url| extract_ip(ckb_url.as_ref()))
            .collect::<HashSet<_>>();
        let dot2line = |ip: &str| {
            let new = ip.replace(".", "_");
            format!("host_{}", new)
        };

        {
            let mut counts = HashMap::new();
            connections.iter().for_each(|(inbound, outbound)| {
                if known_ips.contains(inbound) && known_ips.contains(outbound) {
                    let entry = counts.entry(inbound.to_owned()).or_insert(0);
                    *entry += 1;
                    let entry = counts.entry(outbound.to_owned()).or_insert(0);
                    *entry += 1;
                }
            });
            connections.iter().for_each(|(inbound, outbound)| {
                if let Some(count) = counts.remove(inbound) {
                    println!(
                        "    {} [ label = \"{} *{}*\" ];",
                        dot2line(inbound),
                        inbound,
                        count
                    );
                }
                if let Some(count) = counts.remove(outbound) {
                    println!(
                        "    {} [ label = \"{} *{}*\" ];",
                        dot2line(outbound),
                        outbound,
                        count
                    );
                }
            });
        }

        {
            connections.iter().for_each(|(inbound, outbound)| {
                if known_ips.contains(inbound) && known_ips.contains(outbound) {
                    println!("    {} -- {};", dot2line(inbound), dot2line(outbound));
                }
            });
        }
        println!("}}");
    }
}

// FIXME Use more accurate identifier
fn extract_ip(address: &str) -> String {
    if let Ok(multiaddr) = address.parse::<MultiAddr>() {
        let ip_port = multiaddr.extract_ip_addr().unwrap();
        ip_port.ip.to_string()
    } else if let Ok(socket_addr) = address[7..].parse::<SocketAddr>() {
        // FIXME ugly
        socket_addr.ip().to_string()
    } else {
        panic!("cannot parse {}", address)
    }
}
