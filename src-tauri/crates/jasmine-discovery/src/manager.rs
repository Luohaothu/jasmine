use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use jasmine_core::{
    CoreError, DeviceId, DiscoveryService, PeerDiscoveredCallback, PeerInfo, PeerLostCallback,
    Result,
};
use tokio::task::JoinHandle;
use tokio::time;
use tracing::warn;

use crate::{MdnsDiscovery, MdnsDiscoveryConfig, UdpBroadcastConfig, UdpBroadcastDiscovery};

pub const DISCOVERY_FALLBACK_DELAY: Duration = Duration::from_secs(10);

pub type ManagedServiceFuture<'a> = Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;

#[doc(hidden)]
pub trait ManagedDiscoveryService: Send + Sync {
    fn start_boxed(&self) -> ManagedServiceFuture<'_>;
    fn stop_boxed(&self) -> ManagedServiceFuture<'_>;
    fn on_peer_discovered(&mut self, callback: PeerDiscoveredCallback);
    fn on_peer_lost(&mut self, callback: PeerLostCallback);
}

impl ManagedDiscoveryService for MdnsDiscovery {
    fn start_boxed(&self) -> ManagedServiceFuture<'_> {
        Box::pin(async move { DiscoveryService::start(self).await })
    }

    fn stop_boxed(&self) -> ManagedServiceFuture<'_> {
        Box::pin(async move { DiscoveryService::stop(self).await })
    }

    fn on_peer_discovered(&mut self, callback: PeerDiscoveredCallback) {
        DiscoveryService::on_peer_discovered(self, callback);
    }

    fn on_peer_lost(&mut self, callback: PeerLostCallback) {
        DiscoveryService::on_peer_lost(self, callback);
    }
}

