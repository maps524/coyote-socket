// Lovense Standard API protocol — adds support for game integrations that
// expect to talk to a Lovense Remote local server. Mounted on the existing
// WebSocket port; `websocket::handle_connection` peeks the stream and routes
// non-Upgrade HTTP traffic to `handle_http_connection`.
//
// See: docs/plans/lr-integration.md (captured Lovense spec) and ../../../../LR_spoofer
// for the Node.js reference implementation that inspired this module.

pub mod handler;
pub mod messages;

pub use handler::{handle_http_connection, is_websocket_upgrade};
