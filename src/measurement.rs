pub use influxdb::{InfluxDbWriteable, Timestamp, WriteQuery};

// TODO provide a comamnd to list all the measurements; even print the show table influxsql, like `use
// $database; show tables`

// TODO when document these measurements, please describe the related dashboards as well, which can
// help underdand the measuement more deeply.

pub trait IntoWriteQuery: InfluxDbWriteable {
    fn query_name(&self) -> String
    where
        Self: std::marker::Sized,
    {
        let type_name = tynm::type_name::<Self>();
        case_style::CaseStyle::guess(type_name)
            .unwrap_or_else(|_| panic!("failed to convert the type name to kebabcase style"))
            .to_kebabcase()
    }

    fn into_write_query(self) -> WriteQuery
    where
        Self: std::marker::Sized,
    {
        let query_name = self.query_name();
        InfluxDbWriteable::into_query(self, query_name)
    }
}

impl<T: InfluxDbWriteable> IntoWriteQuery for T {}

#[derive(InfluxDbWriteable, Clone, Debug)]
pub struct Block {
    pub time: Timestamp,

    pub number: u64,
    pub time_interval: u64, // ms
    pub transactions_count: u32,
    pub uncles_count: u32,
    pub proposals_count: u32,
    pub version: u32,

    #[tag]
    pub miner_lock_args: String,
}

#[derive(InfluxDbWriteable, Clone, Debug)]
pub struct Uncle {
    pub time: Timestamp,

    pub number: u64,
    pub transactions_count: u32,
    pub proposals_count: u32,
    pub version: u32,
    // TODO rename ltcousin
    pub slower_than_cousin: i64,

    #[tag]
    pub miner_lock_args: String,
}

#[derive(InfluxDbWriteable, Clone, Debug)]
pub struct Epoch {
    pub time: Timestamp,

    pub number: u64,
    pub length: u64,
    pub duration: u64, // ms
    pub uncles_count: u32,
}

#[derive(InfluxDbWriteable, Clone, Debug)]
pub struct Transaction {
    pub time: Timestamp,

    pub number: u64, // block number
    pub pc_delay: u32,
}

#[derive(InfluxDbWriteable, Clone, Debug)]
pub struct Propagation {
    pub time: Timestamp,

    pub time_interval: u64, // ms

    #[tag]
    pub percentile: u32,
    #[tag]
    pub message_type: String,
}

#[derive(InfluxDbWriteable, Clone, Debug)]
pub struct HighLatency {
    pub time: Timestamp,

    pub time_interval: u64,

    #[tag]
    pub addr: String,
}

#[derive(InfluxDbWriteable, Clone, Debug)]
pub struct Peers {
    pub time: Timestamp,
    pub peers_total: u32,
}

#[derive(InfluxDbWriteable, Clone, Debug)]
pub struct Reorganization {
    // timestamp of fixed point of forks
    pub time: Timestamp,
    pub attached_length: u32,
    pub old_tip_number: u64,
    pub new_tip_number: u64,
    pub ancestor_number: u64,

    #[tag]
    pub old_tip_hash: String,
    #[tag]
    pub new_tip_hash: String,
    #[tag]
    pub ancestor_hash: String,
}

#[derive(InfluxDbWriteable, Clone, Debug)]
pub struct TxTransition {
    // timestamp of entering in transaction pool
    pub time: Timestamp,

    pub elapsed: u64, // ms

    #[tag]
    pub event: String,
    #[tag]
    pub txhash: String,
}

#[derive(InfluxDbWriteable, Clone, Debug)]
pub struct PoolWaitingTransactionsTotal {
    pub time: Timestamp,
    pub transactions_total: u32,
}

#[derive(InfluxDbWriteable, Clone, Debug)]
pub struct Log {
    pub time: Timestamp,
    pub marker: u32, // always be 1
    #[tag]
    pub category: String,
    #[tag]
    pub raw: String,
}
