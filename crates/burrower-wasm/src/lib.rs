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
//! code). Data flows through caller-managed memory; optional `alloc` and
//! `dealloc` exports provide buffers for hosts without their own allocator.
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
///
/// # Safety
/// A non-null `goal_ptr` with nonzero `goal_len` must reference that many
/// initialized, readable bytes in one allocation. A non-null `out_ptr` must
/// permit exclusive writes of up to `out_cap` bytes. Both regions must remain
/// valid for this call; neither length may exceed `isize::MAX`.
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

/// `goal_hash_hex(goal_ptr, goal_len, out_ptr, out_cap) -> bytes_written`
///
/// Computes the goal hash and writes 16 hex chars, or returns a negative error.
///
/// # Safety
/// A non-null `goal_ptr` with nonzero `goal_len` must reference that many
/// initialized, readable bytes in one allocation. A non-null `out_ptr` must
/// permit exclusive writes of up to `out_cap` bytes. Both regions must remain
/// valid for this call; neither length may exceed `isize::MAX`.
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

/// Writes the Burrower version into `out_ptr`, returning its length or an error.
///
/// # Safety
/// A non-null `out_ptr` must permit exclusive writes of up to `out_cap` bytes
/// within one live allocation; `out_cap` must not exceed `isize::MAX`.
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

/// Allocate `n` uninitialized bytes, returning null for zero or allocation failure.
/// The caller must initialize bytes before passing them to the input functions.
#[no_mangle]
pub extern "C" fn alloc(n: usize) -> *mut u8 {
    if n == 0 {
        return std::ptr::null_mut();
    }
    let Ok(layout) = std::alloc::Layout::array::<u8>(n) else {
        return std::ptr::null_mut();
    };
    // SAFETY: the checked layout is nonzero and describes an allocation of bytes.
    unsafe { std::alloc::alloc(layout) }
}

/// Release a buffer obtained from `alloc`, whether or not it was initialized.
///
/// # Safety
/// For non-null `ptr` and nonzero `n`, `ptr` must be a live allocation returned
/// by this module's `alloc(n)`, with exactly the same `n`. It must not be used
/// after this call or freed more than once. Null or zero arguments are no-ops.
#[no_mangle]
pub unsafe extern "C" fn dealloc(ptr: *mut u8, n: usize) {
    if ptr.is_null() || n == 0 {
        return;
    }
    if let Ok(layout) = std::alloc::Layout::array::<u8>(n) {
        std::alloc::dealloc(ptr, layout);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abi_round_trips_goal_json_and_distinguishes_goal_hashes() {
        let first = b"lemma preserved: assumes \"True\" shows \"False\"";
        let second = b"lemma other: \"True\"";
        let mut output = [0u8; 1024];
        // SAFETY: the inputs and output are separate live byte arrays and all
        // lengths are bounded by those allocations.
        unsafe {
            let count = parse_goal_json(
                first.as_ptr(),
                first.len(),
                output.as_mut_ptr(),
                output.len(),
            );
            assert!(count > 0);
            let value: serde_json::Value =
                serde_json::from_slice(&output[..count as usize]).unwrap();
            assert_eq!(value["raw"], core::str::from_utf8(first).unwrap());
            assert_eq!(value["language"], "isabelle");
            assert!(value["tokens"]
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t == "preserved"));
            assert_eq!(
                goal_hash_hex(first.as_ptr(), first.len(), output.as_mut_ptr(), 16),
                16
            );
            let first_hash = output[..16].to_vec();
            assert!(first_hash.iter().all(u8::is_ascii_hexdigit));
            assert_eq!(
                goal_hash_hex(second.as_ptr(), second.len(), output.as_mut_ptr(), 16),
                16
            );
            assert_ne!(&output[..16], first_hash);
            assert_eq!(
                goal_hash_hex(first.as_ptr(), first.len(), output.as_mut_ptr(), 16),
                16
            );
            assert_eq!(&output[..16], first_hash);
        }
        assert_eq!(add(u32::MAX, 1), 0);
    }

    #[test]
    fn abi_rejects_short_or_null_outputs_without_writing_past_capacity() {
        let mut output = [0xa5; 256];
        let invalid_utf8 = [0xff, 0xfe];
        // SAFETY: null pointers have defined error/no-op behavior here;
        // all non-null pointers and capacities refer to live byte arrays.
        unsafe {
            assert_eq!(version(std::ptr::null_mut(), 0), -2);
            assert_eq!(version(output.as_mut_ptr(), 1), -1);
            assert_eq!(output, [0xa5; 256]);
            assert_eq!(
                goal_hash_hex(std::ptr::null(), 0, output.as_mut_ptr(), 15),
                -1
            );
            assert_eq!(output, [0xa5; 256]);
            let count = parse_goal_json(std::ptr::null(), 0, output.as_mut_ptr(), output.len());
            assert!(count > 0);
            let empty_json = output[..count as usize].to_vec();
            let parsed: serde_json::Value = serde_json::from_slice(&empty_json).unwrap();
            assert_eq!(parsed["raw"], "");
            let invalid_count = parse_goal_json(
                invalid_utf8.as_ptr(),
                invalid_utf8.len(),
                output.as_mut_ptr(),
                output.len(),
            );
            assert_eq!(count, invalid_count);
            assert_eq!(&output[..count as usize], empty_json);
            dealloc(std::ptr::null_mut(), 0);
        }
    }

    #[test]
    fn allocated_buffer_supports_output_and_uninitialized_release() {
        let buffer = alloc(64);
        assert!(!buffer.is_null());
        // SAFETY: this buffer is live, exclusively owned, and has capacity 64.
        unsafe {
            let written = version(buffer, 64);
            assert!(written > 0);
            assert_eq!(
                slice::from_raw_parts(buffer, written as usize),
                b"burrower-core 0.0.1"
            );
            dealloc(buffer, 64);
        }
        let uninitialized = alloc(17);
        assert!(!uninitialized.is_null());
        // SAFETY: release the unchanged allocation with its original size.
        unsafe { dealloc(uninitialized, 17) };
        assert!(alloc(0).is_null());
        assert!(alloc(usize::MAX).is_null());
    }
}
