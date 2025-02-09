use ckb_jsonrpc_types::{BlockNumber, Script, Uint64};
use jsonrpsee::{
    core::{async_trait, Error},
    proc_macros::rpc,
};
use serde::{Deserialize, Serialize};

use std::sync::{atomic::AtomicPtr, Arc};

use crate::{
    cell_process::CellProcess,
    rpc_client::{IndexerTip, RpcClient, ScriptType, SearchKey, SearchKeyFilter},
    ScanTip, ScanTipInner,
};

#[derive(Serialize, Deserialize, Debug, Clone, Hash, PartialEq, Eq)]
pub struct RpcSearchKey {
    pub script: Script,
    pub script_type: ScriptType,
    pub filter: Option<RpcSearchKeyFilter>,
}

impl RpcSearchKey {
    pub fn into_key(self, block_range: Option<[Uint64; 2]>) -> SearchKey {
        SearchKey {
            script: self.script,
            script_type: self.script_type,
            filter: if self.filter.is_some() {
                self.filter.map(|f| f.into_filter(block_range))
            } else {
                Some(RpcSearchKeyFilter::default().into_filter(block_range))
            },
            with_data: None,
            group_by_transaction: Some(true),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, Hash, PartialEq, Eq)]
pub struct RpcSearchKeyFilter {
    pub script: Option<Script>,
    pub script_len_range: Option<[Uint64; 2]>,
    pub output_data_len_range: Option<[Uint64; 2]>,
    pub output_capacity_range: Option<[Uint64; 2]>,
}

impl RpcSearchKeyFilter {
    fn into_filter(self, block_range: Option<[Uint64; 2]>) -> SearchKeyFilter {
        SearchKeyFilter {
            script: self.script,
            script_len_range: self.script_len_range,
            output_data_len_range: self.output_data_len_range,
            output_capacity_range: self.output_capacity_range,
            block_range,
        }
    }
}

#[rpc(server)]
pub trait Emitter {
    #[method(name = "register")]
    async fn register(&self, search_key: RpcSearchKey, start: BlockNumber) -> Result<bool, Error>;

    #[method(name = "delete")]
    async fn delete(&self, search_key: RpcSearchKey) -> Result<bool, Error>;

    #[method(name = "info")]
    async fn info(&self) -> Result<Vec<(RpcSearchKey, ScanTip)>, Error>;
}

pub(crate) struct EmitterRpc {
    pub state: Arc<dashmap::DashMap<RpcSearchKey, ScanTip>>,
    pub cell_handles: dashmap::DashMap<RpcSearchKey, tokio::task::JoinHandle<()>>,
    pub client: RpcClient,
}

#[async_trait]
impl EmitterServer for EmitterRpc {
    async fn register(&self, search_key: RpcSearchKey, start: BlockNumber) -> Result<bool, Error> {
        if self.state.contains_key(&search_key) {
            return Ok(false);
        }
        let indexer_tip = self
            .client
            .get_indexer_tip()
            .await
            .map_err(|e| Error::Custom(e.to_string()))?;

        if indexer_tip.block_number > start {
            let header = self
                .client
                .get_header_by_number(start)
                .await
                .map_err(|e| Error::Custom(e.to_string()))?;

            let scan_tip = {
                let tip = IndexerTip {
                    block_hash: header.hash,
                    block_number: header.inner.number,
                };
                ScanTip(Arc::new(ScanTipInner(AtomicPtr::new(Box::into_raw(
                    Box::new(tip),
                )))))
            };

            self.state.insert(search_key.clone(), scan_tip.clone());

            let mut cell_process = CellProcess {
                key: search_key.clone(),
                client: self.client.clone(),
                scan_tip,
            };

            let handle = tokio::spawn(async move {
                cell_process.run().await;
            });

            self.cell_handles.insert(search_key, handle);
            return Ok(true);
        }

        Ok(false)
    }

    async fn delete(&self, search_key: RpcSearchKey) -> Result<bool, Error> {
        if self.state.remove(&search_key).is_some() {
            let handle = self.cell_handles.remove(&search_key).unwrap();
            handle.1.abort();
            return Ok(true);
        }
        Ok(false)
    }

    async fn info(&self) -> Result<Vec<(RpcSearchKey, ScanTip)>, Error> {
        Ok(self
            .state
            .iter()
            .map(|kv| (kv.key().clone(), kv.value().clone()))
            .collect::<Vec<_>>())
    }
}
