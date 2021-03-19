#![cfg_attr(feature = "docinclude", feature(external_doc))]
#![cfg_attr(feature = "docinclude", doc(include = "../README.md"))]

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Cursor;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use async_raft::async_trait::async_trait;
use async_raft::error::{ChangeConfigError, ClientReadError, ClientWriteError};
use async_raft::raft::{Entry, EntryPayload, MembershipConfig};
use async_raft::storage::{CurrentSnapshotData, HardState, InitialState};
use async_raft::{AppData, AppDataResponse, NodeId, RaftStorage};

use async_raft::raft::{AppendEntriesRequest, AppendEntriesResponse, ClientWriteRequest};
use async_raft::raft::{InstallSnapshotRequest, InstallSnapshotResponse};
use async_raft::raft::{VoteRequest, VoteResponse};
use async_raft::{Config, Raft, RaftMetrics, RaftNetwork};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::RwLock;
use tokio::sync::{RwLockReadGuard, RwLockWriteGuard};

const ERR_INCONSISTENT_LOG: &str =
    "a query was received which was expecting data to be in place which does not exist in the log";

/// The application data request type which the `WALStore` works with.
///
/// Conceptually, for demo purposes, this represents an update to a client's status info,
/// returning the previously recorded status.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ClientRequest {
    /// The ID of the client which has sent the request.
    pub client: String,
    /// The serial number of this request.
    pub serial: u64,
    /// A string describing the status of the client. For a real application, this should probably
    /// be an enum representing all of the various types of requests / operations which a client
    /// can perform.
    pub status: String,
}

impl AppData for ClientRequest {}

/// The application data response type which the `WALStore` works with.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ClientResponse(Option<String>);

impl AppDataResponse for ClientResponse {}

/// Error used to trigger Raft shutdown from storage.
#[derive(Clone, Debug, Error)]
pub enum ShutdownError {
    #[error("unsafe storage error")]
    UnsafeStorageError,
}

/// The application snapshot type which the `WALStore` works with.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct WALStoreSnapshot {
    /// The last index covered by this snapshot.
    pub index: u64,
    /// The term of the last index covered by this snapshot.
    pub term: u64,
    /// The last memberhsip config included in this snapshot.
    pub membership: MembershipConfig,
    /// The data of the state machine at the time of this snapshot.
    pub data: Vec<u8>,
}

/// The state machine of the `WALStore`.
#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub struct WALStoreStateMachine {
    pub last_applied_log: u64,
    /// A mapping of client IDs to their state info.
    pub client_serial_responses: HashMap<String, (u64, Option<String>)>,
    /// The current status of a client by ID.
    pub client_status: HashMap<String, String>,
}

/// An in-memory storage system implementing the `async_raft::RaftStorage` trait.
pub struct WALStore {
    /// The ID of the Raft node for which this memory storage instances is configured.
    id: NodeId,
    /// The Raft log.
    log: RwLock<BTreeMap<u64, Entry<ClientRequest>>>,
    /// The Raft state machine.
    sm: RwLock<WALStoreStateMachine>,
    /// The current hard state.
    hs: RwLock<Option<HardState>>,
    /// The current snapshot.
    current_snapshot: RwLock<Option<WALStoreSnapshot>>,
}

impl WALStore {
    /// Create a new `WALStore` instance.
    pub fn new(id: NodeId) -> Self {
        let log = RwLock::new(BTreeMap::new());
        let sm = RwLock::new(WALStoreStateMachine::default());
        let hs = RwLock::new(None);
        let current_snapshot = RwLock::new(None);
        Self {
            id,
            log,
            sm,
            hs,
            current_snapshot,
        }
    }

    /// Create a new `WALStore` instance with some existing state (for testing).
    // Disabled for now, since I can't figure out how to get it to work: #[cfg(test)]
    pub fn new_with_state(
        id: NodeId,
        log: BTreeMap<u64, Entry<ClientRequest>>,
        sm: WALStoreStateMachine,
        hs: Option<HardState>,
        current_snapshot: Option<WALStoreSnapshot>,
    ) -> Self {
        let log = RwLock::new(log);
        let sm = RwLock::new(sm);
        let hs = RwLock::new(hs);
        let current_snapshot = RwLock::new(current_snapshot);
        Self {
            id,
            log,
            sm,
            hs,
            current_snapshot,
        }
    }

