//! The keeper: the long-lived parent of Space's agents, so the server can restart (by plan or
//! by crash) while agents keep working. See [`daemon`] for what it does and [`client`] for the
//! server's side.

pub mod client;
pub mod daemon;
pub mod proto;

pub use client::{AgentEvent, AgentPipe, KeeperClient, Welcome};
pub use proto::{AgentInfo, KEEPER_PROTOCOL, ORPHAN_REPLY, OpenRequest, OrphanReply, SpawnSpec};
