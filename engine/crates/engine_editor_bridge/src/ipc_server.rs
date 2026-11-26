//! Threaded `WebSocket` IPC server for binary viewport frames and real-time editor events.

use core::{
    net::{IpAddr, SocketAddr},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::Duration,
};
use std::{
    collections::HashMap,
    io::ErrorKind,
    net::{TcpListener, TcpStream},
    sync::{Arc, Mutex},
    thread,
};

use bevy_camera::visibility::Visibility;
use bevy_ecs::{
    hierarchy::ChildOf,
    lifecycle::RemovedComponents,
    name::Name,
    query::{Added, Changed, Or, With},
    resource::Resource,
    system::{Query, ResMut},
    world::World,
};
use bevy_time::Time;
use crossbeam_channel::{bounded, unbounded, Receiver, Sender};
use engine_core::{ProjectManifest, WorldMode};
use engine_protocol::{
    encode_viewport_frame, ClientIpcMessage, FrameMetrics, GizmoMode, GizmoSnapConfig,
    GizmoSpace, ServerIpcMessage, ViewportCameraMode, ViewportMode, PROTOCOL_VERSION,
};
use engine_render::{
    gizmo::TransformGizmoState, ViewportConfig, ViewportFrameChannel, ViewportFramePacket,
    ViewportInputQueue,
};
use engine_scene::{build_scene_tree, ActiveScene, SceneEntity};
use tungstenite::{accept, Message};

use crate::{
    history::EditorHistory,
    selection::{BridgeDirtyFlags, EditorSelection},
};

/// Incoming IPC message tagged with its originating `WebSocket` client ID.
#[derive(Debug, Clone)]
pub struct InboundClientMessage {
    /// Internal identifier assigned to the client connection.
    pub client_id: u64,
    /// Decoded client IPC message payload.
    pub message: ClientIpcMessage,
}

struct ClientSlot {
    control_tx: Sender<String>,
    latest_frame: Arc<Mutex<Option<Vec<u8>>>>,
}

/// Non-blocking `WebSocket` IPC server running on background threads and synced inside [`bevy_app::Update`].
#[derive(Resource)]
pub struct EditorIpcServer {
    bound_addr: SocketAddr,
    shutdown: Arc<AtomicBool>,
    clients: Arc<Mutex<HashMap<u64, ClientSlot>>>,
    inbound_rx: Receiver<InboundClientMessage>,
}

impl EditorIpcServer {
    /// Binds a `WebSocket` IPC TCP listener on `(host, port)` (`port = 0` chooses an ephemeral port).
    pub fn bind(host: IpAddr, port: u16) -> std::io::Result<Self> {
        let listener = TcpListener::bind((host, port))?;
        listener.set_nonblocking(true)?;
        let bound_addr = listener.local_addr()?;

        let shutdown = Arc::new(AtomicBool::new(false));
        let clients: Arc<Mutex<HashMap<u64, ClientSlot>>> = Arc::new(Mutex::new(HashMap::new()));
        let (inbound_tx, inbound_rx) = unbounded();
        let next_client_id = Arc::new(AtomicU64::new(1));

        let accept_shutdown = Arc::clone(&shutdown);
        let accept_clients = Arc::clone(&clients);
        thread::Builder::new()
            .name("engine-ipc-accept".to_owned())
            .spawn(move || {
                run_accept_loop(
                    listener,
                    accept_shutdown,
                    accept_clients,
                    inbound_tx,
                    next_client_id,
                );
            })?;

        Ok(Self {
            bound_addr,
            shutdown,
            clients,
            inbound_rx,
        })
    }

    /// Returns the local socket address bound by the `WebSocket` server.
    #[must_use]
    pub const fn bound_addr(&self) -> SocketAddr {
        self.bound_addr
    }

    /// Returns the number of currently connected `WebSocket` clients.
    #[must_use]
    pub fn client_count(&self) -> usize {
        self.clients.lock().map_or(0, |guard| guard.len())
    }

    /// Drains all inbound client messages queued since the last call.
    pub fn drain_inbound(&self) -> Vec<InboundClientMessage> {
        let mut out = Vec::new();
        while let Ok(msg) = self.inbound_rx.try_recv() {
            out.push(msg);
        }
        out
    }

    /// Sends a [`ServerIpcMessage`] to a single connected client.
    pub fn send_to(&self, client_id: u64, msg: &ServerIpcMessage) {
        let Ok(json) = serde_json::to_string(msg) else {
            return;
        };
        if let Ok(guard) = self.clients.lock()
            && let Some(slot) = guard.get(&client_id)
        {
            let _ = slot.control_tx.try_send(json);
        }
    }