    /// Get a handle to the log for testing purposes.
    pub async fn get_log(&self) -> RwLockWriteGuard<'_, BTreeMap<u64, Entry<ClientRequest>>> {
        self.log.write().await
    }

    /// Get a handle to the state machine for testing purposes.
    pub async fn get_state_machine(&self) -> RwLockWriteGuard<'_, WALStoreStateMachine> {
        self.sm.write().await
    }

    /// Get a handle to the current hard state for testing purposes.
    pub async fn read_hard_state(&self) -> RwLockReadGuard<'_, Option<HardState>> {
        self.hs.read().await
    }
}

#[async_trait]
impl RaftStorage<ClientRequest, ClientResponse> for WALStore {
    type Snapshot = Cursor<Vec<u8>>;
    type ShutdownError = ShutdownError;

    #[tracing::instrument(level = "trace", skip(self))]
    async fn get_membership_config(&self) -> Result<MembershipConfig> {
        let log = self.log.read().await;
        let cfg_opt = log.values().rev().find_map(|entry| match &entry.payload {
            EntryPayload::ConfigChange(cfg) => Some(cfg.membership.clone()),
            EntryPayload::SnapshotPointer(snap) => Some(snap.membership.clone()),
            _ => None,
        });
        Ok(match cfg_opt {
            Some(cfg) => cfg,
            None => MembershipConfig::new_initial(self.id),
        })
    }

    #[tracing::instrument(level = "trace", skip(self))]
    async fn get_initial_state(&self) -> Result<InitialState> {
        let membership = self.get_membership_config().await?;
        let mut hs = self.hs.write().await;
        let log = self.log.read().await;
        let sm = self.sm.read().await;
        match &mut *hs {
            Some(inner) => {
                let (last_log_index, last_log_term) = match log.values().rev().next() {
                    Some(log) => (log.index, log.term),
                    None => (0, 0),
                };
                let last_applied_log = sm.last_applied_log;
                Ok(InitialState {
                    last_log_index,
                    last_log_term,
                    last_applied_log,
                    hard_state: inner.clone(),
                    membership,
                })
            }
            None => {
                let new = InitialState::new_initial(self.id);
                *hs = Some(new.hard_state.clone());
                Ok(new)
            }
        }
    }

    #[tracing::instrument(level = "trace", skip(self, hs))]
    async fn save_hard_state(&self, hs: &HardState) -> Result<()> {
        *self.hs.write().await = Some(hs.clone());
        Ok(())
    }

    #[tracing::instrument(level = "trace", skip(self))]
    async fn get_log_entries(&self, start: u64, stop: u64) -> Result<Vec<Entry<ClientRequest>>> {
        // Invalid request, return empty vec.
        if start > stop {
            tracing::error!("invalid request, start > stop");
            return Ok(vec![]);
        }
        let log = self.log.read().await;
        Ok(log.range(start..stop).map(|(_, val)| val.clone()).collect())
    }

    #[tracing::instrument(level = "trace", skip(self))]
    async fn delete_logs_from(&self, start: u64, stop: Option<u64>) -> Result<()> {
        if stop.as_ref().map(|stop| &start > stop).unwrap_or(false) {
            tracing::error!("invalid request, start > stop");
            return Ok(());
        }
        let mut log = self.log.write().await;

        // If a stop point was specified, delete from start until the given stop point.
        if let Some(stop) = stop.as_ref() {
            for key in start..*stop {
                log.remove(&key);
            }
            return Ok(());
        }
        // Else, just split off the remainder.
        log.split_off(&start);
        Ok(())
    }

    #[tracing::instrument(level = "trace", skip(self, entry))]
    async fn append_entry_to_log(&self, entry: &Entry<ClientRequest>) -> Result<()> {
        let mut log = self.log.write().await;
        log.insert(entry.index, entry.clone());
        Ok(())
    }

    #[tracing::instrument(level = "trace", skip(self, entries))]
    async fn replicate_to_log(&self, entries: &[Entry<ClientRequest>]) -> Result<()> {
        let mut log = self.log.write().await;
        for entry in entries {
            log.insert(entry.index, entry.clone());
        }
        Ok(())
    }

    #[tracing::instrument(level = "trace", skip(self, data))]
    async fn apply_entry_to_state_machine(
        &self,
        index: &u64,
        data: &ClientRequest,
    ) -> Result<ClientResponse> {
        let mut sm = self.sm.write().await;
        sm.last_applied_log = *index;
        if let Some((serial, res)) = sm.client_serial_responses.get(&data.client) {
            if serial == &data.serial {
                return Ok(ClientResponse(res.clone()));
            }
        }
        let previous = sm
            .client_status
            .insert(data.client.clone(), data.status.clone());
        sm.client_serial_responses
            .insert(data.client.clone(), (data.serial, previous.clone()));
        Ok(ClientResponse(previous))
    }

