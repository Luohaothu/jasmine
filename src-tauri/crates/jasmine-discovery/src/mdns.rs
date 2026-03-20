use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use jasmine_core::{
    CoreError, DeviceId, DiscoveryService, PeerDiscoveredCallback, PeerInfo, PeerLostCallback,
    Result,
};
use mdns_sd::{ResolvedService, ServiceDaemon, ServiceEvent, ServiceInfo};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{self, Instant};
use uuid::Uuid;

pub const JASMINE_MDNS_SERVICE_TYPE: &str = "_jasmine._tcp.local";
pub const MDNS_PEER_TTL: Duration = Duration::from_secs(30);

const CANONICAL_MDNS_SERVICE_TYPE: &str = "_jasmine._tcp.local.";
const IP_CHECK_INTERVAL_SECS: u32 = 5;
const TTL_SWEEP_INTERVAL: Duration = Duration::from_secs(1);
const TXT_DEVICE_ID: &str = "device_id";
const TXT_DISPLAY_NAME: &str = "display_name";
const TXT_WS_PORT: &str = "ws_port";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MdnsDiscoveryConfig {
    pub device_id: DeviceId,
    pub display_name: String,
    pub ws_port: u16,
}

impl MdnsDiscoveryConfig {
    fn registration(&self) -> MdnsServiceRegistration {
        let mut txt_records = BTreeMap::new();
        txt_records.insert(TXT_DEVICE_ID.to_string(), self.device_id.0.to_string());
        txt_records.insert(TXT_DISPLAY_NAME.to_string(), self.display_name.clone());
        txt_records.insert(TXT_WS_PORT.to_string(), self.ws_port.to_string());

        let instance_suffix = self.device_id.0.simple().to_string();
        let instance_name = format!("jasmine-{}", &instance_suffix[..6]);

        MdnsServiceRegistration {
            service_type: JASMINE_MDNS_SERVICE_TYPE.to_string(),
            instance_name,
            host_name: format!("jasmine-{}.local.", self.device_id.0),
            port: self.ws_port,
            txt_records,
        }
    }
}

pub struct MdnsDiscovery {
    backend: Arc<dyn MdnsBackend>,
    config: MdnsDiscoveryConfig,
    callbacks: Arc<Mutex<CallbackState>>,
    peers: Arc<Mutex<HashMap<DeviceId, TrackedPeer>>>,
    runtime: Mutex<RuntimeState>,
}

impl MdnsDiscovery {
    pub fn new(config: MdnsDiscoveryConfig) -> Result<Self> {
        let backend = Arc::new(MdnsSdBackend::new()?);
        Ok(Self::with_backend(config, backend))
    }

    fn with_backend(config: MdnsDiscoveryConfig, backend: Arc<dyn MdnsBackend>) -> Self {
        Self {
            backend,
            config,
            callbacks: Arc::new(Mutex::new(CallbackState::default())),
            peers: Arc::new(Mutex::new(HashMap::new())),
            runtime: Mutex::new(RuntimeState::default()),
        }
    }
}

impl DiscoveryService for MdnsDiscovery {
    async fn start(&self) -> Result<()> {
        {
            let runtime = self.runtime.lock().expect("lock mdns runtime state");
            if runtime.started {
                return Ok(());
            }
        }

        self.backend.set_ip_check_interval(IP_CHECK_INTERVAL_SECS)?;

        let registration = self.config.registration();
        let service_fullname = self.backend.register(registration)?;
        let event_rx = match self.backend.browse(JASMINE_MDNS_SERVICE_TYPE) {
            Ok(receiver) => receiver,
            Err(error) => {
                let _ = self.backend.unregister(&service_fullname);
                return Err(error);
            }
        };

        let browse_task = spawn_browse_task(
            self.config.device_id.clone(),
            Arc::clone(&self.peers),
            Arc::clone(&self.callbacks),
            event_rx,
        );
        let ttl_task = spawn_ttl_task(Arc::clone(&self.peers), Arc::clone(&self.callbacks));

        let mut runtime = self.runtime.lock().expect("lock mdns runtime state");
        runtime.started = true;
        runtime.service_fullname = Some(service_fullname);
        runtime.browse_task = Some(browse_task);
        runtime.ttl_task = Some(ttl_task);

        Ok(())
    }

