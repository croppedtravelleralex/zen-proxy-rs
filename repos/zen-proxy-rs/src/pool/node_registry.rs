use std::collections::HashMap;
use std::sync::RwLock;

use crate::pool::{NodeId, NodeRef};

pub struct NodeRegistry {
    nodes: RwLock<HashMap<NodeId, NodeRef>>,
}

impl NodeRegistry {
    pub fn new() -> Self {
        Self {
            nodes: RwLock::new(HashMap::new()),
        }
    }

    pub fn insert(&self, node: NodeRef) {
        self.nodes.write().unwrap().insert(node.id.clone(), node);
    }

    pub fn get(&self, node_id: &str) -> Option<NodeRef> {
        self.nodes.read().unwrap().get(node_id).cloned()
    }

    pub fn remove(&self, node_id: &str) -> Option<NodeRef> {
        self.nodes.write().unwrap().remove(node_id)
    }

    pub fn ids(&self) -> Vec<NodeId> {
        self.nodes.read().unwrap().keys().cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.nodes.read().unwrap().len()
    }
}

impl Default for NodeRegistry {
    fn default() -> Self {
        Self::new()
    }
}
