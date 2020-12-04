use serde::{Deserialize, Serialize};
use crossbeam::channel::Sender;
use std::collections::HashMap;
use ckb_types::core::BlockNumber;
use ckb_types::packed::Byte32;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ForkConfig {
    pub ckb_subscribe_url: String,
}

pub struct Fork {
    // #{block_number => #{block_hash => parent_hash}}
    cache: HashMap<BlockNumber, HashMap<Byte32, Byte32>>
}