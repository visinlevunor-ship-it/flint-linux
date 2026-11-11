//! The parts of the UEFI specification (2.10) this bootloader uses. Tables list every
//! function slot in order, so the offsets match the firmware's; slots that are never called
//! are plain `usize`.

#![allow(dead_code)]

use core::ffi::c_void;

pub type Handle = *mut c_void;
pub type Event = *mut c_void;
pub type Status = usize;

pub const SUCCESS: Status = 0;
const ERR: usize = 1 << (usize::BITS - 1);
pub const LOAD_ERROR: Status = ERR | 1;
pub const INVALID_PARAMETER: Status = ERR | 2;
pub const UNSUPPORTED: Status = ERR | 3;
pub const BUFFER_TOO_SMALL: Status = ERR | 5;
pub const NOT_READY: Status = ERR | 6;
pub const DEVICE_ERROR: Status = ERR | 7;
pub const OUT_OF_RESOURCES: Status = ERR | 9;
pub const NOT_FOUND: Status = ERR | 14;
pub const ACCESS_DENIED: Status = ERR | 15;
pub const ABORTED: Status = ERR | 21;
pub const SECURITY_VIOLATION: Status = ERR | 26;

pub fn is_error(s: Status) -> bool {
    s & ERR != 0
}

pub fn status_text(s: Status) -> &'static str {
    match s {
        LOAD_ERROR => "load error (not a valid kernel image?)",
        INVALID_PARAMETER => "invalid parameter",
        UNSUPPORTED => "unsupported",
        BUFFER_TOO_SMALL => "buffer too small",
        DEVICE_ERROR => "device error (the disk could not be read)",
        OUT_OF_RESOURCES => "out of memory",
        NOT_FOUND => "not found",
        ACCESS_DENIED => "access denied",
        ABORTED => "aborted",
        SECURITY_VIOLATION => "refused by Secure Boot",
        _ => "error",
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct Guid(pub u32, pub u16, pub u16, pub [u8; 8]);

pub const LOADED_IMAGE_GUID: Guid = Guid(0x5B1B31A1, 0x9562, 0x11D2, [0x8E, 0x3F, 0x00, 0xA0, 0xC9, 0x69, 0x72, 0x3B]);
pub const SIMPLE_FILE_SYSTEM_GUID: Guid = Guid(0x964E5B22, 0x6459, 0x11D2, [0x8E, 0x39, 0x00, 0xA0, 0xC9, 0x69, 0x72, 0x3B]);
pub const FILE_INFO_GUID: Guid = Guid(0x09576E92, 0x6D3F, 0x11D2, [0x8E, 0x39, 0x00, 0xA0, 0xC9, 0x69, 0x72, 0x3B]);
pub const DEVICE_PATH_GUID: Guid = Guid(0x09576E91, 0x6D3F, 0x11D2, [0x8E, 0x39, 0x00, 0xA0, 0xC9, 0x69, 0x72, 0x3B]);
pub const LOAD_FILE2_GUID: Guid = Guid(0x4006C0C1, 0xFCB3, 0x403E, [0x99, 0x6D, 0x4A, 0x6C, 0x87, 0x24, 0xE0, 0x6D]);
pub const GLOBAL_VARIABLE_GUID: Guid = Guid(0x8BE4DF61, 0x93CA, 0x11D2, [0xAA, 0x0D, 0x00, 0xE0, 0x98, 0x03, 0x2B, 0x8C]);
/// Where Linux's EFI stub looks for the initrd (a LoadFile2 protocol on this vendor media path).
pub const LINUX_INITRD_MEDIA_GUID: Guid = Guid(0x5568E427, 0x68FC, 0x4F3D, [0xAC, 0x74, 0xCA, 0x55, 0x52, 0x31, 0xCC, 0x68]);

#[repr(C)]
pub struct TableHeader {
    pub signature: u64,
    pub revision: u32,
    pub header_size: u32,
    pub crc32: u32,
    pub reserved: u32,
}

#[repr(C)]
pub struct SystemTable {
    pub hdr: TableHeader,
    pub firmware_vendor: *const u16,
    pub firmware_revision: u32,
    pub console_in_handle: Handle,
    pub con_in: *mut SimpleTextInput,
    pub console_out_handle: Handle,
    pub con_out: *mut SimpleTextOutput,
    pub standard_error_handle: Handle,
    pub std_err: *mut SimpleTextOutput,
    pub runtime_services: *mut RuntimeServices,
    pub boot_services: *mut BootServices,
    pub number_of_table_entries: usize,
    pub configuration_table: *mut c_void,
}

#[repr(C)]
pub struct InputKey {
    pub scan_code: u16,
    pub unicode_char: u16,
}

pub const SCAN_UP: u16 = 0x01;
pub const SCAN_DOWN: u16 = 0x02;
pub const SCAN_ESC: u16 = 0x17;

#[repr(C)]
pub struct SimpleTextInput {
    pub reset: extern "efiapi" fn(*mut SimpleTextInput, bool) -> Status,
    pub read_key_stroke: extern "efiapi" fn(*mut SimpleTextInput, *mut InputKey) -> Status,
    pub wait_for_key: Event,
}

#[repr(C)]
pub struct TextMode {
    pub max_mode: i32,
    pub mode: i32,
    pub attribute: i32,
    pub cursor_column: i32,
    pub cursor_row: i32,
    pub cursor_visible: bool,
}

#[repr(C)]
pub struct SimpleTextOutput {
    pub reset: extern "efiapi" fn(*mut SimpleTextOutput, bool) -> Status,
    pub output_string: extern "efiapi" fn(*mut SimpleTextOutput, *const u16) -> Status,
    pub test_string: usize,
    pub query_mode: extern "efiapi" fn(*mut SimpleTextOutput, usize, *mut usize, *mut usize) -> Status,
    pub set_mode: usize,
    pub set_attribute: extern "efiapi" fn(*mut SimpleTextOutput, usize) -> Status,
    pub clear_screen: extern "efiapi" fn(*mut SimpleTextOutput) -> Status,
    pub set_cursor_position: extern "efiapi" fn(*mut SimpleTextOutput, usize, usize) -> Status,
    pub enable_cursor: extern "efiapi" fn(*mut SimpleTextOutput, bool) -> Status,
    pub mode: *mut TextMode,
}

pub const ATTR_NORMAL: usize = 0x07; // light grey on black
pub const ATTR_HIGHLIGHT: usize = 0x70; // black on light grey
pub const ATTR_TITLE: usize = 0x0F; // white on black
pub const ATTR_ERROR: usize = 0x0C; // light red on black

pub const TIMER_RELATIVE: u32 = 2;
pub const EVT_TIMER: u32 = 0x8000_0000;

pub const ALLOCATE_ANY_PAGES: u32 = 0;
pub const LOADER_DATA: u32 = 2;

pub const NATIVE_INTERFACE: u32 = 0;

#[repr(C)]
pub struct BootServices {
    pub hdr: TableHeader,
    pub raise_tpl: usize,
    pub restore_tpl: usize,
    pub allocate_pages: extern "efiapi" fn(u32, u32, usize, *mut u64) -> Status,
    pub free_pages: extern "efiapi" fn(u64, usize) -> Status,
    pub get_memory_map: usize,
    pub allocate_pool: extern "efiapi" fn(u32, usize, *mut *mut c_void) -> Status,
    pub free_pool: extern "efiapi" fn(*mut c_void) -> Status,
    pub create_event: extern "efiapi" fn(u32, usize, usize, *mut c_void, *mut Event) -> Status,
    pub set_timer: extern "efiapi" fn(Event, u32, u64) -> Status,
    pub wait_for_event: extern "efiapi" fn(usize, *const Event, *mut usize) -> Status,
    pub signal_event: usize,
    pub close_event: extern "efiapi" fn(Event) -> Status,
    pub check_event: usize,
    pub install_protocol_interface: extern "efiapi" fn(*mut Handle, *const Guid, u32, *mut c_void) -> Status,
    pub reinstall_protocol_interface: usize,
    pub uninstall_protocol_interface: extern "efiapi" fn(Handle, *const Guid, *mut c_void) -> Status,
    pub handle_protocol: extern "efiapi" fn(Handle, *const Guid, *mut *mut c_void) -> Status,
    pub reserved: usize,
    pub register_protocol_notify: usize,
    pub locate_handle: usize,
    pub locate_device_path: usize,
    pub install_configuration_table: usize,
    pub load_image: extern "efiapi" fn(bool, Handle, *const DevicePathNode, *mut c_void, usize, *mut Handle) -> Status,
    pub start_image: extern "efiapi" fn(Handle, *mut usize, *mut *mut u16) -> Status,
    pub exit: usize,
    pub unload_image: extern "efiapi" fn(Handle) -> Status,
    pub exit_boot_services: usize,
    pub get_next_monotonic_count: usize,
    pub stall: extern "efiapi" fn(usize) -> Status,
    pub set_watchdog_timer: extern "efiapi" fn(usize, u64, usize, *const u16) -> Status,
    // ... more that are not used
}

pub const RESET_COLD: u32 = 0;

#[repr(C)]
pub struct RuntimeServices {
    pub hdr: TableHeader,
    pub get_time: usize,
    pub set_time: usize,
    pub get_wakeup_time: usize,
    pub set_wakeup_time: usize,
    pub set_virtual_address_map: usize,
    pub convert_pointer: usize,
    pub get_variable: extern "efiapi" fn(*const u16, *const Guid, *mut u32, *mut usize, *mut c_void) -> Status,
    pub get_next_variable_name: usize,
    pub set_variable: extern "efiapi" fn(*const u16, *const Guid, u32, usize, *const c_void) -> Status,
    pub get_next_high_monotonic_count: usize,
    pub reset_system: extern "efiapi" fn(u32, Status, usize, *const c_void) -> !,
}

pub const VARIABLE_NON_VOLATILE: u32 = 1;
pub const VARIABLE_BOOTSERVICE_ACCESS: u32 = 2;
pub const VARIABLE_RUNTIME_ACCESS: u32 = 4;
pub const OS_INDICATIONS_BOOT_TO_FW_UI: u64 = 1;

#[repr(C)]
pub struct LoadedImage {
    pub revision: u32,
    pub parent_handle: Handle,
    pub system_table: *mut SystemTable,
    pub device_handle: Handle,
    pub file_path: *const DevicePathNode,
    pub reserved: *mut c_void,
    pub load_options_size: u32,
    pub load_options: *mut c_void,
    pub image_base: *mut c_void,
    pub image_size: u64,
    pub image_code_type: u32,
    pub image_data_type: u32,
    pub unload: usize,
}

#[repr(C)]
pub struct SimpleFileSystem {
    pub revision: u64,
    pub open_volume: extern "efiapi" fn(*mut SimpleFileSystem, *mut *mut File) -> Status,
}

pub const FILE_MODE_READ: u64 = 1;

#[repr(C)]
pub struct File {
    pub revision: u64,
    pub open: extern "efiapi" fn(*mut File, *mut *mut File, *const u16, u64, u64) -> Status,
    pub close: extern "efiapi" fn(*mut File) -> Status,
    pub delete: usize,
    pub read: extern "efiapi" fn(*mut File, *mut usize, *mut c_void) -> Status,
    pub write: usize,
    pub get_position: usize,
    pub set_position: usize,
    pub get_info: extern "efiapi" fn(*mut File, *const Guid, *mut usize, *mut c_void) -> Status,
}

/// Device path node header; the node's data follows it.
#[repr(C, packed)]
pub struct DevicePathNode {
    pub kind: u8,
    pub subtype: u8,
    pub length: [u8; 2],
}

pub const DP_MEDIA: u8 = 4;
pub const DP_MEDIA_HARD_DRIVE: u8 = 1;
pub const DP_MEDIA_VENDOR: u8 = 3;
pub const DP_MEDIA_FILE_PATH: u8 = 4;
pub const DP_END: u8 = 0x7F;
pub const DP_END_ENTIRE: u8 = 0xFF;

#[repr(C)]
pub struct LoadFile2 {
    pub load_file: extern "efiapi" fn(*mut LoadFile2, *const DevicePathNode, bool, *mut usize, *mut c_void) -> Status,
}

/// VenMedia(LINUX_INITRD_MEDIA_GUID)/End, as one block of memory.
#[repr(C, packed)]
pub struct InitrdDevicePath {
    pub vendor: DevicePathNode,
    pub guid: Guid,
    pub end: DevicePathNode,
}
