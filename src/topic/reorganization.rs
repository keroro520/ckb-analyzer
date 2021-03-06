//! This module aims to the instant chain reorganization events.
//! This module produces the below measurements:
//!   - [Reorganization](TODO link to Reorganization)
//!
//! ### How it works?
//!
//! There is an thread subscribed to CKB on the topic "NewTipHeader"(see more about CKB subscription
//! interface from [..](TODO link to ckb subscribe). This subscription will notify us whenever the
//! tip changes. Via observing the tip changing, we construct the last part of main chain. Then we
//! can observe a chain reorganization occurs by comparing the arrival tip change with our
//! constructed main chain.
//!
//! ### Why measure it?
//!
//! Monitoring to chain reorganization may help to:
//!   - detect selfish mining attacks
//!   - understand the network between miners

// TODO rename main_ to canonical_; remove prefix main_ from main_tip_hash/main_tip_number

use crate::config::Config;
use crate::subscribe::{Subscription, Topic};
use crate::util::retry_send;
use ckb_suite_rpc::{ckb_jsonrpc_types::HeaderView as JsonHeader, Jsonrpc};
use ckb_types::core::{BlockNumber, HeaderView};
use ckb_types::packed::Byte32;
use jsonrpc_core::serde_from_str;
use std::time::Duration;

pub(crate) struct Reorganization {
    config: Config,
    subscriber: crossbeam::channel::Receiver<(Topic, String)>,
    jsonrpc: Jsonrpc,
    query_sender: crossbeam::channel::Sender<String>,
    main_tip_number: BlockNumber,
    main_tip_hash: Byte32,
}

impl Reorganization {
    pub(crate) fn new(
        config: Config,
        jsonrpc: Jsonrpc,
        query_sender: crossbeam::channel::Sender<String>,
    ) -> (Self, Subscription) {
        let (subscription, subscriber) =
            Subscription::new(config.subscription_url(), Topic::NewTipHeader);
        (
            Self {
                config,
                jsonrpc,
                query_sender,
                subscriber,
                main_tip_number: 0,
                main_tip_hash: Default::default(),
            },
            subscription,
        )
    }

    async fn try_recv_subscription(
        &self,
    ) -> Result<(Topic, String), crossbeam::channel::TryRecvError> {
        self.subscriber.try_recv()
    }

    pub(crate) async fn run(mut self) {
        loop {
            match self.try_recv_subscription().await {
                Ok((topic, message)) => {
                    assert_eq!(Topic::NewTipHeader, topic);
                    let header: JsonHeader = serde_from_str(&message).unwrap();
                    let header: HeaderView = header.into();
                    self.handle(&header).await;
                }
                Err(crossbeam::channel::TryRecvError::Disconnected) => return,
                Err(crossbeam::channel::TryRecvError::Empty) => {
                    tokio::time::sleep(Duration::from_secs(1)).await
                }
            }
        }
    }

    async fn handle(&mut self, header: &HeaderView) {
        if self.main_tip_hash == header.parent_hash() || self.main_tip_number == 0 {
            self.main_tip_hash = header.hash();
            self.main_tip_number = header.number();
        } else {
            let new_tip = header;
            let old_tip = self.get_header(self.main_tip_hash.clone()).await;
            let ancestor = self.locate_ancestor(&old_tip, new_tip).await;
            self.report_reorganization(new_tip, &old_tip, &ancestor)
                .await;

            self.main_tip_hash = header.hash();
            self.main_tip_number = header.number();
        }
    }

    async fn locate_ancestor(&mut self, old_tip: &HeaderView, new_tip: &HeaderView) -> HeaderView {
        let mut old_tip = old_tip.clone();
        let mut new_tip = new_tip.clone();
        #[allow(clippy::comparison_chain)]
        if old_tip.number() > new_tip.number() {
            for _ in 0..old_tip.number() - new_tip.number() {
                old_tip = self.get_header(old_tip.parent_hash()).await;
            }
        } else if old_tip.number() < new_tip.number() {
            for _ in 0..new_tip.number() - old_tip.number() {
                new_tip = self.get_header(new_tip.parent_hash()).await;
            }
        }
        assert_eq!(old_tip.number(), new_tip.number());
        while old_tip.hash() != new_tip.hash() {
            old_tip = self.get_header(old_tip.parent_hash()).await;
            new_tip = self.get_header(new_tip.parent_hash()).await;
        }
        old_tip
    }

    async fn report_reorganization(
        &mut self,
        new_tip: &HeaderView,
        old_tip: &HeaderView,
        ancestor: &HeaderView,
    ) {
        let attached_length = new_tip.number() - ancestor.number();
        log::info!(
            "Reorganize from #{}({:#x}) to #{}({:#x}), attached_length = {}",
            old_tip.number(),
            old_tip.hash(),
            new_tip.number(),
            new_tip.hash(),
            attached_length
        );

        let time = chrono::NaiveDateTime::from_timestamp(
            (ancestor.timestamp() / 1000) as i64,
            (ancestor.timestamp() % 1000 * 1000) as u32,
        );
        let point = crate::table::Reorganization {
            network: self.config.network(),
            time,
            attached_length: attached_length as i32,
            old_tip_number: old_tip.number() as i64,
            new_tip_number: new_tip.number() as i64,
            ancestor_number: ancestor.number() as i64,
            old_tip_hash: format!("{:#x}", old_tip.hash()),
            new_tip_hash: format!("{:#x}", new_tip.hash()),
            ancestor_hash: format!("{:#x}", ancestor.hash()),
        };
        retry_send(&self.query_sender, point.insert_query()).await;
    }

    async fn get_header(&mut self, block_hash: Byte32) -> HeaderView {
        if let Some(header) = self.jsonrpc.get_header(block_hash.clone()) {
            return header.into();
        }

        if let Some(block) = self.jsonrpc.get_fork_block(block_hash) {
            return block.header.into();
        }

        unreachable!()
    }
}