    async fn stop(&self) -> Result<()> {
        let (browse_task, ttl_task, service_fullname) = {
            let mut runtime = self.runtime.lock().expect("lock mdns runtime state");
            if !runtime.started {
                return Ok(());
            }

            runtime.started = false;
            (
                runtime.browse_task.take(),
                runtime.ttl_task.take(),
                runtime.service_fullname.take(),
            )
        };

        if let Some(task) = browse_task {
            task.abort();
            let _ = task.await;
        }

        if let Some(task) = ttl_task {
            task.abort();
            let _ = task.await;
        }

        self.backend.stop_browse(JASMINE_MDNS_SERVICE_TYPE)?;

        if let Some(service_fullname) = service_fullname {
            self.backend.unregister(&service_fullname)?;
        }

        self.backend.shutdown()?;
        self.peers.lock().expect("lock mdns peers").clear();

        Ok(())
    }

    fn on_peer_discovered(&mut self, callback: PeerDiscoveredCallback) {
        self.callbacks
            .lock()
            .expect("lock mdns callbacks")
            .discovered = Some(callback);
    }

    fn on_peer_lost(&mut self, callback: PeerLostCallback) {
        self.callbacks.lock().expect("lock mdns callbacks").lost = Some(callback);
    }
}

#[derive(Default)]
struct CallbackState {
    discovered: Option<PeerDiscoveredCallback>,
    lost: Option<PeerLostCallback>,
}

#[derive(Default)]
struct RuntimeState {
    started: bool,
    service_fullname: Option<String>,
    browse_task: Option<JoinHandle<()>>,
    ttl_task: Option<JoinHandle<()>>,
}

