mod manager;
mod mdns;
mod udp;

use jasmine_core::{DiscoveryService, PeerInfo};

pub use manager::{DiscoveryManager, DISCOVERY_FALLBACK_DELAY};
pub use mdns::{MdnsDiscovery, MdnsDiscoveryConfig, JASMINE_MDNS_SERVICE_TYPE, MDNS_PEER_TTL};
pub use udp::{
    UdpBroadcastConfig, UdpBroadcastDiscovery, JASMINE_UDP_BROADCAST_ADDR,
    JASMINE_UDP_DISCOVERY_TYPE, UDP_BEACON_INTERVAL, UDP_PEER_TTL,
};

pub struct DiscoveryModule<T: DiscoveryService> {
    service: T,
    bootstrap_peers: Vec<PeerInfo>,
}

impl<T: DiscoveryService> DiscoveryModule<T> {
    pub fn new(service: T) -> Self {
        Self {
            service,
            bootstrap_peers: Vec::new(),
        }
    }

    pub fn service(&self) -> &T {
        &self.service
    }

    pub fn bootstrap_peers(&self) -> &[PeerInfo] {
        &self.bootstrap_peers
    }
}
