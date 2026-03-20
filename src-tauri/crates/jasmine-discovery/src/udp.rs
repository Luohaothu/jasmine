use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use jasmine_core::{
    CoreError, DeviceId, DiscoveryService, PeerDiscoveredCallback, PeerInfo, PeerLostCallback,
    Result,
};
use serde::{Deserialize, Serialize};
use tokio::net::UdpSocket;
use tokio::task::JoinHandle;
use tokio::time::{self, Instant};
use uuid::Uuid;

pub const JASMINE_UDP_BROADCAST_ADDR: &str = "255.255.255.255:53317";
pub const JASMINE_UDP_DISCOVERY_TYPE: &str = "jasmine-announce";
pub const UDP_BEACON_INTERVAL: Duration = Duration::from_secs(5);
pub const UDP_PEER_TTL: Duration = Duration::from_secs(15);
const JASMINE_IPV4_ANY: &str = "0.0.0.0:53317";

#[derive(Debug, Clone)]
pub struct UdpBroadcastConfig {
    pub device_id: DeviceId,
    pub display_name: String,
    pub ws_port: u16,
    pub bind_addr: String,
    pub broadcast_addr: String,
    pub beacon_interval: Duration,
    pub peer_ttl: Duration,
}

impl UdpBroadcastConfig {
    pub fn new(device_id: DeviceId, display_name: impl Into<String>, ws_port: u16) -> Self {
        Self {
            device_id,
            display_name: display_name.into(),
            ws_port,
            bind_addr: JASMINE_IPV4_ANY.to_string(),
            broadcast_addr: JASMINE_UDP_BROADCAST_ADDR.to_string(),
            beacon_interval: UDP_BEACON_INTERVAL,
            peer_ttl: UDP_PEER_TTL,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UdpBeacon {
    #[serde(rename = "type")]
    kind: String,
    device_id: String,
    display_name: String,
    ws_port: u16,
    timestamp: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct UdpTrackedPeer {
    peer: PeerInfo,
    last_seen: Instant,
}

#[derive(Default)]
struct RuntimeState {
    started: bool,
    socket: Option<Arc<UdpSocket>>,
    announce_task: Option<JoinHandle<()>>,
    receive_task: Option<JoinHandle<()>>,
    ttl_task: Option<JoinHandle<()>>,
}

#[derive(Default)]
struct CallbackState {
    discovered: Option<PeerDiscoveredCallback>,
    lost: Option<PeerLostCallback>,
}

pub struct UdpBroadcastDiscovery {
    config: UdpBroadcastConfig,
    callbacks: Arc<Mutex<CallbackState>>,
    peers: Arc<Mutex<HashMap<DeviceId, UdpTrackedPeer>>>,
    runtime: Mutex<RuntimeState>,
}

impl UdpBroadcastDiscovery {
    pub fn new(config: UdpBroadcastConfig) -> Self {
        Self {
            config,
            callbacks: Arc::new(Mutex::new(CallbackState::default())),
            peers: Arc::new(Mutex::new(HashMap::new())),
            runtime: Mutex::new(RuntimeState::default()),
        }
    }

    pub fn peers(&self) -> Vec<PeerInfo> {
        self.peers
            .lock()
            .expect("lock udp peers")
            .values()
            .map(|tracked| tracked.peer.clone())
            .collect()
    }
}

impl DiscoveryService for UdpBroadcastDiscovery {
    async fn start(&self) -> Result<()> {
        {
            let runtime = self.runtime.lock().expect("lock udp runtime state");
            if runtime.started {
                return Ok(());
            }
        }

        let socket = Arc::new(
            UdpSocket::bind(self.config.bind_addr.as_str())
                .await
                .map_err(|error| CoreError::Validation(error.to_string()))?,
        );
        socket
            .set_broadcast(true)
            .map_err(|error| CoreError::Validation(error.to_string()))?;

        let announce_socket = socket.clone();
        let receive_socket = socket.clone();
        let peers_for_ttl = self.peers.clone();
        let callbacks_for_ttl = self.callbacks.clone();
        let peer_ttl = self.config.peer_ttl;
        let callbacks_for_receive = self.callbacks.clone();
        let peers_for_receive = self.peers.clone();
        let local_device = self.config.device_id.clone();
        let beacon_interval = self.config.beacon_interval;

        let announce_task = spawn_announce_task(
            announce_socket,
            self.config.broadcast_addr.clone(),
            beacon_interval,
            self.config.ws_port,
            self.config.device_id.clone(),
            self.config.display_name.clone(),
        );

        let receive_task = spawn_receive_task(
            receive_socket,
            local_device,
            peers_for_receive,
            callbacks_for_receive,
        );

        let ttl_task = spawn_ttl_task(peers_for_ttl, callbacks_for_ttl, peer_ttl);

        let mut runtime = self.runtime.lock().expect("lock udp runtime state");
        runtime.started = true;
        runtime.socket = Some(socket);
        runtime.announce_task = Some(announce_task);
        runtime.receive_task = Some(receive_task);
        runtime.ttl_task = Some(ttl_task);

        Ok(())
    }

    async fn stop(&self) -> Result<()> {
        let (announce_task, receive_task, ttl_task, socket) = {
            let mut runtime = self.runtime.lock().expect("lock udp runtime state");
            if !runtime.started {
                return Ok(());
            }

            runtime.started = false;
            (
                runtime.announce_task.take(),
                runtime.receive_task.take(),
                runtime.ttl_task.take(),
                runtime.socket.take(),
            )
        };

        if let Some(task) = announce_task {
            task.abort();
            let _ = task.await;
        }

        if let Some(task) = receive_task {
            task.abort();
            let _ = task.await;
        }

        if let Some(task) = ttl_task {
            task.abort();
            let _ = task.await;
        }

        drop(socket);
        self.peers.lock().expect("lock udp peers").clear();

        Ok(())
    }

    fn on_peer_discovered(&mut self, callback: PeerDiscoveredCallback) {
        self.callbacks
            .lock()
            .expect("lock udp callbacks")
            .discovered = Some(callback);
    }

    fn on_peer_lost(&mut self, callback: PeerLostCallback) {
        self.callbacks.lock().expect("lock udp callbacks").lost = Some(callback);
    }
}

fn spawn_announce_task(
    socket: Arc<UdpSocket>,
    broadcast_addr: String,
    interval: Duration,
    ws_port: u16,
    device_id: DeviceId,
    display_name: String,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = time::interval(interval);
        loop {
            if let Ok(payload) = build_beacon_payload(&device_id, &display_name, ws_port) {
                let _ = socket.send_to(payload.as_bytes(), &broadcast_addr).await;
            }

            let _ = ticker.tick().await;
        }
    })
}

fn build_beacon_payload(device_id: &DeviceId, display_name: &str, ws_port: u16) -> Result<String> {
    let beacon = UdpBeacon {
        kind: JASMINE_UDP_DISCOVERY_TYPE.to_string(),
        device_id: device_id.0.to_string(),
        display_name: display_name.to_string(),
        ws_port,
        timestamp: current_timestamp_ms(),
    };
    serde_json::to_string(&beacon).map_err(CoreError::from)
}

fn spawn_receive_task(
    socket: Arc<UdpSocket>,
    local_device_id: DeviceId,
    peers: Arc<Mutex<HashMap<DeviceId, UdpTrackedPeer>>>,
    callbacks: Arc<Mutex<CallbackState>>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut buffer = vec![0_u8; 2048];
        loop {
            let (size, source) = match socket.recv_from(&mut buffer).await {
                Ok((size, source)) => (size, source),
                Err(_) => continue,
            };
            let _ = apply_beacon(
                source,
                &buffer[..size],
                Instant::now(),
                &local_device_id,
                peers.clone(),
                callbacks.clone(),
            );
        }
    })
}

