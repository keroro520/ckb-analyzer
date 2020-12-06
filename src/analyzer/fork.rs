use crate::subscribe::{Subscription, Topic};
use ckb_types::core::BlockNumber;
use ckb_types::packed::Byte32;
use crossbeam::channel::{bounded, Receiver};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ForkConfig {
    pub ckb_subscribe_url: String,
}

pub struct Fork {
    // #{block_number => #{block_hash => parent_hash}}
    cache: HashMap<BlockNumber, HashMap<Byte32, Byte32>>,
    receiver: Receiver<String>,
}

impl Fork {
    pub fn init(config: ForkConfig) -> (Self, Subscription) {
        let (sender, receiver) = bounded(5000);
        let subscription = Subscription::new(config.ckb_subscribe_url, Topic::NewTipBlock, sender);
        (
            Self {
                receiver,
                cache: Default::default(),
            },
            subscription,
        )
    }

    pub async fn run(self) {
        while let Ok(msg) = self.receiver.recv() {
            println!("bilibili Fork::run msg: {}", msg);
        }
    }
}