    /// Broadcasts a [`ServerIpcMessage`] to all connected clients.
    pub fn broadcast_message(&self, msg: &ServerIpcMessage) {
        let Ok(json) = serde_json::to_string(msg) else {
            return;
        };
        if let Ok(guard) = self.clients.lock() {
            for slot in guard.values() {
                let _ = slot.control_tx.try_send(json.clone());
            }
        }
    }

    /// Queues a binary `BV9F` frame for all connected clients, dropping older unsent frames per client.
    pub fn broadcast_binary_frame(&self, frame_bytes: Vec<u8>) {
        if let Ok(guard) = self.clients.lock() {
            for slot in guard.values() {
                if let Ok(mut latest) = slot.latest_frame.lock() {
                    *latest = Some(frame_bytes.clone());
                }
            }
        }
    }
}

impl Drop for EditorIpcServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
    }
}

fn run_accept_loop(
    listener: TcpListener,
    shutdown: Arc<AtomicBool>,
    clients: Arc<Mutex<HashMap<u64, ClientSlot>>>,
    inbound_tx: Sender<InboundClientMessage>,
    next_client_id: Arc<AtomicU64>,
) {
    while !shutdown.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _peer_addr)) => {
                let client_id = next_client_id.fetch_add(1, Ordering::Relaxed);
                let client_shutdown = Arc::clone(&shutdown);
                let client_map = Arc::clone(&clients);
                let client_inbound_tx = inbound_tx.clone();

                let spawn_result = thread::Builder::new()
                    .name(format!("engine-ipc-client-{client_id}"))
                    .spawn(move || {
                        run_client_connection(
                            client_id,
                            stream,
                            client_shutdown,
                            client_map,
                            client_inbound_tx,
                        );
                    });

                if let Err(err) = spawn_result {
                    tracing::warn!(error = %err, "failed to spawn IPC client worker thread");
                }
            }
            Err(err) if err.kind() == ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(20));
            }
            Err(err) => {
                if !shutdown.load(Ordering::Relaxed) {
                    tracing::warn!(error = %err, "IPC accept loop terminated unexpectedly");
                }
                break;
            }
        }
    }
}

fn run_client_connection(
    client_id: u64,
    stream: TcpStream,
    shutdown: Arc<AtomicBool>,
    clients: Arc<Mutex<HashMap<u64, ClientSlot>>>,
    inbound_tx: Sender<InboundClientMessage>,
) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_nodelay(true);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));

    let mut ws = match accept(stream) {
        Ok(ws) => ws,
        Err(err) => {
            tracing::debug!(client_id, error = %err, "WebSocket handshake failed");
            return;
        }
    };

    let _ = ws
        .get_ref()
        .set_read_timeout(Some(Duration::from_millis(5)));

    let (control_tx, control_rx) = bounded::<String>(128);
    let latest_frame = Arc::new(Mutex::new(None));

    if let Ok(mut guard) = clients.lock() {
        guard.insert(
            client_id,
            ClientSlot {
                control_tx: control_tx.clone(),
                latest_frame: Arc::clone(&latest_frame),
            },
        );
    }

    while !shutdown.load(Ordering::Relaxed) {
        while let Ok(json_msg) = control_rx.try_recv() {
            if ws.send(Message::text(json_msg)).is_err() {
                shutdown_client(&mut ws, client_id, &clients);
                return;
            }
        }

        let pending_frame = latest_frame.lock().ok().and_then(|mut guard| guard.take());
        if let Some(frame_bytes) = pending_frame
            && ws.send(Message::binary(frame_bytes)).is_err()
        {
            break;
        }

        match ws.read() {
            Ok(Message::Text(text)) => {
                match serde_json::from_str::<ClientIpcMessage>(text.as_str()) {
                    Ok(message) => {
                        let _ = inbound_tx.send(InboundClientMessage { client_id, message });
                    }
                    Err(err) => {
                        let err_msg = ServerIpcMessage::Error {
                            message: format!("Invalid client IPC message: {err}"),
                        };
                        if let Ok(json) = serde_json::to_string(&err_msg) {
                            let _ = control_tx.try_send(json);
                        }
                    }
                }
            }
            Ok(Message::Ping(payload)) => {
                if ws.send(Message::Pong(payload)).is_err() {
                    break;
                }
            }
            Ok(Message::Close(_)) => {
                break;
            }
            Ok(_) => {}
            Err(tungstenite::Error::Io(ref err))
                if matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(_) => {
                break;
            }
        }
    }

    shutdown_client(&mut ws, client_id, &clients);
}

