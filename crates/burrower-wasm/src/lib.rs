// SPDX-License-Identifier: MPL-2.0
// Copyright (c) Jonathan D.A. Jewell <j.d.a.jewell@open.ac.uk>
//! # Burrower WASM shim
//!
//! Thin C-ABI wrapper around `burrower-core` so the engine ships as
//! a stand-alone `.wasm` artifact. The artifact is the input to
//! ECHIDNA's `typed_wasm` prover oracle (see
//! `docs/TYPED-WASM-VERIFICATION.adoc`).
//!
//! ## ABI
//!
//! The exposed entry points take `*const u8` + `usize` length pairs
//! for inputs (UTF-8 strings) and write outputs into a guest-supplied
//! buffer, returning the number of bytes written (or a negative error
//! code). This is the conservative ABI — no allocator, no globals,
//! all data flows through caller-managed memory.
//!
//! Future: ergonomic JS wrapper via `wasm-bindgen`. Today: minimal
//! ABI to keep the artifact small and verifier-friendly.
//!
//! Note: `burrower-core` brings `std` in transitively (anyhow, walkdir,
//! serde_json, …), so this shim shares std too. A no_std core is a
//! future possibility but not required for the MVP.

use std::slice;

/// Read a UTF-8 string from `(ptr, len)`. Returns empty on invalid input.
unsafe fn read_str(ptr: *const u8, len: usize) -> String {
    if ptr.is_null() || len == 0 {
        return String::new();
    }
    let bytes = slice::from_raw_parts(ptr, len);
    core::str::from_utf8(bytes).unwrap_or("").into()
}

/// Write a UTF-8 string into `(out_ptr, out_cap)`. Returns bytes
/// written, or a negative error:
///   -1 = output buffer too small
///   -2 = null output pointer
unsafe fn write_str(s: &str, out_ptr: *mut u8, out_cap: usize) -> i32 {
    if out_ptr.is_null() {
        return -2;
    }
    let bytes = s.as_bytes();
    if bytes.len() > out_cap {
        return -1;
    }
    let dst = slice::from_raw_parts_mut(out_ptr, bytes.len());
    dst.copy_from_slice(bytes);
    bytes.len() as i32
}

/// `parse_goal_json(goal_ptr, goal_len, out_ptr, out_cap) -> bytes_written`
///
/// Parses a goal string and writes the JSON-serialised `Goal` into
/// the output buffer.
#[no_mangle]
pub unsafe extern "C" fn parse_goal_json(
    goal_ptr: *const u8,
    goal_len: usize,
    out_ptr: *mut u8,
    out_cap: usize,
) -> i32 {
    let raw = read_str(goal_ptr, goal_len);
    let goal = burrower_core::parse_goal(&raw);
    let json = serde_json::to_string(&goal).unwrap_or_default();
    write_str(&json, out_ptr, out_cap)
}

/// `goal_hash_hex(goal_ptr, goal_len, out_ptr, out_cap) -> 16` (always)
///
/// Computes the goal hash and writes 16 hex chars.
#[no_mangle]
pub unsafe extern "C" fn goal_hash_hex(
    goal_ptr: *const u8,
    goal_len: usize,
    out_ptr: *mut u8,
    out_cap: usize,
) -> i32 {
    let raw = read_str(goal_ptr, goal_len);
    let h = burrower_core::goal_hash(&raw);
    write_str(&h, out_ptr, out_cap)
}

/// `version() -> bytes_written`. Writes the Burrower version into out_ptr.
#[no_mangle]
pub unsafe extern "C" fn version(out_ptr: *mut u8, out_cap: usize) -> i32 {
    write_str("burrower-core 0.0.1", out_ptr, out_cap)
}

/// `add(a, b) -> a + b`. Smoke test for the wasm import surface —
/// confirms the module loads and basic call works.
#[no_mangle]
pub extern "C" fn add(a: u32, b: u32) -> u32 {
    a.wrapping_add(b)
}

// ---------------------------------------------------------------------
// Memory management for hosts that don't bring their own allocator.
// Hosts call `alloc(n)` to get a pointer they can write into, and
// `dealloc(ptr, n)` to free it.
// ---------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn alloc(n: usize) -> *mut u8 {
    let mut buf: Vec<u8> = Vec::with_capacity(n);
    let ptr = buf.as_mut_ptr();
    core::mem::forget(buf);
    ptr
}

#[no_mangle]
pub unsafe extern "C" fn dealloc(ptr: *mut u8, n: usize) {
    if ptr.is_null() || n == 0 {
        return;
    }
    let _ = Vec::from_raw_parts(ptr, n, n);
}