#[derive(Debug, Clone)]
struct TrackedPeer {
    fullname: String,
    peer: PeerInfo,
    last_seen: Instant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MdnsServiceRegistration {
    service_type: String,
    instance_name: String,
    host_name: String,
    port: u16,
    txt_records: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum MdnsEvent {
    SearchStarted(String),
    SearchStopped(String),
    ServiceResolved(MdnsResolvedService),
    ServiceRemoved {
        service_type: String,
        fullname: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MdnsResolvedService {
    service_type: String,
    fullname: String,
    host_name: String,
    port: u16,
    addresses: Vec<String>,
    txt_records: BTreeMap<String, String>,
}

trait MdnsBackend: Send + Sync {
    fn set_ip_check_interval(&self, interval_secs: u32) -> Result<()>;
    fn register(&self, registration: MdnsServiceRegistration) -> Result<String>;
    fn browse(&self, service_type: &str) -> Result<mpsc::UnboundedReceiver<MdnsEvent>>;
    fn stop_browse(&self, service_type: &str) -> Result<()>;
    fn unregister(&self, fullname: &str) -> Result<()>;
    fn shutdown(&self) -> Result<()>;
}

struct MdnsSdBackend {
    daemon: ServiceDaemon,
}

impl MdnsSdBackend {
    fn new() -> Result<Self> {
        let daemon =
            ServiceDaemon::new().map_err(|error| mdns_error("create mdns daemon", error))?;
        Ok(Self { daemon })
    }
}

impl MdnsBackend for MdnsSdBackend {
    fn set_ip_check_interval(&self, interval_secs: u32) -> Result<()> {
        self.daemon
            .set_ip_check_interval(interval_secs)
            .map_err(|error| mdns_error("configure mdns ip check interval", error))
    }

    fn register(&self, registration: MdnsServiceRegistration) -> Result<String> {
        let service_type = canonical_service_type(&registration.service_type);
        let device_id = registration
            .txt_records
            .get(TXT_DEVICE_ID)
            .map(String::as_str)
            .unwrap_or_default();
        let display_name = registration
            .txt_records
            .get(TXT_DISPLAY_NAME)
            .map(String::as_str)
            .unwrap_or_default();
        let ws_port = registration
            .txt_records
            .get(TXT_WS_PORT)
            .map(String::as_str)
            .unwrap_or_default();
        let properties = [
            (TXT_DEVICE_ID, device_id),
            (TXT_DISPLAY_NAME, display_name),
            (TXT_WS_PORT, ws_port),
        ];

        let service_info = ServiceInfo::new(
            &service_type,
            &registration.instance_name,
            &registration.host_name,
            "",
            registration.port,
            &properties[..],
        )
        .map_err(|error| mdns_error("build mdns service info", error))?
        .enable_addr_auto();

        let service_fullname = service_info.get_fullname().to_string();
        self.daemon
            .register(service_info)
            .map_err(|error| mdns_error("register mdns service", error))?;

        Ok(service_fullname)
    }

    fn browse(&self, service_type: &str) -> Result<mpsc::UnboundedReceiver<MdnsEvent>> {
        let receiver = self
            .daemon
            .browse(&canonical_service_type(service_type))
            .map_err(|error| mdns_error("start mdns browse", error))?;
        let (event_tx, event_rx) = mpsc::unbounded_channel();

        tokio::spawn(async move {
            while let Ok(event) = receiver.recv_async().await {
                if let Some(mapped_event) = map_service_event(event) {
                    if event_tx.send(mapped_event).is_err() {
                        break;
                    }
                }
            }
        });

        Ok(event_rx)
    }

    fn stop_browse(&self, service_type: &str) -> Result<()> {
        self.daemon
            .stop_browse(&canonical_service_type(service_type))
            .map_err(|error| mdns_error("stop mdns browse", error))
    }

    fn unregister(&self, fullname: &str) -> Result<()> {
        self.daemon
            .unregister(fullname)
            .map_err(|error| mdns_error("unregister mdns service", error))?;
        Ok(())
    }

    fn shutdown(&self) -> Result<()> {
        self.daemon
            .shutdown()
            .map_err(|error| mdns_error("shutdown mdns daemon", error))?;
        Ok(())
    }
}

fn spawn_browse_task(
    local_device_id: DeviceId,
    peers: Arc<Mutex<HashMap<DeviceId, TrackedPeer>>>,
    callbacks: Arc<Mutex<CallbackState>>,
    mut event_rx: mpsc::UnboundedReceiver<MdnsEvent>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(event) = event_rx.recv().await {
            match event {
                MdnsEvent::ServiceResolved(resolved) => {
                    handle_resolved_event(&local_device_id, &peers, &callbacks, resolved)
                }
                MdnsEvent::ServiceRemoved { fullname, .. } => {
                    handle_removed_event(&peers, &callbacks, &fullname)
                }
                MdnsEvent::SearchStarted(_) | MdnsEvent::SearchStopped(_) => {}
            }
        }
    })
}

fn spawn_ttl_task(
    peers: Arc<Mutex<HashMap<DeviceId, TrackedPeer>>>,
    callbacks: Arc<Mutex<CallbackState>>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = time::interval(TTL_SWEEP_INTERVAL);
        loop {
            interval.tick().await;
            let expired = collect_expired_peers(&peers);
            for device_id in expired {
                emit_lost(&callbacks, device_id);
            }
        }
    })
}

