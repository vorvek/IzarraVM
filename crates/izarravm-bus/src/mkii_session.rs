// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use std::marker::PhantomData;

#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct MkiiCounterPath {
    pub root_offset: u32,
    pub pointee_offset: u32,
}

impl MkiiCounterPath {
    pub const DIRECT: u32 = u32::MAX;

    pub const fn direct(root_offset: u32) -> Self {
        Self {
            root_offset,
            pointee_offset: Self::DIRECT,
        }
    }

    pub const fn indirect(root_offset: u32, pointee_offset: u32) -> Self {
        Self {
            root_offset,
            pointee_offset,
        }
    }

    pub const fn trace(root_offset: u32) -> Self {
        Self::indirect(
            root_offset,
            std::mem::offset_of!(crate::BusTrace, elapsed_clocks) as u32,
        )
    }
}

#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct MkiiBusSessionParts {
    pub trace_clocks: MkiiCounterPath,
    pub isa_clocks: MkiiCounterPath,
    pub mapping_epoch: MkiiCounterPath,
    pub trace_origin: u64,
    pub cost_epoch: u64,
    pub bus_numerator: u64,
    pub bus_denominator: u64,
}

/// Authenticates mkII's live counter layout and inert policy for one CPU run.
pub struct MkiiBusSession<'a, B> {
    owner: *const B,
    parts: MkiiBusSessionParts,
    borrow: PhantomData<&'a B>,
}

impl<'a, B> MkiiBusSession<'a, B> {
    /// # Safety
    /// Each path must name aligned, initialized `u64` storage in this exact `B`,
    /// directly or through an aligned slot readable as a native pointer value
    /// to live, aligned `u64` storage. Traversals start from the CPU
    /// invocation's original raw owner and reload child pointers after helpers.
    /// These requirements must hold throughout that invocation, after this
    /// acquisition borrow ends. Helpers must not re-enter native execution.
    ///
    /// Policy and cost epoch must remain fixed until the invocation returns.
    /// Certified source fetches and aligned plain-RAM reads must remain inert.
    /// The live total is ceil(((trace - origin) + ISA) * numerator / denominator),
    /// and mapping epochs share the owned-source namespace. No service request
    /// may be outstanding at acquisition; trace formation/certification cannot
    /// raise one. Other bus mutations must be observed before native continuation.
    #[allow(unsafe_code)]
    pub unsafe fn certify(owner: &'a B, parts: MkiiBusSessionParts) -> Option<Self> {
        (parts.bus_numerator != 0 && parts.bus_denominator != 0).then_some(Self {
            owner: std::ptr::from_ref(owner),
            parts,
            borrow: PhantomData,
        })
    }

    pub fn into_parts(self, expected_owner: *const B) -> Option<MkiiBusSessionParts> {
        std::ptr::eq(self.owner, expected_owner).then_some(self.parts)
    }
}