fn apply_beacon(
    source: SocketAddr,
    payload: &[u8],
    now: Instant,
    local_device_id: &DeviceId,
    peers: Arc<Mutex<HashMap<DeviceId, UdpTrackedPeer>>>,
    callbacks: Arc<Mutex<CallbackState>>,
) -> Option<(PeerInfo, DeviceId)> {
    let parsed = parse_beacon(payload)?;
    if parsed.kind != JASMINE_UDP_DISCOVERY_TYPE {
        return None;
    }

    let device_id = DeviceId(Uuid::parse_str(&parsed.device_id).ok()?);
    if device_id == *local_device_id {
        return None;
    }

    let peer = PeerInfo {
        device_id: device_id.clone(),
        user_id: None,
        display_name: parsed.display_name,
        ws_port: Some(parsed.ws_port),
        addresses: vec![source.ip().to_string()],
    };

    let mut emitted = None;
    {
        let mut guard = peers.lock().expect("lock udp peers");
        let should_emit = match guard.get_mut(&device_id) {
            Some(existing) => {
                let changed = existing.peer != peer;
                existing.peer = peer.clone();
                existing.last_seen = now;
                changed
            }
            None => {
                guard.insert(
                    device_id.clone(),
                    UdpTrackedPeer {
                        peer: peer.clone(),
                        last_seen: now,
                    },
                );
                true
            }
        };

        if should_emit {
            emitted = Some(peer.clone());
        }
    }

    if emitted.is_some() {
        emit_discovered(&callbacks, peer.clone());
    }

    Some((peer, device_id))
}