fn handle_resolved_event(
    local_device_id: &DeviceId,
    peers: &Arc<Mutex<HashMap<DeviceId, TrackedPeer>>>,
    callbacks: &Arc<Mutex<CallbackState>>,
    resolved: MdnsResolvedService,
) {
    let Some(peer) = peer_from_resolved(local_device_id, &resolved) else {
        return;
    };

    let device_id = peer.device_id.clone();
    let mut discovered_event = None;
    let mut peers_guard = peers.lock().expect("lock mdns peers");

    match peers_guard.get_mut(&device_id) {
        Some(tracked) => {
            let changed = tracked.peer != peer || tracked.fullname != resolved.fullname;
            tracked.peer = peer.clone();
            tracked.fullname = resolved.fullname;
            tracked.last_seen = Instant::now();
            if changed {
                discovered_event = Some(peer);
            }
        }
        None => {
            peers_guard.insert(
                device_id,
                TrackedPeer {
                    fullname: resolved.fullname,
                    peer: peer.clone(),
                    last_seen: Instant::now(),
                },
            );
            discovered_event = Some(peer);
        }
    }

    drop(peers_guard);

    if let Some(peer) = discovered_event {
        emit_discovered(callbacks, peer);
    }
}

fn handle_removed_event(
    peers: &Arc<Mutex<HashMap<DeviceId, TrackedPeer>>>,
    callbacks: &Arc<Mutex<CallbackState>>,
    fullname: &str,
) {
    let removed = {
        let mut peers_guard = peers.lock().expect("lock mdns peers");
        let maybe_device_id = peers_guard.iter().find_map(|(device_id, tracked)| {
            (tracked.fullname == fullname).then(|| device_id.clone())
        });

        maybe_device_id.and_then(|device_id| peers_guard.remove(&device_id).map(|_| device_id))
    };

    if let Some(device_id) = removed {
        emit_lost(callbacks, device_id);
    }
}

fn collect_expired_peers(peers: &Arc<Mutex<HashMap<DeviceId, TrackedPeer>>>) -> Vec<DeviceId> {
    let now = Instant::now();
    let mut peers_guard = peers.lock().expect("lock mdns peers");
    let expired: Vec<DeviceId> = peers_guard
        .iter()
        .filter(|(_, tracked)| now.duration_since(tracked.last_seen) >= MDNS_PEER_TTL)
        .map(|(device_id, _)| device_id.clone())
        .collect();

    for device_id in &expired {
        peers_guard.remove(device_id);
    }

    expired
}

fn peer_from_resolved(
    local_device_id: &DeviceId,
    resolved: &MdnsResolvedService,
) -> Option<PeerInfo> {
    let device_id = resolved.txt_records.get(TXT_DEVICE_ID)?;
    let parsed_device_id = DeviceId(Uuid::parse_str(device_id).ok()?);
    if &parsed_device_id == local_device_id {
        return None;
    }

    let display_name = resolved
        .txt_records
        .get(TXT_DISPLAY_NAME)
        .cloned()
        .filter(|name| !name.trim().is_empty())?;

    let ws_port = resolved
        .txt_records
        .get(TXT_WS_PORT)
        .and_then(|value| value.parse::<u16>().ok())
        .or(Some(resolved.port));

    Some(PeerInfo {
        device_id: parsed_device_id,
        user_id: None,
        display_name,
        ws_port,
        addresses: normalize_addresses(resolved.addresses.clone()),
    })
}

fn normalize_addresses(mut addresses: Vec<String>) -> Vec<String> {
    addresses.retain(|address| !address.trim().is_empty());
    addresses.sort();
    addresses.dedup();
    addresses
}

fn emit_discovered(callbacks: &Arc<Mutex<CallbackState>>, peer: PeerInfo) {
    let callback = callbacks
        .lock()
        .expect("lock mdns callbacks")
        .discovered
        .clone();

    if let Some(callback) = callback {
        callback(peer);
    }
}

fn emit_lost(callbacks: &Arc<Mutex<CallbackState>>, device_id: DeviceId) {
    let callback = callbacks.lock().expect("lock mdns callbacks").lost.clone();

    if let Some(callback) = callback {
        callback(device_id);
    }
}

