//! The ntdll exports RamWarden uses, declared by hand.
//!
//! These are undocumented but have been stable for over a decade; Process
//! Explorer, RAMMap and Process Hacker all depend on them. Declaring them here,
//! in one place, keeps the `unsafe` surface small and independent of how the
//! `windows` crate chooses to expose (or not expose) them in a given version.

use std::ffi::c_void;

pub type NtStatus = i32;

#[link(name = "ntdll")]
unsafe extern "system" {
    pub fn NtQuerySystemInformation(
        class: u32,
        info: *mut c_void,
        length: u32,
        return_length: *mut u32,
    ) -> NtStatus;
    pub fn NtSetSystemInformation(class: u32, info: *mut c_void, length: u32) -> NtStatus;
    pub fn NtSuspendProcess(process: *mut c_void) -> NtStatus;
    pub fn NtResumeProcess(process: *mut c_void) -> NtStatus;
    pub fn NtQueryInformationProcess(
        process: *mut c_void,
        class: u32,
        info: *mut c_void,
        length: u32,
        return_length: *mut u32,
    ) -> NtStatus;
}

pub const STATUS_INFO_LENGTH_MISMATCH: NtStatus = 0xC000_0004_u32 as i32;
pub const STATUS_BUFFER_TOO_SMALL: NtStatus = 0xC000_0023_u32 as i32;
pub const STATUS_ACCESS_DENIED: NtStatus = 0xC000_0022_u32 as i32;
pub const STATUS_PRIVILEGE_NOT_HELD: NtStatus = 0xC000_0061_u32 as i32;
pub const STATUS_PROCESS_IS_TERMINATING: NtStatus = 0xC000_010A_u32 as i32;

/// `SYSTEM_INFORMATION_CLASS` values.
pub const SYSTEM_PROCESS_INFORMATION: u32 = 5;
pub const SYSTEM_MEMORY_LIST_INFORMATION: u32 = 80;
pub const SYSTEM_STORE_INFORMATION: u32 = 109;

/// `PROCESSINFOCLASS::ProcessCommandLineInformation` (Windows 8.1+).
pub const PROCESS_COMMAND_LINE_INFORMATION: u32 = 60;

pub fn success(status: NtStatus) -> bool {
    status >= 0
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct UnicodeString {
    pub length: u16,
    pub maximum_length: u16,
    pub buffer: *mut u16,
}

impl UnicodeString {
    /// # Safety
    /// `buffer` must point to `length` bytes of valid UTF-16, or be null.
    pub unsafe fn to_string_lossy(&self) -> String {
        if self.buffer.is_null() || self.length == 0 {
            return String::new();
        }
        // SAFETY: the caller guarantees the buffer covers `length` bytes.
        let units = unsafe { std::slice::from_raw_parts(self.buffer, self.length as usize / 2) };
        String::from_utf16_lossy(units)
    }
}

/// `SYSTEM_PROCESS_INFORMATION`, x64 layout. Followed in memory by
/// `number_of_threads` [`SystemThreadInformation`] entries.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SystemProcessInformation {
    pub next_entry_offset: u32,
    pub number_of_threads: u32,
    /// The private working set — Task Manager's default "Memory" column.
    pub working_set_private_size: i64,
    pub hard_fault_count: u32,
    pub number_of_threads_high_watermark: u32,
    pub cycle_time: u64,
    /// FILETIME: 100 ns intervals since 1601-01-01 UTC.
    pub create_time: i64,
    pub user_time: i64,
    pub kernel_time: i64,
    pub image_name: UnicodeString,
    pub base_priority: i32,
    pub unique_process_id: usize,
    pub inherited_from_unique_process_id: usize,
    pub handle_count: u32,
    pub session_id: u32,
    pub unique_process_key: usize,
    pub peak_virtual_size: usize,
    pub virtual_size: usize,
    pub page_fault_count: u32,
    pub peak_working_set_size: usize,
    pub working_set_size: usize,
    pub quota_peak_paged_pool_usage: usize,
    pub quota_paged_pool_usage: usize,
    pub quota_peak_non_paged_pool_usage: usize,
    pub quota_non_paged_pool_usage: usize,
    pub pagefile_usage: usize,
    pub peak_pagefile_usage: usize,
    /// Private committed bytes ("Commit size" in Task Manager).
    pub private_page_count: usize,
    pub read_operation_count: i64,
    pub write_operation_count: i64,
    pub other_operation_count: i64,
    pub read_transfer_count: i64,
    pub write_transfer_count: i64,
    pub other_transfer_count: i64,
}

/// `SYSTEM_THREAD_INFORMATION`, x64 layout.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SystemThreadInformation {
    pub kernel_time: i64,
    pub user_time: i64,
    pub create_time: i64,
    pub wait_time: u32,
    pub start_address: usize,
    pub client_process: usize,
    pub client_thread: usize,
    pub priority: i32,
    pub base_priority: i32,
    pub context_switches: u32,
    pub thread_state: u32,
    pub wait_reason: u32,
}

/// `KTHREAD_STATE::Waiting`.
pub const THREAD_STATE_WAITING: u32 = 5;
/// `KWAIT_REASON::Suspended`.
pub const WAIT_REASON_SUSPENDED: u32 = 5;

/// `SYSTEM_MEMORY_LIST_COMMAND` values for [`SYSTEM_MEMORY_LIST_INFORMATION`].
pub const MEMORY_PURGE_LOW_PRIORITY_STANDBY_LIST: u32 = 5;

/// `SYSTEM_STORE_INFORMATION`: a request to the store manager.
#[repr(C)]
pub struct SystemStoreInformation {
    /// Bits 0..8: version (1). Bits 8..32: `StoreInformationClass`.
    pub version_and_class: u32,
    pub data: *mut c_void,
    pub length: u32,
}

/// `SYSTEM_STORE_INFORMATION_CLASS::MemCompressionInfoRequest`.
pub const MEM_COMPRESSION_INFO_REQUEST: u32 = 22;

/// `SM_MEM_COMPRESSION_INFO_REQUEST`, version 3.
#[repr(C)]
#[derive(Default)]
pub struct MemCompressionInfo {
    /// Bits 0..8: version (3).
    pub version: u32,
    pub compression_pid: u32,
    /// Working set of the Memory Compression process, in pages.
    pub working_set_size: u32,
    pub total_data_compressed: usize,
    pub total_compressed_size: usize,
    pub total_unique_data_compressed: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The layouts are hand-written; a wrong size would walk the buffer at the
    /// wrong stride and read garbage for every process after the first.
    #[test]
    fn the_x64_layouts_have_their_documented_sizes() {
        assert_eq!(std::mem::size_of::<SystemProcessInformation>(), 0x100);
        assert_eq!(std::mem::size_of::<SystemThreadInformation>(), 0x50);
    }
}