fn spawn_ttl_task(
    peers: Arc<Mutex<HashMap<DeviceId, UdpTrackedPeer>>>,
    callbacks: Arc<Mutex<CallbackState>>,
    ttl: Duration,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = time::interval(Duration::from_secs(1));
        loop {
            let _ = ticker.tick().await;
            let now = Instant::now();
            let expired = collect_expired_peers(&peers, ttl, now);
            for device_id in expired {
                emit_lost(&callbacks, device_id);
            }
        }
    })
}

fn emit_discovered(callbacks: &Arc<Mutex<CallbackState>>, peer: PeerInfo) {
    let callback = callbacks
        .lock()
        .expect("lock udp callbacks")
        .discovered
        .clone();
    if let Some(callback) = callback {
        callback(peer);
    }
}

fn emit_lost(callbacks: &Arc<Mutex<CallbackState>>, device_id: DeviceId) {
    let callback = callbacks.lock().expect("lock udp callbacks").lost.clone();
    if let Some(callback) = callback {
        callback(device_id);
    }
}

fn parse_beacon(payload: &[u8]) -> Option<UdpBeacon> {
    serde_json::from_slice::<UdpBeacon>(payload).ok()
}

fn collect_expired_peers(
    peers: &Arc<Mutex<HashMap<DeviceId, UdpTrackedPeer>>>,
    ttl: Duration,
    now: Instant,
) -> Vec<DeviceId> {
    let mut peers = peers.lock().expect("lock udp peers");
    let expired: Vec<DeviceId> = peers
        .iter()
        .filter_map(|(device_id, tracked)| {
            (now.duration_since(tracked.last_seen) >= ttl).then_some(device_id.clone())
        })
        .collect();

    for device_id in &expired {
        peers.remove(device_id);
    }

    expired
}

