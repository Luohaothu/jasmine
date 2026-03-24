use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use jasmine_core::ProtocolMessage;
use jasmine_crypto::Session;
use tokio::net::TcpListener;
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::JoinHandle;
use tokio_tungstenite::accept_async;
use tracing::warn;

use crate::transport::{perform_server_handshake, run_connection_loop, ConnectionCommand};
use crate::{MessagingError, Result, WsDisconnectReason, WsPeerConnection, WsServerConfig};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WsServerEvent {
    PeerConnected {
        peer: WsPeerConnection,
    },
    MessageReceived {
        peer: WsPeerConnection,
        message: ProtocolMessage,
    },
    CallSignalingReceived {
        peer: WsPeerConnection,
        message: ProtocolMessage,
    },
    PeerDisconnected {
        peer: WsPeerConnection,
        reason: WsDisconnectReason,
    },
}

fn is_call_signaling_message(message: &ProtocolMessage) -> bool {
    matches!(
        message,
        ProtocolMessage::CallOffer { .. }
            | ProtocolMessage::CallAnswer { .. }
            | ProtocolMessage::IceCandidate { .. }
            | ProtocolMessage::CallHangup { .. }
            | ProtocolMessage::CallReject { .. }
    )
}

struct ServerPeerHandle {
    peer: WsPeerConnection,
    session: Arc<Mutex<Session>>,
    command_tx: mpsc::Sender<ConnectionCommand>,
}

struct WsServerInner {
    peers: Arc<Mutex<HashMap<String, ServerPeerHandle>>>,
    events: broadcast::Sender<WsServerEvent>,
    shutdown_tx: watch::Sender<bool>,
    accept_task: Mutex<Option<JoinHandle<()>>>,
    closed: AtomicBool,
}

pub struct WsServer {
    local_addr: SocketAddr,
    config: WsServerConfig,
    inner: Arc<WsServerInner>,
}

impl WsServer {
    pub async fn bind(config: WsServerConfig) -> Result<Self> {
        let listener = TcpListener::bind(&config.bind_addr)
            .await
            .map_err(|error| MessagingError::Transport(error.to_string()))?;
        let local_addr = listener
            .local_addr()
            .map_err(|error| MessagingError::Transport(error.to_string()))?;
        let (events, _) = broadcast::channel(256);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let peers = Arc::new(Mutex::new(HashMap::new()));
        let inner = Arc::new(WsServerInner {
            peers: Arc::clone(&peers),
            events,
            shutdown_tx,
            accept_task: Mutex::new(None),
            closed: AtomicBool::new(false),
        });

        let accept_task =
            spawn_accept_loop(listener, config.clone(), Arc::clone(&inner), shutdown_rx);
        inner
            .accept_task
            .lock()
            .expect("lock ws server accept task")
            .replace(accept_task);

        Ok(Self {
            local_addr,
            config,
            inner,
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    pub fn subscribe(&self) -> broadcast::Receiver<WsServerEvent> {
        self.inner.events.subscribe()
    }

    pub fn connected_peers(&self) -> Vec<WsPeerConnection> {
        let mut peers: Vec<WsPeerConnection> = self
            .inner
            .peers
            .lock()
            .expect("lock ws server peers")
            .values()
            .map(|handle| handle.peer.clone())
            .collect();
        peers.sort_by(|left, right| left.identity.device_id.cmp(&right.identity.device_id));
        peers
    }

    pub async fn send_to(&self, peer_device_id: &str, message: ProtocolMessage) -> Result<()> {
        let sender = self
            .inner
            .peers
            .lock()
            .expect("lock ws server peers")
            .get(peer_device_id)
            .map(|handle| handle.command_tx.clone())
            .ok_or_else(|| MessagingError::PeerNotConnected(peer_device_id.to_string()))?;

        sender
            .send(ConnectionCommand::Send(message))
            .await
            .map_err(|_| MessagingError::PeerNotConnected(peer_device_id.to_string()))
    }

    pub fn file_crypto_material(
        &self,
        peer_device_id: &str,
        file_id: &str,
    ) -> Result<([u8; 32], [u8; 8])> {
        let session = self
            .inner
            .peers
            .lock()
            .expect("lock ws server peers")
            .get(peer_device_id)
            .map(|handle| Arc::clone(&handle.session))
            .ok_or_else(|| MessagingError::PeerNotConnected(peer_device_id.to_string()))?;

        let material = session
            .lock()
            .expect("lock ws server session")
            .derive_file_crypto_material(file_id);

        Ok(material)
    }

    pub fn sender_key_distribution_material(&self, peer_device_id: &str) -> Result<[u8; 32]> {
        let session = self
            .inner
            .peers
            .lock()
            .expect("lock ws server peers")
            .get(peer_device_id)
            .map(|handle| Arc::clone(&handle.session))
            .ok_or_else(|| MessagingError::PeerNotConnected(peer_device_id.to_string()))?;

        let material = session
            .lock()
            .expect("lock ws server session")
            .sender_key_distribution_material();

        Ok(material)
    }

    pub async fn shutdown(&self) -> Result<()> {
        if self.inner.closed.swap(true, Ordering::SeqCst) {
            return Ok(());
        }

        let _ = self.inner.shutdown_tx.send(true);
        let peer_senders: Vec<mpsc::Sender<ConnectionCommand>> = self
            .inner
            .peers
            .lock()
            .expect("lock ws server peers")
            .values()
            .map(|handle| handle.command_tx.clone())
            .collect();

        for sender in peer_senders {
            let _ = sender.send(ConnectionCommand::Close).await;
        }

        let accept_task = self
            .inner
            .accept_task
            .lock()
            .expect("lock ws server accept task")
            .take();
        if let Some(task) = accept_task {
            let _ = task.await;
        }

        Ok(())
    }

    pub fn config(&self) -> &WsServerConfig {
        &self.config
    }
}

fn spawn_accept_loop(
    listener: TcpListener,
    config: WsServerConfig,
    inner: Arc<WsServerInner>,
    mut shutdown_rx: watch::Receiver<bool>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                changed = shutdown_rx.changed() => {
                    if changed.is_ok() && *shutdown_rx.borrow() {
                        break;
                    }
                }
                accepted = listener.accept() => {
                    match accepted {
                        Ok((stream, address)) => {
                            let config = config.clone();
                            let inner = Arc::clone(&inner);
                            let shutdown_rx = inner.shutdown_tx.subscribe();
                            tokio::spawn(async move {
                                if let Err(error) = handle_connection(stream, address, config, inner, shutdown_rx).await {
                                    warn!("websocket server connection ended with error: {error}");
                                }
                            });
                        }
                        Err(error) => {
                            if *shutdown_rx.borrow() {
                                break;
                            }
                            warn!("websocket accept failed: {error}");
                        }
                    }
                }
            }
        }
    })
}

