use iroh::{Endpoint, PublicKey};
use iroh::endpoint::Connection;
use lru::LruCache;

use std::fs::File;
use std::sync::{Arc, Mutex, RwLock};
use std::collections::{BTreeMap, HashMap, HashSet};

use crate::messages::{VPFSNode,Location,CacheEntry};

#[derive(Debug)]
pub(crate) struct DaemonState {
    pub endpoint: Endpoint,
    pub root: RwLock<Option<VPFSNode>>,
    pub local: VPFSNode,
    pub connections: Mutex<HashMap<String, Arc<Mutex<Connection>>>>, // name of node -> connection
    pub known_hosts: Mutex<Option<HashMap<String, PublicKey>>>,  // name of node -> public key
    pub cache: Mutex<LruCache<Location, CacheEntry>>,
    pub max_cache_size: usize,
    pub used_cache_bytes: RwLock<usize>,
    pub file_access_lock: RwLock<()>,
    pub open_files: Mutex<HashMap<i32, File>>,
}
