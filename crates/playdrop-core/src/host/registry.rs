//! Live connections per peer and the placement of the session stream (protocol.md 3.3).
//!
//! HTTP/2 lets only the dialler open streams, so a host can send requests only on a
//! connection it dialled. A pair may therefore hold two connections, one dialled by each
//! side. The session stream lives on exactly one of them: the one dialled by the lower
//! Device ID when both exist. A newer connection from the same dialler replaces the older
//! one. See the "Implementation notes" of protocol.md.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use super::lock;
use super::session::Session;
use crate::identity::DeviceId;
use crate::net::{CloseReason, Connection, Direction};

#[derive(Default)]
struct Link {
    inbound: Option<Arc<Connection>>,
    outbound: Option<Arc<Connection>>,
    session: Option<Arc<Session>>,
}

/// What to do with a new session stream (protocol.md 3.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Placement {
    /// No live session: install it.
    Install,
    /// It is preferred over the live one (or is from the same dialler and newer).
    Replace,
    /// The live one is preferred: refuse it (`409 conflict`).
    Reject,
}

/// Connections and sessions by peer.
pub(crate) struct Registry {
    local: DeviceId,
    links: Mutex<HashMap<DeviceId, Link>>,
    all: Mutex<HashMap<u64, Arc<Connection>>>,
}

impl Registry {
    pub(crate) fn new(local: DeviceId) -> Self {
        Self {
            local,
            links: Mutex::new(HashMap::new()),
            all: Mutex::new(HashMap::new()),
        }
    }

    /// True if `conn` was dialled by the lower of the two Device IDs (protocol.md 3.3).
    pub(crate) fn dialled_by_lower(&self, conn: &Connection) -> bool {
        let peer = conn.device_id();
        match conn.direction() {
            Direction::Outbound => self.local < peer,
            Direction::Inbound => peer < self.local,
        }
    }

    /// Remembers a connection so shutdown can close it.
    pub(crate) fn track(&self, conn: &Arc<Connection>) {
        lock(&self.all).insert(conn.id(), Arc::clone(conn));
    }

    /// Every live connection.
    pub(crate) fn all_connections(&self) -> Vec<Arc<Connection>> {
        lock(&self.all).values().cloned().collect()
    }

    /// Adds a trusted connection to its peer's link. A newer connection from the same
    /// dialler replaces the older one, which is closed (protocol.md 3.3). Returns the other
    /// direction's live connection, if any, for the 3.3 liveness check.
    pub(crate) fn attach(&self, conn: &Arc<Connection>) -> Option<Arc<Connection>> {
        let mut links = lock(&self.links);
        let link = links.entry(conn.device_id()).or_default();
        let (slot, other) = match conn.direction() {
            Direction::Inbound => (&mut link.inbound, &link.outbound),
            Direction::Outbound => (&mut link.outbound, &link.inbound),
        };
        let other = other.clone().filter(|c| !c.is_closed());
        if let Some(old) = slot.replace(Arc::clone(conn)) {
            if old.id() != conn.id() {
                old.close(CloseReason::Duplicate);
            }
        }
        other
    }

    /// Forgets a closed connection. A session on it is left for its own reader to end, so
    /// the peer is reported offline exactly once.
    pub(crate) fn detach(&self, conn: &Connection) {
        lock(&self.all).remove(&conn.id());
        let mut links = lock(&self.links);
        if let Some(link) = links.get_mut(&conn.device_id()) {
            for slot in [&mut link.inbound, &mut link.outbound] {
                if slot.as_ref().is_some_and(|c| c.id() == conn.id()) {
                    *slot = None;
                }
            }
        }
    }

    /// Our live outbound connection to `peer`.
    pub(crate) fn outbound(&self, peer: &DeviceId) -> Option<Arc<Connection>> {
        lock(&self.links)
            .get(peer)
            .and_then(|l| l.outbound.clone())
            .filter(|c| !c.is_closed())
    }

    /// Every live connection with `peer`.
    pub(crate) fn connections_of(&self, peer: &DeviceId) -> Vec<Arc<Connection>> {
        lock(&self.all)
            .values()
            .filter(|c| c.device_id() == *peer)
            .cloned()
            .collect()
    }

    /// The peer's live session.
    pub(crate) fn session(&self, peer: &DeviceId) -> Option<Arc<Session>> {
        lock(&self.links)
            .get(peer)
            .and_then(|l| l.session.clone())
            .filter(|s| s.is_live())
    }

    /// Every live session.
    pub(crate) fn sessions(&self) -> Vec<Arc<Session>> {
        lock(&self.links)
            .values()
            .filter_map(|l| l.session.clone())
            .filter(|s| s.is_live())
            .collect()
    }

    fn placement_locked(&self, link: &Link, conn: &Connection) -> Placement {
        let Some(existing) = link.session.as_ref().filter(|s| s.is_live()) else {
            return Placement::Install;
        };
        if existing.conn.id() == conn.id() {
            return Placement::Reject;
        }
        let old_pref = self.dialled_by_lower(&existing.conn);
        let new_pref = self.dialled_by_lower(conn);
        match (old_pref, new_pref) {
            (true, false) => Placement::Reject,
            _ => Placement::Replace,
        }
    }

    /// Where a session on `conn` would go (protocol.md 3.3).
    pub(crate) fn placement(&self, conn: &Connection) -> Placement {
        let links = lock(&self.links);
        match links.get(&conn.device_id()) {
            Some(link) => self.placement_locked(link, conn),
            None => Placement::Install,
        }
    }

    /// Installs `session` for its peer if the 3.3 rule allows. Returns the placement and
    /// the session it replaced, which the caller ends.
    pub(crate) fn install(&self, session: &Arc<Session>) -> (Placement, Option<Arc<Session>>) {
        let mut links = lock(&self.links);
        let link = links.entry(session.conn.device_id()).or_default();
        let placement = self.placement_locked(link, &session.conn);
        match placement {
            Placement::Reject => (placement, None),
            Placement::Install | Placement::Replace => {
                let old = link.session.replace(Arc::clone(session));
                (placement, old.filter(|o| o.id != session.id))
            }
        }
    }

    /// Removes `session` if it is still the installed one. Returns whether it was.
    pub(crate) fn uninstall(&self, session: &Session) -> bool {
        let mut links = lock(&self.links);
        let Some(link) = links.get_mut(&session.conn.device_id()) else {
            return false;
        };
        if link.session.as_ref().is_some_and(|s| s.id == session.id) {
            link.session = None;
            true
        } else {
            false
        }
    }

    /// Drops everything known about `peer` (unpair) and returns its connections.
    pub(crate) fn forget(&self, peer: &DeviceId) -> Vec<Arc<Connection>> {
        lock(&self.links).remove(peer);
        self.connections_of(peer)
    }
}
