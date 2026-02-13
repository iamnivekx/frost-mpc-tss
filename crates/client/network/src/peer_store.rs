// This file is part of Substrate.
//
// Copyright (C) Parity Technologies (UK) Ltd.
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program. If not, see <https://www.gnu.org/licenses/>.

//! [`PeerStoreProvider`] provides peer reputation management.
//!
//! This is a simplified version of the peer store for the request-response protocol.
//! It only provides the `peer_reputation` method needed by the request-response handler.

use libp2p::PeerId;
use std::fmt::Debug;

/// We don't accept nodes whose reputation is under this value.
pub const BANNED_THRESHOLD: i32 = 71 * (i32::MIN / 100);

/// Trait providing peer reputation management.
///
/// This is a simplified version that only provides the reputation check needed
/// by the request-response protocol handler.
pub trait PeerStoreProvider: Debug + Send + Sync {
    /// Get peer reputation.
    fn peer_reputation(&self, peer_id: &PeerId) -> i32;
}

/// Mock implementation of [`PeerStoreProvider`] for testing or when peer store is not needed.
///
/// This implementation always returns a reputation of 0, meaning peers are not banned.
#[derive(Debug, Clone)]
pub struct MockPeerStore;

impl PeerStoreProvider for MockPeerStore {
    fn peer_reputation(&self, _peer_id: &PeerId) -> i32 {
        0
    }
}
