//! Configuration for the Editor Host HTTP BRP and `WebSocket` IPC bridge.

use core::net::{IpAddr, Ipv4Addr};

use bevy_ecs::resource::Resource;

/// Default TCP port used by the HTTP Bevy Remote Protocol server.
pub const DEFAULT_BRP_PORT: u16 = 15702;

/// Default TCP port used by the `WebSocket` editor IPC and frame stream server.
pub const DEFAULT_IPC_PORT: u16 = 15704;

/// Configuration resource for [`crate::EditorBridgePlugin`].
#[derive(Resource, Debug, Clone, PartialEq, Eq)]
pub struct EditorBridgeConfig {
    /// Host IP address bound by HTTP BRP and `WebSocket` IPC listeners.
    pub host: IpAddr,
    /// Port bound by the HTTP Bevy Remote Protocol server.
    pub brp_port: u16,
    /// Port bound by the `WebSocket` IPC server (`0` selects an ephemeral OS port).
    pub ipc_port: u16,
    /// Enables the HTTP JSON-RPC server (`RemoteHttpPlugin`).
    pub enable_http_brp: bool,
    /// Enables the real-time `WebSocket` IPC and binary frame server.
    pub enable_ws_ipc: bool,
    /// Adds permissive localhost CORS headers to HTTP BRP responses for the Tauri webview.
    pub allow_cors: bool,
}

impl Default for EditorBridgeConfig {
    fn default() -> Self {
        Self {
            host: IpAddr::V4(Ipv4Addr::LOCALHOST),
            brp_port: DEFAULT_BRP_PORT,
            ipc_port: DEFAULT_IPC_PORT,
            enable_http_brp: true,
            enable_ws_ipc: true,
            allow_cors: true,
        }
    }
}
