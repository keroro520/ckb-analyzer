use crossbeam::channel::Sender;
use jsonrpc_client_transports::RpcError;
use jsonrpc_core::{futures::prelude::*, serde_from_str, Result};
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
    NewTipHeader,
    NewTipBlock,
    NewTransaction,
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
    pub address: SocketAddr,
    pub topic: Topic,
    pub senders: Vec<Sender<String>>,
}

impl Subscription {
    pub fn new(ckb_subscription_url: String, topic: Topic) -> Self {
        let address = ckb_subscription_url
            .parse::<SocketAddr>()
            .unwrap_or_else(|err| {
                panic!("failed to parse {}, error: {:?}", ckb_subscription_url, err)
            });
        Self {
            address,
            topic,
            senders: Vec::new(),
        }
    }

    pub fn add_sender(mut self, sender: Sender<String>) -> Self {
        self.senders.push(sender);
        self
    }

    pub async fn run(self) {
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
        let subscription_future = requester.subscribe(self.topic).and_then(
            |subscriber: TypedSubscriptionStream<String>| {
                subscriber.for_each(move |message| {
                    for sender in self.senders.iter() {
                        sender
                            .send(message.clone())
                            .unwrap_or_else(|err| panic!("channel error: {:?}", err));
                    }
                    //
                    // let block: ckb_suite_rpc::ckb_jsonrpc_types::BlockView =
                    //     serde_from_str(&message)
                    //         .map_err(|err| RpcError::ParseError(message, err.into()))?;
                    // let block: ckb_types::core::BlockView = block.into();
                    // println!("bilibili subscribe block #{}", block.number());
                    Ok(())
                })
            },
        );
        duplex
            .join(subscription_future)
            .map(|_| ())
            .map_err(|err| panic!("{:?}", err))
            .wait()
            .unwrap();
    }
}