fn shutdown_client(
    ws: &mut tungstenite::WebSocket<TcpStream>,
    client_id: u64,
    clients: &Arc<Mutex<HashMap<u64, ClientSlot>>>,
) {
    if let Ok(mut guard) = clients.lock() {
        guard.remove(&client_id);
    }
    let _ = ws.close(None);
}

#[derive(Resource, Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct LastBroadcastViewport {
    state: Option<(ViewportMode, ViewportCameraMode, u32, u32)>,
}

#[derive(Resource, Debug, Default)]
pub(crate) struct MetricsPublishState {
    last_sent_secs: Option<f64>,
    smoothed_fps: f32,
}

type HierarchyMutationFilter = Or<(
    Added<SceneEntity>,
    Changed<Name>,
    Changed<ChildOf>,
    Changed<Visibility>,
)>;

pub(crate) fn detect_scene_hierarchy_changes(
    mut dirty: ResMut<BridgeDirtyFlags>,
    mutated: Query<(), (With<SceneEntity>, HierarchyMutationFilter)>,
    mut removed_scene_entities: RemovedComponents<SceneEntity>,
    mut removed_child_of: RemovedComponents<ChildOf>,
) {
    let mut changed = !mutated.is_empty();
    if removed_scene_entities.read().next().is_some() {
        removed_scene_entities.clear();
        changed = true;
    }
    if removed_child_of.read().next().is_some() {
        removed_child_of.clear();
        changed = true;
    }
    if changed {
        dirty.hierarchy_dirty = true;
    }
}

pub(crate) fn process_ipc_inbound(world: &mut World) {
    let inbound = {
        let Some(ipc) = world.get_resource::<EditorIpcServer>() else {
            return;
        };
        ipc.drain_inbound()
    };

    for InboundClientMessage { client_id, message } in inbound {
        match message {
            ClientIpcMessage::Hello {
                protocol_version,
                client_name,
            } => {
                tracing::info!(
                    client_id,
                    client_name = %client_name,
                    protocol_version,
                    "Editor WebSocket client connected"
                );
                send_hello_snapshot(world, client_id, protocol_version);
            }
            ClientIpcMessage::ViewportInput { event } => {
                if let Some(mut queue) = world.get_resource_mut::<ViewportInputQueue>() {
                    queue.push(event);
                }
            }
            ClientIpcMessage::SetViewportMode { mode } => {
                let changed = if let Some(mut config) = world.get_resource_mut::<ViewportConfig>() {
                    if config.mode != mode {
                        config.mode = mode;
                        true
                    } else {
                        false
                    }
                } else {
                    false
                };
                if changed && let Some(mut dirty) = world.get_resource_mut::<BridgeDirtyFlags>() {
                    dirty.viewport_dirty = true;
                }
            }
            ClientIpcMessage::SetCameraMode { camera_mode } => {
                let changed = if let Some(mut config) = world.get_resource_mut::<ViewportConfig>() {
                    if config.camera_mode != camera_mode {
                        config.camera_mode = camera_mode;
                        true
                    } else {
                        false
                    }
                } else {
                    false
                };
                if changed && let Some(mut dirty) = world.get_resource_mut::<BridgeDirtyFlags>() {
                    dirty.viewport_dirty = true;
                }
            }
            ClientIpcMessage::SelectEntities { guids } => {
                let changed = world
                    .get_resource_mut::<EditorSelection>()
                    .is_some_and(|mut selection| selection.set(guids));
                if changed && let Some(mut dirty) = world.get_resource_mut::<BridgeDirtyFlags>() {
                    dirty.selection_dirty = true;
                }
            }
            ClientIpcMessage::SetGizmoState { mode, space, snap } => {
                if let Some(mut gizmo) = world.get_resource_mut::<TransformGizmoState>() {
                    let changed =
                        gizmo.mode != mode || gizmo.space != space || gizmo.snap != snap;
                    gizmo.mode = mode;
                    gizmo.space = space;
                    gizmo.snap = snap;
                    if changed {
                        gizmo.state_dirty = true;
                    }
                }
            }
            ClientIpcMessage::Ping { nonce } => {
                if let Some(ipc) = world.get_resource::<EditorIpcServer>() {
                    ipc.send_to(client_id, &ServerIpcMessage::Pong { nonce });
                }
            }
        }
    }
}