async fn handle_connection(
    stream: tokio::net::TcpStream,
    address: SocketAddr,
    config: WsServerConfig,
    inner: Arc<WsServerInner>,
    shutdown_rx: watch::Receiver<bool>,
) -> Result<()> {
    let mut socket = accept_async(stream)
        .await
        .map_err(|error| MessagingError::Transport(error.to_string()))?;
    let handshake = perform_server_handshake(
        &mut socket,
        &config.local_peer,
        config.local_private_key.as_deref(),
        config.peer_key_store.as_deref(),
        address.to_string(),
        config.handshake_timeout,
    )
    .await?;
    let peer = handshake.peer;
    let session = Arc::new(Mutex::new(handshake.session));
    let (command_tx, command_rx) = mpsc::channel(64);

    let previous = inner.peers.lock().expect("lock ws server peers").insert(
        peer.identity.device_id.clone(),
        ServerPeerHandle {
            peer: peer.clone(),
            session: Arc::clone(&session),
            command_tx: command_tx.clone(),
        },
    );

    if let Some(previous) = previous {
        let _ = previous.command_tx.send(ConnectionCommand::Close).await;
    }

    let _ = inner
        .events
        .send(WsServerEvent::PeerConnected { peer: peer.clone() });

    let events = inner.events.clone();
    let peer_for_messages = peer.clone();
    let reason = run_connection_loop(
        socket,
        Arc::clone(&session),
        config.heartbeat_interval,
        config.max_missed_heartbeats,
        command_rx,
        shutdown_rx,
        move |message| {
            let signaling_message = is_call_signaling_message(&message).then(|| message.clone());
            let _ = events.send(WsServerEvent::MessageReceived {
                peer: peer_for_messages.clone(),
                message,
            });
            if let Some(message) = signaling_message {
                let _ = events.send(WsServerEvent::CallSignalingReceived {
                    peer: peer_for_messages.clone(),
                    message,
                });
            }
        },
    )
    .await;

    inner
        .peers
        .lock()
        .expect("lock ws server peers")
        .remove(&peer.identity.device_id);
    let _ = inner
        .events
        .send(WsServerEvent::PeerDisconnected { peer, reason });
    Ok(())
}
