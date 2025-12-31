// This file is part of Substrate.

// Copyright (C) 2020-2022 Parity Technologies (UK) Ltd.
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with this program. If not, see <https://www.gnu.org/licenses/>.

use crate::ServicetoWorkerMsg;
use futures::{
    channel::{mpsc, oneshot},
    SinkExt,
};
use mpc_network::RoomId;
use std::fmt::Debug;
use tracing::error;

/// Service to interact with the [`crate::Worker`].
#[derive(Clone)]
pub struct Service {
    to_worker: mpsc::Sender<ServicetoWorkerMsg>,
}

impl Debug for Service {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("MPC-TSS-Service").finish()
    }
}

/// A [`Service`] allows to interact with a [`crate::Worker`], e.g. to start a keygen or a keysign.
/// [`crate::Worker`]'
impl Service {
    pub(crate) fn new(to_worker: mpsc::Sender<ServicetoWorkerMsg>) -> Self {
        Self { to_worker }
    }

    /// Start a keygen.
    pub async fn keygen(
        &mut self,
        t: u16,
        n: u16,
        payload: Vec<u8>,
        room_id: RoomId,
        sender: oneshot::Sender<anyhow::Result<Vec<u8>>>,
    ) {
        if let Err(_) = self
            .to_worker
            .send(ServicetoWorkerMsg::KeyGen(t, n, room_id, payload, sender))
            .await
        {
            // If send fails, the sender is still in the message, so we can't send error here
            // This should not happen in normal operation
            error!("Failed to send KeyGen message to worker - channel may be closed");
        }
    }

    /// [`crate::Worker`] to start a keysign.
    pub async fn keysign(
        &mut self,
        n: u16,
        room_id: RoomId,
        payload: Vec<u8>,
        sender: oneshot::Sender<anyhow::Result<Vec<u8>>>,
    ) {
        if let Err(_) = self
            .to_worker
            .send(ServicetoWorkerMsg::KeySign(n, room_id, payload, sender))
            .await
        {
            // If send fails, the sender is still in the message, so we can't send error here
            // This should not happen in normal operation
            error!("Failed to send KeySign message to worker - channel may be closed");
        }
    }
}