fn send_hello_snapshot(world: &mut World, client_id: u64, client_protocol_version: u32) {
    let project_name = world
        .get_resource::<ProjectManifest>()
        .map_or_else(|| "Untitled Project".to_owned(), |m| m.project.name.clone());

    let (scene_name, scene_path, world_mode) = world.get_resource::<ActiveScene>().map_or_else(
        || {
            (
                "Untitled Scene".to_owned(),
                None,
                world
                    .get_resource::<WorldMode>()
                    .copied()
                    .unwrap_or_default(),
            )
        },
        |active| {
            (
                active.name.clone(),
                active.path.as_ref().map(|p| p.display().to_string()),
                active.world_mode,
            )
        },
    );

    let (viewport_mode, camera_mode, width, height) =
        world.get_resource::<ViewportConfig>().map_or(
            (
                ViewportMode::Offscreen,
                ViewportCameraMode::Mode3D,
                1280,
                720,
            ),
            |cfg| (cfg.mode, cfg.camera_mode, cfg.width, cfg.height),
        );

    let (gizmo_mode, gizmo_space, gizmo_snap) = world
        .get_resource::<TransformGizmoState>()
        .map_or_else(
            || (GizmoMode::default(), GizmoSpace::default(), GizmoSnapConfig::default()),
            |g| (g.mode, g.space, g.snap),
        );

    let selection = world
        .get_resource::<EditorSelection>()
        .map(|s| s.guids().to_vec())
        .unwrap_or_default();

    let history_state = world
        .get_resource::<EditorHistory>()
        .map(EditorHistory::state)
        .unwrap_or_default();

    let roots = build_scene_tree(world);

    let Some(ipc) = world.get_resource::<EditorIpcServer>() else {
        return;
    };

    if client_protocol_version != PROTOCOL_VERSION {
        ipc.send_to(
            client_id,
            &ServerIpcMessage::Error {
                message: format!(
                    "Protocol version mismatch: client={client_protocol_version}, runtime={PROTOCOL_VERSION}"
                ),
            },
        );
    }

    ipc.send_to(
        client_id,
        &ServerIpcMessage::Welcome {
            protocol_version: PROTOCOL_VERSION,
            project_name,
            world_mode,
            scene_name,
            scene_path,
            viewport_mode,
            camera_mode,
        },
    );
    ipc.send_to(client_id, &ServerIpcMessage::HierarchySnapshot { roots });
    ipc.send_to(
        client_id,
        &ServerIpcMessage::SelectionChanged { guids: selection },
    );
    ipc.send_to(
        client_id,
        &ServerIpcMessage::ViewportStateChanged {
            viewport_mode,
            camera_mode,
            width,
            height,
        },
    );
    ipc.send_to(
        client_id,
        &ServerIpcMessage::GizmoStateChanged {
            mode: gizmo_mode,
            space: gizmo_space,
            snap: gizmo_snap,
        },
    );
    ipc.send_to(
        client_id,
        &ServerIpcMessage::HistoryStateChanged {
            state: history_state,
        },
    );
}