fn map_service_event(event: ServiceEvent) -> Option<MdnsEvent> {
    match event {
        ServiceEvent::SearchStarted(service_type) => Some(MdnsEvent::SearchStarted(
            normalize_service_type(&service_type),
        )),
        ServiceEvent::SearchStopped(service_type) => Some(MdnsEvent::SearchStopped(
            normalize_service_type(&service_type),
        )),
        ServiceEvent::ServiceResolved(resolved) => Some(MdnsEvent::ServiceResolved(
            resolved_service_from_mdns(resolved.as_ref()),
        )),
        ServiceEvent::ServiceRemoved(service_type, fullname) => Some(MdnsEvent::ServiceRemoved {
            service_type: normalize_service_type(&service_type),
            fullname,
        }),
        ServiceEvent::ServiceFound(_, _) => None,
        _ => None,
    }
}

fn resolved_service_from_mdns(resolved: &ResolvedService) -> MdnsResolvedService {
    let mut txt_records = BTreeMap::new();

    if let Some(value) = resolved.get_property_val_str(TXT_DEVICE_ID) {
        txt_records.insert(TXT_DEVICE_ID.to_string(), value.to_string());
    }
    if let Some(value) = resolved.get_property_val_str(TXT_DISPLAY_NAME) {
        txt_records.insert(TXT_DISPLAY_NAME.to_string(), value.to_string());
    }
    if let Some(value) = resolved.get_property_val_str(TXT_WS_PORT) {
        txt_records.insert(TXT_WS_PORT.to_string(), value.to_string());
    }

    MdnsResolvedService {
        service_type: normalize_service_type(&resolved.ty_domain),
        fullname: resolved.fullname.clone(),
        host_name: resolved.host.clone(),
        port: resolved.port,
        addresses: resolved
            .get_addresses()
            .iter()
            .map(ToString::to_string)
            .collect(),
        txt_records,
    }
}

fn canonical_service_type(service_type: &str) -> String {
    if service_type.ends_with('.') {
        service_type.to_string()
    } else if service_type == JASMINE_MDNS_SERVICE_TYPE {
        CANONICAL_MDNS_SERVICE_TYPE.to_string()
    } else {
        format!("{service_type}.")
    }
}

fn normalize_service_type(service_type: &str) -> String {
    service_type.trim_end_matches('.').to_string()
}