    #[tracing::instrument(level = "trace", skip(self, entries))]
    async fn replicate_to_state_machine(&self, entries: &[(&u64, &ClientRequest)]) -> Result<()> {
        let mut sm = self.sm.write().await;
        for (index, data) in entries {
            sm.last_applied_log = **index;
            if let Some((serial, _)) = sm.client_serial_responses.get(&data.client) {
                if serial == &data.serial {
                    continue;
                }
            }
            let previous = sm
                .client_status
                .insert(data.client.clone(), data.status.clone());
            sm.client_serial_responses
                .insert(data.client.clone(), (data.serial, previous.clone()));
        }
        Ok(())
    }

    #[tracing::instrument(level = "trace", skip(self))]
    async fn do_log_compaction(&self) -> Result<CurrentSnapshotData<Self::Snapshot>> {
        let (data, last_applied_log);
        {
            // Serialize the data of the state machine.
            let sm = self.sm.read().await;
            data = serde_json::to_vec(&*sm)?;
            last_applied_log = sm.last_applied_log;
        } // Release state machine read lock.

        let membership_config;
        {
            // Go backwards through the log to find the most recent membership config <= the `through` index.
            let log = self.log.read().await;
            membership_config = log
                .values()
                .rev()
                .skip_while(|entry| entry.index > last_applied_log)
                .find_map(|entry| match &entry.payload {
                    EntryPayload::ConfigChange(cfg) => Some(cfg.membership.clone()),
                    _ => None,
                })
                .unwrap_or_else(|| MembershipConfig::new_initial(self.id));
        } // Release log read lock.

        let snapshot_bytes: Vec<u8>;
        let term;
        {
            let mut log = self.log.write().await;
            let mut current_snapshot = self.current_snapshot.write().await;
            term = log
                .get(&last_applied_log)
                .map(|entry| entry.term)
                .ok_or_else(|| anyhow::anyhow!(ERR_INCONSISTENT_LOG))?;
            *log = log.split_off(&last_applied_log);
            log.insert(
                last_applied_log,
                Entry::new_snapshot_pointer(
                    last_applied_log,
                    term,
                    "".into(),
                    membership_config.clone(),
                ),
            );

            let snapshot = WALStoreSnapshot {
                index: last_applied_log,
                term,
                membership: membership_config.clone(),
                data,
            };
            snapshot_bytes = serde_json::to_vec(&snapshot)?;
            *current_snapshot = Some(snapshot);
        } // Release log & snapshot write locks.

        tracing::trace!(
            { snapshot_size = snapshot_bytes.len() },
            "log compaction complete"
        );
        Ok(CurrentSnapshotData {
            term,
            index: last_applied_log,
            membership: membership_config.clone(),
            snapshot: Box::new(Cursor::new(snapshot_bytes)),
        })
    }

    #[tracing::instrument(level = "trace", skip(self))]
    async fn create_snapshot(&self) -> Result<(String, Box<Self::Snapshot>)> {
        Ok((String::from(""), Box::new(Cursor::new(Vec::new())))) // Snapshot IDs are insignificant to this storage engine.
    }

    #[tracing::instrument(level = "trace", skip(self, snapshot))]
    async fn finalize_snapshot_installation(
        &self,
        index: u64,
        term: u64,
        delete_through: Option<u64>,
        id: String,
        snapshot: Box<Self::Snapshot>,
    ) -> Result<()> {
        tracing::trace!(
            { snapshot_size = snapshot.get_ref().len() },
            "decoding snapshot for installation"
        );
        let raw = serde_json::to_string_pretty(snapshot.get_ref().as_slice())?;
        println!("JSON SNAP:\n{}", raw);
        let new_snapshot: WALStoreSnapshot = serde_json::from_slice(snapshot.get_ref().as_slice())?;
        // Update log.
        {
            // Go backwards through the log to find the most recent membership config <= the `through` index.
            let mut log = self.log.write().await;
            let membership_config = log
                .values()
                .rev()
                .skip_while(|entry| entry.index > index)
                .find_map(|entry| match &entry.payload {
                    EntryPayload::ConfigChange(cfg) => Some(cfg.membership.clone()),
                    _ => None,
                })
                .unwrap_or_else(|| MembershipConfig::new_initial(self.id));

            match &delete_through {
                Some(through) => {
                    *log = log.split_off(&(through + 1));
                }
                None => log.clear(),
            }
            log.insert(
                index,
                Entry::new_snapshot_pointer(index, term, id, membership_config),
            );
        }

        // Update the state machine.
        {
            let new_sm: WALStoreStateMachine = serde_json::from_slice(&new_snapshot.data)?;
            let mut sm = self.sm.write().await;
            *sm = new_sm;
        }

        // Update current snapshot.
        let mut current_snapshot = self.current_snapshot.write().await;
        *current_snapshot = Some(new_snapshot);
        Ok(())
    }