impl ManagedDiscoveryService for UdpBroadcastDiscovery {
    fn start_boxed(&self) -> ManagedServiceFuture<'_> {
        Box::pin(async move { DiscoveryService::start(self).await })
    }

    fn stop_boxed(&self) -> ManagedServiceFuture<'_> {
        Box::pin(async move { DiscoveryService::stop(self).await })
    }

    fn on_peer_discovered(&mut self, callback: PeerDiscoveredCallback) {
        DiscoveryService::on_peer_discovered(self, callback);
    }

    fn on_peer_lost(&mut self, callback: PeerLostCallback) {
        DiscoveryService::on_peer_lost(self, callback);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiscoverySource {
    Mdns,
    Udp,
}

#[derive(Default)]
struct ManagerCallbacks {
    discovered: Option<PeerDiscoveredCallback>,
    lost: Option<PeerLostCallback>,
}

#[derive(Default)]
struct SourcePeers {
    mdns: Option<PeerInfo>,
    udp: Option<PeerInfo>,
}

impl SourcePeers {
    fn update(&mut self, source: DiscoverySource, peer: PeerInfo) {
        match source {
            DiscoverySource::Mdns => self.mdns = Some(peer),
            DiscoverySource::Udp => self.udp = Some(peer),
        }
    }

    fn clear(&mut self, source: DiscoverySource) {
        match source {
            DiscoverySource::Mdns => self.mdns = None,
            DiscoverySource::Udp => self.udp = None,
        }
    }

    fn effective_peer(&self) -> Option<PeerInfo> {
        self.mdns.clone().or_else(|| self.udp.clone())
    }

    fn is_empty(&self) -> bool {
        self.mdns.is_none() && self.udp.is_none()
    }
}

#[derive(Default)]
struct ManagerState {
    callbacks: ManagerCallbacks,
    peers: HashMap<DeviceId, SourcePeers>,
}

#[derive(Default)]
struct RuntimeState {
    started: bool,
    udp_started: bool,
    fallback_task: Option<JoinHandle<()>>,
}

enum ManagerEmission {
    Discovered(PeerInfo),
    Lost(DeviceId),
}

pub struct DiscoveryManager<M = MdnsDiscovery, U = UdpBroadcastDiscovery> {
    mdns: Arc<M>,
    udp: Arc<U>,
    state: Arc<Mutex<ManagerState>>,
    runtime: Arc<Mutex<RuntimeState>>,
}

impl DiscoveryManager<MdnsDiscovery, UdpBroadcastDiscovery> {
    pub fn new(mdns_config: MdnsDiscoveryConfig, udp_config: UdpBroadcastConfig) -> Result<Self> {
        let mdns = MdnsDiscovery::new(mdns_config)?;
        Ok(Self::with_services(
            mdns,
            UdpBroadcastDiscovery::new(udp_config),
        ))
    }
}

impl<M, U> DiscoveryManager<M, U>
where
    M: ManagedDiscoveryService + 'static,
    U: ManagedDiscoveryService + 'static,
{
    pub fn with_services(mut mdns: M, mut udp: U) -> Self {
        let state = Arc::new(Mutex::new(ManagerState::default()));
        register_callbacks(&mut mdns, Arc::clone(&state), DiscoverySource::Mdns);
        register_callbacks(&mut udp, Arc::clone(&state), DiscoverySource::Udp);

        Self {
            mdns: Arc::new(mdns),
            udp: Arc::new(udp),
            state,
            runtime: Arc::new(Mutex::new(RuntimeState::default())),
        }
    }

    pub fn peers(&self) -> Vec<PeerInfo> {
        let mut peers: Vec<PeerInfo> = self
            .state
            .lock()
            .expect("lock discovery manager state")
            .peers
            .values()
            .filter_map(SourcePeers::effective_peer)
            .collect();
        peers.sort_by_key(|peer| peer.device_id.0.as_u128());
        peers
    }
}

impl<M, U> DiscoveryService for DiscoveryManager<M, U>
where
    M: ManagedDiscoveryService + 'static,
    U: ManagedDiscoveryService + 'static,
{
    async fn start(&self) -> Result<()> {
        {
            let runtime = self.runtime.lock().expect("lock discovery manager runtime");
            if runtime.started {
                return Ok(());
            }
        }

        match self.mdns.start_boxed().await {
            Ok(()) => {
                {
                    let mut runtime = self.runtime.lock().expect("lock discovery manager runtime");
                    runtime.started = true;
                    runtime.udp_started = false;
                    runtime.fallback_task = None;
                }

                let fallback_task = spawn_udp_fallback_task(
                    Arc::clone(&self.udp),
                    Arc::clone(&self.state),
                    Arc::clone(&self.runtime),
                );
                let mut runtime = self.runtime.lock().expect("lock discovery manager runtime");
                runtime.fallback_task = Some(fallback_task);
                Ok(())
            }
            Err(mdns_error) => {
                self.udp
                    .start_boxed()
                    .await
                    .map_err(|udp_error| combine_start_errors(mdns_error, udp_error))?;

                let mut runtime = self.runtime.lock().expect("lock discovery manager runtime");
                runtime.started = true;
                runtime.udp_started = true;
                runtime.fallback_task = None;
                Ok(())
            }
        }
    }

    async fn stop(&self) -> Result<()> {
        let fallback_task = {
            let mut runtime = self.runtime.lock().expect("lock discovery manager runtime");
            if !runtime.started {
                return Ok(());
            }

            runtime.started = false;
            runtime.udp_started = false;
            runtime.fallback_task.take()
        };

        if let Some(task) = fallback_task {
            task.abort();
            let _ = task.await;
        }

        let mdns_result = self.mdns.stop_boxed().await;
        let udp_result = self.udp.stop_boxed().await;
        self.state
            .lock()
            .expect("lock discovery manager state")
            .peers
            .clear();

        match (mdns_result, udp_result) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
            (Err(mdns_error), Err(udp_error)) => Err(CoreError::Validation(format!(
                "failed to stop discovery services: mdns: {mdns_error}; udp: {udp_error}"
            ))),
        }
    }

    fn on_peer_discovered(&mut self, callback: PeerDiscoveredCallback) {
        self.state
            .lock()
            .expect("lock discovery manager state")
            .callbacks
            .discovered = Some(callback);
    }

    fn on_peer_lost(&mut self, callback: PeerLostCallback) {
        self.state
            .lock()
            .expect("lock discovery manager state")
            .callbacks
            .lost = Some(callback);
    }
}