fn current_timestamp_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(since_epoch) => i64::try_from(since_epoch.as_millis()).unwrap_or(i64::MAX),
        Err(_) => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jasmine_core::DeviceId;
    use serde_json::Value;
    use std::net::IpAddr;
    use std::net::Ipv4Addr;
    use std::time::Duration;
    use tokio::net::UdpSocket;
    use tokio::time;
    use uuid::Uuid;

    fn id(seed: u128) -> DeviceId {
        DeviceId(Uuid::from_u128(seed))
    }

    fn config(seed: u128) -> UdpBroadcastConfig {
        UdpBroadcastConfig::new(id(seed), format!("Device-{seed}"), 9735)
    }

    #[test]
    fn udp_beacon_serialization_shape_contains_type_display_and_timestamp() {
        let discovery = UdpBroadcastDiscovery::new(config(1));
        let payload = build_beacon_payload(
            &discovery.config.device_id,
            &discovery.config.display_name,
            discovery.config.ws_port,
        )
        .expect("build beacon");
        let json: Value = serde_json::from_str(&payload).expect("parse json");

        assert_eq!(json["type"], serde_json::json!(JASMINE_UDP_DISCOVERY_TYPE));
        assert_eq!(json["device_id"], serde_json::json!(id(1).0.to_string()));
        assert_eq!(json["display_name"], serde_json::json!("Device-1"));
        assert_eq!(json["ws_port"], serde_json::json!(9735));
        assert!(json["timestamp"].as_i64().is_some());
    }

    #[tokio::test]
    async fn udp_beacon_send_and_receive_loopback() {
        let sender = UdpSocket::bind("127.0.0.1:0").await.expect("bind sender");
        let receiver = UdpSocket::bind("127.0.0.1:0").await.expect("bind receiver");
        let discovery = UdpBroadcastDiscovery::new(config(15));
        let payload = build_beacon_payload(
            &discovery.config.device_id,
            &discovery.config.display_name,
            discovery.config.ws_port,
        )
        .expect("serialize beacon");

        sender
            .send_to(
                payload.as_bytes(),
                receiver.local_addr().expect("receiver addr"),
            )
            .await
            .expect("send loopback");

        let mut buffer = vec![0_u8; 2048];
        let (size, source) = receiver
            .recv_from(&mut buffer)
            .await
            .expect("recv loopback");
        let value = serde_json::from_slice::<Value>(&buffer[..size]).expect("decode payload");
        assert_eq!(value["type"], serde_json::json!(JASMINE_UDP_DISCOVERY_TYPE));
        assert_eq!(
            value["device_id"],
            serde_json::json!(discovery.config.device_id.0.to_string())
        );
        assert_eq!(source.ip(), IpAddr::V4(Ipv4Addr::LOCALHOST));
    }

    #[test]
    fn udp_beacon_parse_and_self_filtering() {
        let local = id(2);
        let discovery = UdpBroadcastDiscovery::new(config(2));
        let beacon = UdpBeacon {
            kind: JASMINE_UDP_DISCOVERY_TYPE.to_string(),
            device_id: local.0.to_string(),
            display_name: "Self Device".to_string(),
            ws_port: 9735,
            timestamp: 10_000,
        };

        let payload = serde_json::to_vec(&beacon).expect("serialize beacon");
        let beacon_update = apply_beacon(
            SocketAddr::from((Ipv4Addr::LOCALHOST, 53317)),
            &payload,
            Instant::now(),
            &local,
            discovery.peers.clone(),
            discovery.callbacks.clone(),
        )
        .map(|(peer, _)| peer);

        assert!(beacon_update.is_none());
        assert!(discovery.peers().is_empty());
    }

    #[test]
    fn udp_peer_dedup_by_device_id_holds_single_entry() {
        let discovery = UdpBroadcastDiscovery::new(config(3));
        let peer_id = id(9);
        let beacon = UdpBeacon {
            kind: JASMINE_UDP_DISCOVERY_TYPE.to_string(),
            device_id: peer_id.0.to_string(),
            display_name: "Peer A".to_string(),
            ws_port: 9735,
            timestamp: 10,
        };

        let payload = serde_json::to_vec(&beacon).expect("serialize beacon");
        let now = Instant::now();
        let _ = apply_beacon(
            SocketAddr::from((Ipv4Addr::LOCALHOST, 53317)),
            &payload,
            now,
            &discovery.config.device_id,
            discovery.peers.clone(),
            discovery.callbacks.clone(),
        );
        assert_eq!(discovery.peers().len(), 1);

        let mut duplicate = beacon.clone();
        duplicate.ws_port = 9750;
        let duplicate_payload = serde_json::to_vec(&duplicate).expect("serialize duplicate");
        let _ = apply_beacon(
            SocketAddr::from((Ipv4Addr::LOCALHOST, 53317)),
            &duplicate_payload,
            now,
            &discovery.config.device_id,
            discovery.peers.clone(),
            discovery.callbacks.clone(),
        );

        assert_eq!(discovery.peers().len(), 1);
        assert_eq!(discovery.peers()[0].ws_port, Some(9750));
    }

    #[tokio::test(start_paused = true)]
    async fn udp_stale_peer_cleanup_after_15_seconds() {
        let discovery = UdpBroadcastDiscovery::new(config(4));
        let peer_id = id(10);
        let source = SocketAddr::from((Ipv4Addr::LOCALHOST, 53317));
        let payload = serde_json::to_vec(&UdpBeacon {
            kind: JASMINE_UDP_DISCOVERY_TYPE.to_string(),
            device_id: peer_id.0.to_string(),
            display_name: "Peer B".to_string(),
            ws_port: 9735,
            timestamp: 0,
        })
        .expect("serialize beacon");

        let _ = apply_beacon(
            source,
            &payload,
            Instant::now(),
            &discovery.config.device_id,
            discovery.peers.clone(),
            discovery.callbacks.clone(),
        );
        assert_eq!(discovery.peers().len(), 1);

        let stale =
            collect_expired_peers(&discovery.peers, Duration::from_secs(15), Instant::now());
        assert_eq!(stale.len(), 0);

        time::advance(Duration::from_secs(14)).await;
        tokio::task::yield_now().await;
        let stale =
            collect_expired_peers(&discovery.peers, Duration::from_secs(15), Instant::now());
        assert_eq!(stale.len(), 0);

        time::advance(Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
        let stale =
            collect_expired_peers(&discovery.peers, Duration::from_secs(15), Instant::now());
        assert_eq!(stale, vec![peer_id]);
        assert_eq!(discovery.peers().len(), 0);
    }
}