fn mdns_error(context: &str, error: impl std::fmt::Display) -> CoreError {
    CoreError::Validation(format!("{context}: {error}"))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use jasmine_core::{DeviceId, DiscoveryService, PeerInfo};
    use tokio::sync::mpsc;
    use tokio::time;
    use uuid::Uuid;

    use super::{
        MdnsBackend, MdnsDiscovery, MdnsDiscoveryConfig, MdnsEvent, MdnsResolvedService,
        MdnsServiceRegistration, JASMINE_MDNS_SERVICE_TYPE,
    };

    #[tokio::test]
    async fn mdns_register_discover_registers_service_and_emits_peer_callback() {
        let backend = FakeMdnsBackend::new();
        let discovered = Arc::new(Mutex::new(Vec::<PeerInfo>::new()));
        let mut discovery = MdnsDiscovery::with_backend(
            config(device_id(1), "Local Device", 9735),
            backend.clone(),
        );

        let discovered_sink = discovered.clone();
        discovery.on_peer_discovered(Arc::new(move |peer| {
            discovered_sink
                .lock()
                .expect("lock discovered peers")
                .push(peer);
        }));

        discovery.start().await.expect("start discovery");

        let registration = backend.last_registration().expect("registration recorded");
        assert_eq!(registration.service_type, JASMINE_MDNS_SERVICE_TYPE);
        assert_eq!(
            registration.txt_records.get("device_id"),
            Some(&device_id(1).0.to_string())
        );
        assert_eq!(
            registration.txt_records.get("display_name"),
            Some(&"Local Device".to_string())
        );
        assert_eq!(
            registration.txt_records.get("ws_port"),
            Some(&"9735".to_string())
        );

        backend.send(MdnsEvent::ServiceResolved(resolved_peer(
            device_id(2),
            "Peer Alpha",
            9844,
            vec!["192.168.1.50".to_string()],
        )));
        settle().await;

        let peers = discovered.lock().expect("lock discovered peers").clone();
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].device_id, device_id(2));
        assert_eq!(peers[0].display_name, "Peer Alpha");
        assert_eq!(peers[0].ws_port, Some(9844));
        assert_eq!(peers[0].addresses, vec!["192.168.1.50".to_string()]);

        discovery.stop().await.expect("stop discovery");
    }

    #[tokio::test]
    async fn mdns_self_filtering_ignores_local_device_announcements() {
        let backend = FakeMdnsBackend::new();
        let discovered = Arc::new(Mutex::new(Vec::<PeerInfo>::new()));
        let local_device = device_id(7);
        let mut discovery = MdnsDiscovery::with_backend(
            config(local_device.clone(), "Local Device", 9735),
            backend.clone(),
        );

        let discovered_sink = discovered.clone();
        discovery.on_peer_discovered(Arc::new(move |peer| {
            discovered_sink
                .lock()
                .expect("lock discovered peers")
                .push(peer);
        }));

        discovery.start().await.expect("start discovery");
        backend.send(MdnsEvent::ServiceResolved(resolved_peer(
            local_device,
            "Local Device",
            9735,
            vec!["127.0.0.1".to_string()],
        )));
        settle().await;

        assert!(discovered.lock().expect("lock discovered peers").is_empty());

        discovery.stop().await.expect("stop discovery");
    }

    #[tokio::test(start_paused = true)]
    async fn mdns_ttl_expiry_marks_peer_lost_after_30_seconds() {
        let backend = FakeMdnsBackend::new();
        let lost = Arc::new(Mutex::new(Vec::<DeviceId>::new()));
        let mut discovery = MdnsDiscovery::with_backend(
            config(device_id(10), "Local Device", 9735),
            backend.clone(),
        );

        let lost_sink = lost.clone();
        discovery.on_peer_lost(Arc::new(move |device_id| {
            lost_sink.lock().expect("lock lost peers").push(device_id);
        }));

        discovery.start().await.expect("start discovery");
        backend.send(MdnsEvent::ServiceResolved(resolved_peer(
            device_id(11),
            "Peer Beta",
            9736,
            vec!["192.168.1.51".to_string()],
        )));
        settle().await;

        time::advance(Duration::from_secs(29)).await;
        settle().await;
        assert!(lost.lock().expect("lock lost peers").is_empty());

        time::advance(Duration::from_secs(1)).await;
        settle().await;
        assert_eq!(
            lost.lock().expect("lock lost peers").clone(),
            vec![device_id(11)]
        );

        discovery.stop().await.expect("stop discovery");
    }

    #[tokio::test]
    async fn mdns_service_removed_emits_peer_lost_callback_immediately() {
        let backend = FakeMdnsBackend::new();
        let lost = Arc::new(Mutex::new(Vec::<DeviceId>::new()));
        let mut discovery = MdnsDiscovery::with_backend(
            config(device_id(20), "Local Device", 9735),
            backend.clone(),
        );

        let lost_sink = lost.clone();
        discovery.on_peer_lost(Arc::new(move |device_id| {
            lost_sink.lock().expect("lock lost peers").push(device_id);
        }));

        discovery.start().await.expect("start discovery");
        let resolved = resolved_peer(
            device_id(21),
            "Peer Gamma",
            9737,
            vec!["192.168.1.52".to_string()],
        );
        let fullname = resolved.fullname.clone();
        backend.send(MdnsEvent::ServiceResolved(resolved));
        settle().await;

        backend.send(MdnsEvent::ServiceRemoved {
            service_type: JASMINE_MDNS_SERVICE_TYPE.to_string(),
            fullname,
        });
        settle().await;

        assert_eq!(
            lost.lock().expect("lock lost peers").clone(),
            vec![device_id(21)]
        );

        discovery.stop().await.expect("stop discovery");
    }

    fn config(device_id: DeviceId, display_name: &str, ws_port: u16) -> MdnsDiscoveryConfig {
        MdnsDiscoveryConfig {
            device_id,
            display_name: display_name.to_string(),
            ws_port,
        }
    }

    fn device_id(seed: u128) -> DeviceId {
        DeviceId(Uuid::from_u128(seed))
    }

    fn resolved_peer(
        device_id: DeviceId,
        display_name: &str,
        ws_port: u16,
        addresses: Vec<String>,
    ) -> MdnsResolvedService {
        let mut txt_records = BTreeMap::new();
        txt_records.insert("device_id".to_string(), device_id.0.to_string());
        txt_records.insert("display_name".to_string(), display_name.to_string());
        txt_records.insert("ws_port".to_string(), ws_port.to_string());

        MdnsResolvedService {
            service_type: JASMINE_MDNS_SERVICE_TYPE.to_string(),
            fullname: format!("{}.{}", device_id.0, JASMINE_MDNS_SERVICE_TYPE),
            host_name: format!("{}.local.", device_id.0),
            port: ws_port,
            addresses,
            txt_records,
        }
    }

    async fn settle() {
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;
    }

    struct FakeMdnsBackend {
        state: Mutex<FakeMdnsBackendState>,
        events_tx: mpsc::UnboundedSender<MdnsEvent>,
    }

    struct FakeMdnsBackendState {
        registrations: Vec<MdnsServiceRegistration>,
        browse_requests: Vec<String>,
        stop_browse_requests: Vec<String>,
        unregister_requests: Vec<String>,
        ip_check_intervals: Vec<u32>,
        shutdown_count: usize,
        events_rx: Option<mpsc::UnboundedReceiver<MdnsEvent>>,
    }

    impl FakeMdnsBackend {
        fn new() -> Arc<Self> {
            let (events_tx, events_rx) = mpsc::unbounded_channel();
            Arc::new(Self {
                state: Mutex::new(FakeMdnsBackendState {
                    registrations: Vec::new(),
                    browse_requests: Vec::new(),
                    stop_browse_requests: Vec::new(),
                    unregister_requests: Vec::new(),
                    ip_check_intervals: Vec::new(),
                    shutdown_count: 0,
                    events_rx: Some(events_rx),
                }),
                events_tx,
            })
        }

        fn send(&self, event: MdnsEvent) {
            self.events_tx.send(event).expect("send fake mdns event");
        }

        fn last_registration(&self) -> Option<MdnsServiceRegistration> {
            self.state
                .lock()
                .expect("lock fake backend state")
                .registrations
                .last()
                .cloned()
        }
    }

    impl MdnsBackend for FakeMdnsBackend {
        fn set_ip_check_interval(&self, interval_secs: u32) -> jasmine_core::Result<()> {
            self.state
                .lock()
                .expect("lock fake backend state")
                .ip_check_intervals
                .push(interval_secs);
            Ok(())
        }

        fn register(&self, registration: MdnsServiceRegistration) -> jasmine_core::Result<String> {
            self.state
                .lock()
                .expect("lock fake backend state")
                .registrations
                .push(registration.clone());
            Ok(format!(
                "{}.{}",
                registration.instance_name, registration.service_type
            ))
        }

        fn browse(
            &self,
            service_type: &str,
        ) -> jasmine_core::Result<mpsc::UnboundedReceiver<MdnsEvent>> {
            let mut state = self.state.lock().expect("lock fake backend state");
            state.browse_requests.push(service_type.to_string());
            Ok(state
                .events_rx
                .take()
                .expect("fake browse receiver available once"))
        }

        fn stop_browse(&self, service_type: &str) -> jasmine_core::Result<()> {
            self.state
                .lock()
                .expect("lock fake backend state")
                .stop_browse_requests
                .push(service_type.to_string());
            Ok(())
        }

        fn unregister(&self, fullname: &str) -> jasmine_core::Result<()> {
            self.state
                .lock()
                .expect("lock fake backend state")
                .unregister_requests
                .push(fullname.to_string());
            Ok(())
        }

        fn shutdown(&self) -> jasmine_core::Result<()> {
            self.state
                .lock()
                .expect("lock fake backend state")
                .shutdown_count += 1;
            Ok(())
        }
    }
}
