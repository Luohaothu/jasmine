use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use jasmine_core::ProtocolMessage;
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::JoinHandle;
use tokio_tungstenite::connect_async;

use crate::transport::{perform_client_handshake, run_connection_loop, ConnectionCommand};
use crate::{MessagingError, Result, WsClientConfig, WsDisconnectReason, WsPeerConnection};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WsClientEvent {
    MessageReceived { message: ProtocolMessage },
    Disconnected { reason: WsDisconnectReason },
}

pub struct WsClient {
    remote_peer: WsPeerConnection,
    events: broadcast::Sender<WsClientEvent>,
    command_tx: mpsc::Sender<ConnectionCommand>,
    shutdown_tx: watch::Sender<bool>,
    task: Mutex<Option<JoinHandle<()>>>,
    closed: AtomicBool,
}

impl WsClient {
    pub async fn connect(url: impl AsRef<str>, config: WsClientConfig) -> Result<Self> {
        let url = url.as_ref().to_string();
        let (mut socket, _) = connect_async(url.as_str())
            .await
            .map_err(|error| MessagingError::Transport(error.to_string()))?;
        let remote_peer = perform_client_handshake(
            &mut socket,
            &config.local_peer,
            ws_endpoint(&url),
            config.handshake_timeout,
        )
        .await?;
        let (events, _) = broadcast::channel(256);
        let (command_tx, command_rx) = mpsc::channel(64);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let event_sender = events.clone();
        let message_sender = event_sender.clone();

        let task = tokio::spawn(async move {
            let reason = run_connection_loop(
                socket,
                config.heartbeat_interval,
                config.max_missed_heartbeats,
                command_rx,
                shutdown_rx,
                move |message| {
                    let _ = message_sender.send(WsClientEvent::MessageReceived { message });
                },
            )
            .await;
            let _ = event_sender.send(WsClientEvent::Disconnected { reason });
        });

        Ok(Self {
            remote_peer,
            events,
            command_tx,
            shutdown_tx,
            task: Mutex::new(Some(task)),
            closed: AtomicBool::new(false),
        })
    }

    pub fn remote_peer(&self) -> Option<WsPeerConnection> {
        Some(self.remote_peer.clone())
    }

    pub fn subscribe(&self) -> broadcast::Receiver<WsClientEvent> {
        self.events.subscribe()
    }

    pub async fn send(&self, message: ProtocolMessage) -> Result<()> {
        self.command_tx
            .send(ConnectionCommand::Send(message))
            .await
            .map_err(|_| MessagingError::Transport("websocket client is closed".to_string()))
    }

    pub async fn disconnect(&self) -> Result<()> {
        if !self.closed.swap(true, Ordering::SeqCst) {
            let _ = self.shutdown_tx.send(true);
            let _ = self.command_tx.send(ConnectionCommand::Close).await;
        }

        let task = self.task.lock().expect("lock ws client task").take();
        if let Some(task) = task {
            let _ = task.await;
        }

        Ok(())
    }
}

fn ws_endpoint(url: &str) -> String {
    url.strip_prefix("ws://")
        .or_else(|| url.strip_prefix("wss://"))
        .unwrap_or(url)
        .split('/')
        .next()
        .unwrap_or(url)
        .to_string()
}