    #[tracing::instrument(level = "trace", skip(self))]
    async fn get_current_snapshot(&self) -> Result<Option<CurrentSnapshotData<Self::Snapshot>>> {
        match &*self.current_snapshot.read().await {
            Some(snapshot) => {
                let reader = serde_json::to_vec(&snapshot)?;
                Ok(Some(CurrentSnapshotData {
                    index: snapshot.index,
                    term: snapshot.term,
                    membership: snapshot.membership.clone(),
                    snapshot: Box::new(Cursor::new(reader)),
                }))
            }
            None => Ok(None),
        }
    }
}

/// A concrete Raft type used during testing.
pub type WALRaft = Raft<ClientRequest, ClientResponse, RaftRouter, WALStore>;

//////////////////////////////////////////////////////////////////////////////////////////////////
//////////////////////////////////////////////////////////////////////////////////////////////////

/// A type which emulates a network transport and implements the `RaftNetwork` trait.
pub struct RaftRouter {
    /// The Raft runtime config which all nodes are using.
    config: Arc<Config>,
    /// The table of all nodes currently known to this router instance.
    routing_table: RwLock<BTreeMap<NodeId, (WALRaft, Arc<WALStore>)>>,
    /// Nodes which are isolated can neither send nor receive frames.
    isolated_nodes: RwLock<HashSet<NodeId>>,
}

impl RaftRouter {
    /// Create a new instance.
    pub fn new(config: Arc<Config>) -> Self {
        Self {
            config,
            routing_table: Default::default(),
            isolated_nodes: Default::default(),
        }
    }

    /// Create and register a new Raft node bearing the given ID.
    pub async fn new_raft_node(self: &Arc<Self>, id: NodeId) {
        let memstore = Arc::new(WALStore::new(id));
        let node = Raft::new(id, self.config.clone(), self.clone(), memstore.clone());
        let mut rt = self.routing_table.write().await;
        rt.insert(id, (node, memstore));
    }

    /// Remove the target node from the routing table & isolation.
    pub async fn remove_node(&self, id: NodeId) -> Option<(WALRaft, Arc<WALStore>)> {
        let mut rt = self.routing_table.write().await;
        let opt_handles = rt.remove(&id);
        let mut isolated = self.isolated_nodes.write().await;
        isolated.remove(&id);

        opt_handles
    }

    /// Initialize all nodes based on the config in the routing table.
    pub async fn initialize_from_single_node(&self, node: NodeId) -> Result<()> {
        tracing::info!({ node }, "initializing cluster from single node");
        let rt = self.routing_table.read().await;
        let members: HashSet<NodeId> = rt.keys().cloned().collect();
        rt.get(&node)
            .ok_or_else(|| anyhow!("node {} not found in routing table", node))?
            .0
            .initialize(members.clone())
            .await?;
        Ok(())
    }

    /// Isolate the network of the specified node.
    #[tracing::instrument(level = "debug", skip(self))]
    pub async fn isolate_node(&self, id: NodeId) {
        self.isolated_nodes.write().await.insert(id);
    }

    /// Get a payload of the latest metrics from each node in the cluster.
    pub async fn latest_metrics(&self) -> Vec<RaftMetrics> {
        let rt = self.routing_table.read().await;
        let mut metrics = vec![];
        for node in rt.values() {
            metrics.push(node.0.metrics().borrow().clone());
        }
        metrics
    }

    /// Get the ID of the current leader.
    pub async fn leader(&self) -> Option<NodeId> {
        let isolated = self.isolated_nodes.read().await;
        self.latest_metrics().await.into_iter().find_map(|node| {
            if node.current_leader == Some(node.id) {
                if isolated.contains(&node.id) {
                    None
                } else {
                    Some(node.id)
                }
            } else {
                None
            }
        })
    }