fn register_callbacks<S>(service: &mut S, state: Arc<Mutex<ManagerState>>, source: DiscoverySource)
where
    S: ManagedDiscoveryService + 'static,
{
    let discovered_state = Arc::clone(&state);
    service.on_peer_discovered(Arc::new(move |peer| {
        if let Some(emission) = record_discovered_peer(&discovered_state, source, peer) {
            emit_manager_event(&discovered_state, emission);
        }
    }));

    service.on_peer_lost(Arc::new(move |device_id| {
        if let Some(emission) = record_lost_peer(&state, source, device_id) {
            emit_manager_event(&state, emission);
        }
    }));
}

fn record_discovered_peer(
    state: &Arc<Mutex<ManagerState>>,
    source: DiscoverySource,
    peer: PeerInfo,
) -> Option<ManagerEmission> {
    let next_peer = {
        let mut state = state.lock().expect("lock discovery manager state");
        let peers = state.peers.entry(peer.device_id.clone()).or_default();
        let previous = peers.effective_peer();
        peers.update(source, peer);
        let current = peers.effective_peer();

        match (previous, current) {
            (prev, Some(current)) if prev.as_ref() != Some(&current) => Some(current),
            _ => None,
        }
    }?;

    Some(ManagerEmission::Discovered(next_peer))
}

fn record_lost_peer(
    state: &Arc<Mutex<ManagerState>>,
    source: DiscoverySource,
    device_id: DeviceId,
) -> Option<ManagerEmission> {
    let emission = {
        let mut state = state.lock().expect("lock discovery manager state");
        let (previous, current, should_remove) = {
            let peers = state.peers.get_mut(&device_id)?;
            let previous = peers.effective_peer();
            peers.clear(source);
            let current = peers.effective_peer();
            let should_remove = peers.is_empty();
            (previous, current, should_remove)
        };

        if should_remove {
            state.peers.remove(&device_id);
        }

        match (previous, current) {
            (Some(_), None) => Some(ManagerEmission::Lost(device_id)),
            (Some(previous), Some(current)) if previous != current => {
                Some(ManagerEmission::Discovered(current))
            }
            _ => None,
        }
    }?;

    Some(emission)
}

fn emit_manager_event(state: &Arc<Mutex<ManagerState>>, emission: ManagerEmission) {
    match emission {
        ManagerEmission::Discovered(peer) => {
            let callback = state
                .lock()
                .expect("lock discovery manager state")
                .callbacks
                .discovered
                .clone();

            if let Some(callback) = callback {
                callback(peer);
            }
        }
        ManagerEmission::Lost(device_id) => {
            let callback = state
                .lock()
                .expect("lock discovery manager state")
                .callbacks
                .lost
                .clone();

            if let Some(callback) = callback {
                callback(device_id);
            }
        }
    }
}

fn spawn_udp_fallback_task<U>(
    udp: Arc<U>,
    state: Arc<Mutex<ManagerState>>,
    runtime: Arc<Mutex<RuntimeState>>,
) -> JoinHandle<()>
where
    U: ManagedDiscoveryService + 'static,
{
    tokio::spawn(async move {
        time::sleep(DISCOVERY_FALLBACK_DELAY).await;

        if !should_start_udp(&state, &runtime) {
            return;
        }

        if let Err(error) = udp.start_boxed().await {
            warn!("failed to start udp fallback discovery: {error}");
            return;
        }

        let should_stop = {
            let mut runtime = runtime.lock().expect("lock discovery manager runtime");
            if runtime.started {
                runtime.udp_started = true;
                false
            } else {
                true
            }
        };

        if should_stop {
            let _ = udp.stop_boxed().await;
        }
    })
}

fn should_start_udp(state: &Arc<Mutex<ManagerState>>, runtime: &Arc<Mutex<RuntimeState>>) -> bool {
    let runtime = runtime.lock().expect("lock discovery manager runtime");
    if !runtime.started || runtime.udp_started {
        return false;
    }
    drop(runtime);

    state
        .lock()
        .expect("lock discovery manager state")
        .peers
        .is_empty()
}