pub(crate) fn publish_ipc_updates(world: &mut World) {
    let client_count = world
        .get_resource::<EditorIpcServer>()
        .map_or(0, EditorIpcServer::client_count);

    let (hierarchy_dirty, selection_dirty, viewport_dirty) =
        if let Some(mut dirty) = world.get_resource_mut::<BridgeDirtyFlags>() {
            let flags = (
                dirty.hierarchy_dirty,
                dirty.selection_dirty,
                dirty.viewport_dirty,
            );
            dirty.hierarchy_dirty = false;
            dirty.selection_dirty = false;
            dirty.viewport_dirty = false;
            flags
        } else {
            (false, false, false)
        };

    let gizmo_update = world
        .get_resource_mut::<TransformGizmoState>()
        .and_then(|mut gizmo| {
            if gizmo.state_dirty {
                gizmo.state_dirty = false;
                Some((gizmo.mode, gizmo.space, gizmo.snap))
            } else {
                None
            }
        });

    let mut refresh_guids = world
        .get_resource_mut::<TransformGizmoState>()
        .map(|mut gizmo| core::mem::take(&mut gizmo.inspector_refresh_guids))
        .unwrap_or_default();

    let (history_update, history_refresh) = world
        .get_resource_mut::<EditorHistory>()
        .map_or((None, Vec::new()), |mut history| {
            let state = if history.dirty {
                history.dirty = false;
                Some(history.state())
            } else {
                None
            };
            let guids = core::mem::take(&mut history.pending_inspector_refresh);
            (state, guids)
        });
    for guid in history_refresh {
        if !refresh_guids.contains(&guid) {
            refresh_guids.push(guid);
        }
    }

    let latest_packet = drain_latest_viewport_frame(world);

    if client_count == 0 {
        if let Some(current) = world
            .get_resource::<ViewportConfig>()
            .map(|cfg| (cfg.mode, cfg.camera_mode, cfg.width, cfg.height))
            && let Some(mut last) = world.get_resource_mut::<LastBroadcastViewport>()
        {
            last.state = Some(current);
        }
        return;
    }

    if hierarchy_dirty {
        let roots = build_scene_tree(world);
        if let Some(ipc) = world.get_resource::<EditorIpcServer>() {
            ipc.broadcast_message(&ServerIpcMessage::HierarchySnapshot { roots });
        }
    }

    if selection_dirty {
        let guids = world
            .get_resource::<EditorSelection>()
            .map(|selection| selection.guids().to_vec())
            .unwrap_or_default();
        if let Some(ipc) = world.get_resource::<EditorIpcServer>() {
            ipc.broadcast_message(&ServerIpcMessage::SelectionChanged { guids });
        }
    }

    if let Some(cfg) = world.get_resource::<ViewportConfig>() {
        let current = (cfg.mode, cfg.camera_mode, cfg.width, cfg.height);
        let state_changed = world
            .get_resource::<LastBroadcastViewport>()
            .is_none_or(|last| last.state != Some(current));
        if viewport_dirty || state_changed {
            if let Some(mut last) = world.get_resource_mut::<LastBroadcastViewport>() {
                last.state = Some(current);
            }
            if let Some(ipc) = world.get_resource::<EditorIpcServer>() {
                ipc.broadcast_message(&ServerIpcMessage::ViewportStateChanged {
                    viewport_mode: current.0,
                    camera_mode: current.1,
                    width: current.2,
                    height: current.3,
                });
            }
        }
    }

    if let Some((mode, space, snap)) = gizmo_update
        && let Some(ipc) = world.get_resource::<EditorIpcServer>()
    {
        ipc.broadcast_message(&ServerIpcMessage::GizmoStateChanged { mode, space, snap });
    }

    if let Some(state) = history_update
        && let Some(ipc) = world.get_resource::<EditorIpcServer>()
    {
        ipc.broadcast_message(&ServerIpcMessage::HistoryStateChanged { state });
    }

    if !refresh_guids.is_empty()
        && let Some(ipc) = world.get_resource::<EditorIpcServer>()
    {
        ipc.broadcast_message(&ServerIpcMessage::InspectorRefresh {
            guids: refresh_guids,
        });
    }

    if let Some(packet) = latest_packet {
        match encode_viewport_frame(&packet.header, &packet.pixels) {
            Ok(encoded) => {
                if let Some(ipc) = world.get_resource::<EditorIpcServer>() {
                    ipc.broadcast_binary_frame(encoded);
                }
            }
            Err(err) => {
                tracing::warn!(error = %err, "failed to encode offscreen viewport frame");
            }
        }
    }

    publish_periodic_metrics(world);
}

fn drain_latest_viewport_frame(world: &World) -> Option<ViewportFramePacket> {
    let channel = world.get_resource::<ViewportFrameChannel>()?;
    let receiver = channel.receiver();
    let mut latest = None;
    while let Ok(packet) = receiver.try_recv() {
        latest = Some(packet);
    }
    latest
}

fn publish_periodic_metrics(world: &mut World) {
    let (now, dt) = world
        .get_resource::<Time>()
        .map_or((0.0, 0.0), |t| (t.elapsed_secs_f64(), t.delta_secs()));

    let (should_send, fps, frame_time_ms) =
        if let Some(mut state) = world.get_resource_mut::<MetricsPublishState>() {
            if dt > f32::EPSILON {
                let instant_fps = 1.0 / dt;
                if state.smoothed_fps <= 0.0 {
                    state.smoothed_fps = instant_fps;
                } else {
                    state.smoothed_fps = state.smoothed_fps * 0.9 + instant_fps * 0.1;
                }
            }
            let due = state
                .last_sent_secs
                .is_none_or(|prev| now - prev >= 0.5 || now < prev);
            if due {
                state.last_sent_secs = Some(now);
            }
            (due, state.smoothed_fps, dt * 1000.0)
        } else {
            (false, 0.0, 0.0)
        };

    if !should_send {
        return;
    }

    let entity_count = world.query::<()>().iter(world).count();
    let scene_entity_count = world
        .query_filtered::<(), With<SceneEntity>>()
        .iter(world)
        .count();

    if let Some(ipc) = world.get_resource::<EditorIpcServer>() {
        ipc.broadcast_message(&ServerIpcMessage::Metrics {
            metrics: FrameMetrics {
                fps,
                frame_time_ms,
                entity_count,
                scene_entity_count,
            },
        });
    }
}