    /// Restore the network of the specified node.
    #[tracing::instrument(level = "debug", skip(self))]
    pub async fn restore_node(&self, id: NodeId) {
        let mut nodes = self.isolated_nodes.write().await;
        nodes.remove(&id);
    }

    pub async fn add_non_voter(
        &self,
        leader: NodeId,
        target: NodeId,
    ) -> Result<(), ChangeConfigError> {
        let rt = self.routing_table.read().await;
        let node = rt
            .get(&leader)
            .unwrap_or_else(|| panic!("node with ID {} does not exist", leader));
        node.0.add_non_voter(target).await
    }

    pub async fn change_membership(
        &self,
        leader: NodeId,
        members: HashSet<NodeId>,
    ) -> Result<(), ChangeConfigError> {
        let rt = self.routing_table.read().await;
        let node = rt
            .get(&leader)
            .unwrap_or_else(|| panic!("node with ID {} does not exist", leader));
        node.0.change_membership(members).await
    }

    /// Send a client read request to the target node.
    pub async fn client_read(&self, target: NodeId) -> Result<(), ClientReadError> {
        let rt = self.routing_table.read().await;
        let node = rt
            .get(&target)
            .unwrap_or_else(|| panic!("node with ID {} does not exist", target));
        node.0.client_read().await
    }

    /// Send a client request to the target node, causing test failure on error.
    pub async fn client_request(&self, target: NodeId, client_id: &str, serial: u64) {
        let req = ClientRequest {
            client: client_id.into(),
            serial,
            status: format!("request-{}", serial),
        };
        if let Err(err) = self.send_client_request(target, req).await {
            tracing::error!({error=%err}, "error from client request");
            panic!(err)
        }
    }

    /// Request the current leader from the target node.
    pub async fn current_leader(&self, target: NodeId) -> Option<NodeId> {
        let rt = self.routing_table.read().await;
        let node = rt
            .get(&target)
            .unwrap_or_else(|| panic!("node with ID {} does not exist", target));
        node.0.current_leader().await
    }

    /// Send multiple client requests to the target node, causing test failure on error.
    pub async fn client_request_many(&self, target: NodeId, client_id: &str, count: usize) {
        for idx in 0..count {
            self.client_request(target, client_id, idx as u64).await
        }
    }

    async fn send_client_request(
        &self,
        target: NodeId,
        req: ClientRequest,
    ) -> std::result::Result<ClientResponse, ClientWriteError<ClientRequest>> {
        let rt = self.routing_table.read().await;
        let node = rt
            .get(&target)
            .unwrap_or_else(|| panic!("node '{}' does not exist in routing table", target));
        node.0
            .client_write(ClientWriteRequest::new(req))
            .await
            .map(|res| res.data)
    }
}

#[async_trait]
impl RaftNetwork<ClientRequest> for RaftRouter {
    /// Send an AppendEntries RPC to the target Raft node (§5).
    async fn append_entries(
        &self,
        target: u64,
        rpc: AppendEntriesRequest<ClientRequest>,
    ) -> Result<AppendEntriesResponse> {
        let rt = self.routing_table.read().await;
        let isolated = self.isolated_nodes.read().await;
        let addr = rt
            .get(&target)
            .expect("target node not found in routing table");
        if isolated.contains(&target) || isolated.contains(&rpc.leader_id) {
            return Err(anyhow!("target node is isolated"));
        }
        Ok(addr.0.append_entries(rpc).await?)
    }

    /// Send an InstallSnapshot RPC to the target Raft node (§7).
    async fn install_snapshot(
        &self,
        target: u64,
        rpc: InstallSnapshotRequest,
    ) -> Result<InstallSnapshotResponse> {
        let rt = self.routing_table.read().await;
        let isolated = self.isolated_nodes.read().await;
        let addr = rt
            .get(&target)
            .expect("target node not found in routing table");
        if isolated.contains(&target) || isolated.contains(&rpc.leader_id) {
            return Err(anyhow!("target node is isolated"));
        }
        Ok(addr.0.install_snapshot(rpc).await?)
    }

    /// Send a RequestVote RPC to the target Raft node (§5).
    async fn vote(&self, target: u64, rpc: VoteRequest) -> Result<VoteResponse> {
        let rt = self.routing_table.read().await;
        let isolated = self.isolated_nodes.read().await;
        let addr = rt
            .get(&target)
            .expect("target node not found in routing table");
        if isolated.contains(&target) || isolated.contains(&rpc.candidate_id) {
            return Err(anyhow!("target node is isolated"));
        }
        Ok(addr.0.vote(rpc).await?)
    }
}
