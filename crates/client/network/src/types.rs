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

//! Network type definitions

use std::{
    borrow::{Borrow, Cow},
    fmt,
    hash::{Hash, Hasher},
    ops::Deref,
    sync::Arc,
};

/// The protocol name transmitted on the wire.
#[derive(Debug, Clone)]
pub enum ProtocolName {
    /// The protocol name as a static string.
    Static(&'static str),
    /// The protocol name as a dynamically allocated string.
    OnHeap(Arc<str>),
}

impl From<&'static str> for ProtocolName {
    fn from(name: &'static str) -> Self {
        Self::Static(name)
    }
}

impl From<Arc<str>> for ProtocolName {
    fn from(name: Arc<str>) -> Self {
        Self::OnHeap(name)
    }
}

impl From<String> for ProtocolName {
    fn from(name: String) -> Self {
        Self::OnHeap(Arc::from(name))
    }
}

impl<'a> From<Cow<'a, str>> for ProtocolName {
    fn from(name: Cow<'a, str>) -> Self {
        match name {
            Cow::Borrowed(s) => Self::OnHeap(Arc::from(s)),
            Cow::Owned(s) => Self::OnHeap(Arc::from(s)),
        }
    }
}

impl Deref for ProtocolName {
    type Target = str;

    fn deref(&self) -> &str {
        match self {
            Self::Static(name) => name,
            Self::OnHeap(name) => &name,
        }
    }
}

impl Borrow<str> for ProtocolName {
    fn borrow(&self) -> &str {
        self
    }
}

impl PartialEq for ProtocolName {
    fn eq(&self, other: &Self) -> bool {
        (self as &str) == (other as &str)
    }
}

impl Eq for ProtocolName {}

impl Hash for ProtocolName {
    fn hash<H: Hasher>(&self, state: &mut H) {
        (self as &str).hash(state)
    }
}

impl fmt::Display for ProtocolName {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(self)
    }
}

impl AsRef<str> for ProtocolName {
    fn as_ref(&self) -> &str {
        self as &str
    }
}

/// Reputation change for a peer.
///
/// This is a simplified version of the reputation change system.
/// A positive value increases reputation, a negative value decreases it.
pub type ReputationChange = i32;
