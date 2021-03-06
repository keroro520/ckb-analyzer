use crate::util::forward_tokio1_channel;
use jsonrpc_client_transports::RpcError;
use jsonrpc_core::{futures::prelude::*, Result};
use jsonrpc_core_client::{
    transports::duplex::{duplex, Duplex},
    RpcChannel, TypedSubscriptionStream,
};
use jsonrpc_derive::rpc;
use jsonrpc_pubsub::{typed::Subscriber, SubscriptionId};
use jsonrpc_server_utils::{codecs::StreamCodec, tokio::codec::Decoder, tokio::net::TcpStream};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;

#[derive(Serialize, Deserialize, PartialEq, Eq, Hash, Debug, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum Topic {
    /// Subscribe new tip headers.
    NewTipHeader,
    /// Subscribe new tip blocks.
    NewTipBlock,
    /// Subscribe new transactions which are submitted to the pool.
    NewTransaction,
    /// Subscribe in-pool transactions which proposed on chain.
    ProposedTransaction,
    /// Subscribe transactions which are abandoned by tx-pool.
    RejectedTransaction,
}

#[allow(clippy::needless_return)]
#[rpc]
pub trait SubscriptionRpc {
    type Metadata;

    #[pubsub(subscription = "subscribe", subscribe, name = "subscribe")]
    fn subscribe(&self, meta: Self::Metadata, subscriber: Subscriber<String>, topic: Topic);

    #[pubsub(subscription = "subscribe", unsubscribe, name = "unsubscribe")]
    fn unsubscribe(&self, meta: Option<Self::Metadata>, id: SubscriptionId) -> Result<bool>;
}

#[derive(Debug, Clone)]
pub struct Subscription {
    address: SocketAddr,
    topic: Topic,
    publisher: crate::tokio01::sync::mpsc::Sender<(Topic, String)>,
}

impl Subscription {
    pub fn new(
        ckb_subscription_url: String,
        topic: Topic,
    ) -> (Self, crossbeam::channel::Receiver<(Topic, String)>) {
        let (publisher, subscriber) = {
            let (publisher, subscriber) = crate::tokio01::sync::mpsc::channel(100);
            let subscriber = forward_tokio1_channel(subscriber);
            (publisher, subscriber)
        };
        let address = ckb_subscription_url
            .parse::<SocketAddr>()
            .unwrap_or_else(|err| {
                panic!("failed to parse {}, error: {:?}", ckb_subscription_url, err)
            });
        (
            Self {
                address,
                topic,
                publisher,
            },
            subscriber,
        )
    }

    // IMPORTANT: This task use `jsonrpc_server_utils::tokio`, which version is 0.1.x. It is
    // incompatible with tokio 0.2! So use jsonrpc_server_utils::tokio as the runtime!
    pub fn run(self) -> impl jsonrpc_core::futures::Future<Item = (), Error = ()> {
        let rawio = TcpStream::connect(&self.address).wait().unwrap();
        let codec = StreamCodec::stream_incoming();
        let framed = Decoder::framed(codec, rawio);

        // Framed is a unified of sink and stream. In order to construct the duplex interface, we need
        // to split framed-sink and framed-stream from framed.
        let (sink, stream) = {
            let (sink, stream) = framed.split();

            // Cast the error to pass the compile
            let sink = sink.sink_map_err(|err| RpcError::Other(err.into()));
            let stream = stream.map_err(|err| RpcError::Other(err.into()));
            (sink, stream)
        };
        let (duplex, sender_channel): (Duplex<_, _>, RpcChannel) = duplex(sink, stream);

        // Construct rpc client which sends messages(requests) to server, and subscribe `NewTipBlock`
        // from server. We get a typed stream of subscription.
        let requester = gen_client::Client::from(sender_channel);
        let publisher = self.publisher.clone();
        let topic = self.topic;
        let subscription = requester.subscribe(topic).and_then(
            move |subscriber: TypedSubscriptionStream<String>| {
                subscriber.for_each(move |message| {
                    publisher
                        .clone()
                        .send((topic, message))
                        .wait()
                        .unwrap_or_else(|err| panic!("channel error: {:?}", err));
                    Ok(())
                })
            },
        );
        log::info!(
            "subscribe topic \"{:?}\" from \"{}\"",
            self.topic,
            self.address,
        );
        duplex
            .join(subscription)
            .map(|_| ())
            .map_err(|err| panic!("map_err error {:?}", err))
    }
}