fn combine_start_errors(mdns_error: CoreError, udp_error: CoreError) -> CoreError {
    CoreError::Validation(format!(
        "failed to start discovery manager: mdns: {mdns_error}; udp fallback: {udp_error}"
    ))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use jasmine_core::{CoreError, DeviceId, DiscoveryService, PeerInfo};
    use tokio::time;
    use uuid::Uuid;

    use super::{
        DiscoveryManager, ManagedDiscoveryService, ManagedServiceFuture, DISCOVERY_FALLBACK_DELAY,
    };

    #[tokio::test(start_paused = true)]
    async fn manager_mdns_success_skips_udp_after_peer_is_discovered() {
        let mdns = FakeDiscoveryService::new("mdns");
        let udp = FakeDiscoveryService::new("udp");
        let manager = DiscoveryManager::with_services(mdns.clone(), udp.clone());

        manager.start().await.expect("start manager");
        mdns.emit_discovered(peer(10, "Peer Alpha", 9735, &["192.168.1.10"]));
        settle().await;

        time::advance(DISCOVERY_FALLBACK_DELAY).await;
        settle().await;

        assert_eq!(mdns.start_call_count(), 1);
        assert_eq!(udp.start_call_count(), 0);
        assert_eq!(
            manager.peers(),
            vec![peer(10, "Peer Alpha", 9735, &["192.168.1.10"])]
        );
    }

    #[tokio::test]
    async fn manager_fallback_starts_udp_when_mdns_start_fails() {
        let mdns = FakeDiscoveryService::new("mdns")
            .with_start_error(CoreError::Validation("mdns unavailable".to_string()));
        let udp = FakeDiscoveryService::new("udp");
        let manager = DiscoveryManager::with_services(mdns.clone(), udp.clone());

        manager.start().await.expect("start manager with fallback");
        udp.emit_discovered(peer(20, "Peer Beta", 9844, &["192.168.1.20"]));
        settle().await;

        assert_eq!(mdns.start_call_count(), 1);
        assert_eq!(udp.start_call_count(), 1);
        assert_eq!(
            manager.peers(),
            vec![peer(20, "Peer Beta", 9844, &["192.168.1.20"])]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn manager_deduplicates_sources_and_prefers_mdns_peer_details() {
        let mdns = FakeDiscoveryService::new("mdns");
        let udp = FakeDiscoveryService::new("udp");
        let manager = DiscoveryManager::with_services(mdns.clone(), udp.clone());

        manager.start().await.expect("start manager");
        settle().await;
        time::advance(DISCOVERY_FALLBACK_DELAY).await;
        settle().await;

        udp.emit_discovered(peer(30, "Peer From Udp", 7777, &["10.0.0.30"]));
        settle().await;
        mdns.emit_discovered(peer(30, "Peer From Mdns", 9735, &["192.168.1.30"]));
        settle().await;

        assert_eq!(udp.start_call_count(), 1);
        assert_eq!(manager.peers().len(), 1);
        assert_eq!(
            manager.peers()[0],
            peer(30, "Peer From Mdns", 9735, &["192.168.1.30"])
        );
    }

    #[tokio::test(start_paused = true)]
    async fn manager_only_emits_lost_after_all_sources_lose_peer() {
        let mdns = FakeDiscoveryService::new("mdns");
        let udp = FakeDiscoveryService::new("udp");
        let lost = Arc::new(Mutex::new(Vec::<DeviceId>::new()));
        let mut manager = DiscoveryManager::with_services(mdns.clone(), udp.clone());

        let lost_sink = lost.clone();
        manager.on_peer_lost(Arc::new(move |device_id| {
            lost_sink.lock().expect("lock lost peers").push(device_id);
        }));

        manager.start().await.expect("start manager");
        settle().await;
        time::advance(DISCOVERY_FALLBACK_DELAY).await;
        settle().await;
        assert_eq!(udp.start_call_count(), 1);

        let shared_peer = peer(40, "Peer Delta", 9735, &["192.168.1.40"]);
        udp.emit_discovered(shared_peer.clone());
        mdns.emit_discovered(shared_peer.clone());
        settle().await;

        udp.emit_lost(device_id(40));
        settle().await;
        assert!(lost.lock().expect("lock lost peers").is_empty());
        assert_eq!(manager.peers(), vec![shared_peer.clone()]);

        mdns.emit_lost(device_id(40));
        settle().await;
        assert_eq!(
            lost.lock().expect("lock lost peers").clone(),
            vec![device_id(40)]
        );
        assert!(manager.peers().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn manager_stop_stops_both_services_and_cancels_pending_fallback() {
        let mdns = FakeDiscoveryService::new("mdns");
        let udp = FakeDiscoveryService::new("udp");
        let manager = DiscoveryManager::with_services(mdns.clone(), udp.clone());

        manager.start().await.expect("start manager");
        manager.stop().await.expect("stop manager");
        time::advance(DISCOVERY_FALLBACK_DELAY).await;
        settle().await;

        assert_eq!(mdns.stop_call_count(), 1);
        assert_eq!(udp.stop_call_count(), 1);
        assert_eq!(udp.start_call_count(), 0);
        assert!(manager.peers().is_empty());
    }

    async fn settle() {
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;
    }

    fn device_id(seed: u128) -> DeviceId {
        DeviceId(Uuid::from_u128(seed))
    }

    fn peer(seed: u128, display_name: &str, ws_port: u16, addresses: &[&str]) -> PeerInfo {
        PeerInfo {
            device_id: device_id(seed),
            user_id: None,
            display_name: display_name.to_string(),
            ws_port: Some(ws_port),
            addresses: addresses
                .iter()
                .map(|address| address.to_string())
                .collect(),
        }
    }

    #[derive(Default)]
    struct FakeServiceState {
        start_calls: usize,
        stop_calls: usize,
        start_error: Option<CoreError>,
        discovered: Option<jasmine_core::PeerDiscoveredCallback>,
        lost: Option<jasmine_core::PeerLostCallback>,
    }

    #[derive(Clone)]
    struct FakeDiscoveryService {
        state: Arc<Mutex<FakeServiceState>>,
    }

    impl FakeDiscoveryService {
        fn new(_name: &'static str) -> Self {
            Self {
                state: Arc::new(Mutex::new(FakeServiceState::default())),
            }
        }

        fn with_start_error(self, error: CoreError) -> Self {
            self.state.lock().expect("lock fake state").start_error = Some(error);
            self
        }

        fn emit_discovered(&self, peer: PeerInfo) {
            let callback = self
                .state
                .lock()
                .expect("lock fake state")
                .discovered
                .clone();

            if let Some(callback) = callback {
                callback(peer);
            }
        }

        fn emit_lost(&self, device_id: DeviceId) {
            let callback = self.state.lock().expect("lock fake state").lost.clone();

            if let Some(callback) = callback {
                callback(device_id);
            }
        }

        fn start_call_count(&self) -> usize {
            self.state.lock().expect("lock fake state").start_calls
        }

        fn stop_call_count(&self) -> usize {
            self.state.lock().expect("lock fake state").stop_calls
        }
    }

    impl ManagedDiscoveryService for FakeDiscoveryService {
        fn start_boxed(&self) -> ManagedServiceFuture<'_> {
            Box::pin(async move {
                let mut state = self.state.lock().expect("lock fake state");
                state.start_calls += 1;
                if let Some(error) = &state.start_error {
                    return Err(error.clone());
                }

                Ok(())
            })
        }

        fn stop_boxed(&self) -> ManagedServiceFuture<'_> {
            Box::pin(async move {
                self.state.lock().expect("lock fake state").stop_calls += 1;
                Ok(())
            })
        }

        fn on_peer_discovered(&mut self, callback: jasmine_core::PeerDiscoveredCallback) {
            self.state.lock().expect("lock fake state").discovered = Some(callback);
        }

        fn on_peer_lost(&mut self, callback: jasmine_core::PeerLostCallback) {
            self.state.lock().expect("lock fake state").lost = Some(callback);
        }
    }
}
