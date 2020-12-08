use crate::serie::{self, IntoWriteQuery};
use crate::subscribe::{Subscription, Topic};
use ckb_types::core::{BlockNumber, BlockView, HeaderView};
use ckb_types::packed::Byte32;
use crossbeam::channel::Sender;
use influxdb::{Timestamp, WriteQuery};
use jsonrpc_core::futures::Stream;
use jsonrpc_core::serde_from_str;
use jsonrpc_server_utils::tokio::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ForkConfig {
    pub ckb_subscribe_url: String,
}

pub const MAX_REORGANIZE_LENGTH: u64 = 500;

pub struct Fork {
    block_receiver: jsonrpc_server_utils::tokio::sync::mpsc::Receiver<String>,
    query_sender: Sender<WriteQuery>,
    main_tip_number: BlockNumber,
    main_tip_hash: Byte32,
    main_chain: HashMap<BlockNumber, Byte32>,
    // #{ block_hash => header }
    cache: HashMap<Byte32, HeaderView>,
}

impl Fork {
    pub fn init(config: ForkConfig, query_sender: Sender<WriteQuery>) -> (Self, Subscription) {
        let (block_sender, block_receiver) = jsonrpc_server_utils::tokio::sync::mpsc::channel(100);
        let subscription =
            Subscription::new(config.ckb_subscribe_url, Topic::NewTipBlock, block_sender);
        (
            Self {
                block_receiver,
                query_sender,
                main_tip_number: Default::default(),
                main_tip_hash: Default::default(),
                main_chain: Default::default(),
                cache: Default::default(),
            },
            subscription,
        )
    }

    pub async fn run(self) {
        println!("run {}", ::std::any::type_name::<Self>());
        self.block_receiver
            .for_each(|message| {
                let block: ckb_suite_rpc::ckb_jsonrpc_types::BlockView = serde_from_str(&message)
                    .unwrap_or_else(|err| {
                        panic!("serde_from_str(\"{}\"), error: {:?}", message, err)
                    });
                let block: BlockView = block.into();
                println!("fork run block: {}", block.number());
                Ok(())
            })
            .wait()
            .unwrap_or_else(|err| panic!("receiver error {:?}", err));
    }

    fn handle(&mut self, block: &BlockView) {
        self.cache.insert(block.hash(), block.header());

        if self.main_tip_hash == block.parent_hash() || self.main_chain.is_empty() {
            self.main_chain.insert(block.number(), block.hash());
            self.main_tip_hash = block.hash();
            self.main_tip_number = block.number();
            return;
        }

        let parent = self
            .cache
            .get(&block.parent_hash())
            .unwrap_or_else(|| panic!("fork.cache.get(block.parent) none"));
        let detached_length = self.main_tip_number.saturating_sub(parent.number());
        let query = serie::Fork {
            time: Timestamp::Milliseconds(block.timestamp() as u128),
            detached_length: detached_length as u32,
        }
        .into_write_query();
        self.query_sender.send(query).unwrap();

        for number in block.number()..=self.main_tip_number {
            self.main_chain.remove(&number);
        }
        self.main_chain.insert(block.number(), block.hash());
        self.main_tip_hash = block.hash();
        self.main_tip_number = block.number();
    }
}
