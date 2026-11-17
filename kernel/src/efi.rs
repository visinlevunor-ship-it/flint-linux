//! The parts of UEFI (2.10) the kernel uses before it takes over the machine: its command
//! line, the base image (through the LoadFile2 protocol flint-boot installs, as for Linux),
//! the framebuffer, and the memory map handed over by ExitBootServices.

#![allow(dead_code)]

use core::ffi::c_void;

pub type Handle = *mut c_void;
pub type Status = usize;

pub const SUCCESS: Status = 0;
const ERR: usize = 1 << (usize::BITS - 1);
pub const BUFFER_TOO_SMALL: Status = ERR | 5;

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct Guid(pub u32, pub u16, pub u16, pub [u8; 8]);

pub const LOADED_IMAGE_GUID: Guid = Guid(0x5B1B31A1, 0x9562, 0x11D2, [0x8E, 0x3F, 0x00, 0xA0, 0xC9, 0x69, 0x72, 0x3B]);
pub const LOAD_FILE2_GUID: Guid = Guid(0x4006C0C1, 0xFCB3, 0x403E, [0x99, 0x6D, 0x4A, 0x6C, 0x87, 0x24, 0xE0, 0x6D]);
pub const LINUX_INITRD_MEDIA_GUID: Guid = Guid(0x5568E427, 0x68FC, 0x4F3D, [0xAC, 0x74, 0xCA, 0x55, 0x52, 0x31, 0xCC, 0x68]);
pub const GOP_GUID: Guid = Guid(0x9042A9DE, 0x23DC, 0x4A38, [0x96, 0xFB, 0x7A, 0xDE, 0xD0, 0x80, 0x51, 0x6A]);

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
    pub con_in: *mut c_void,
    pub console_out_handle: Handle,
    pub con_out: *mut c_void,
    pub standard_error_handle: Handle,
    pub std_err: *mut c_void,
    pub runtime_services: *mut c_void,
    pub boot_services: *mut BootServices,
    pub number_of_table_entries: usize,
    pub configuration_table: *mut c_void,
}

/// Every slot up to LocateProtocol, in the specification's order.
#[repr(C)]
pub struct BootServices {
    pub hdr: TableHeader,
    pub raise_tpl: usize,
    pub restore_tpl: usize,
    pub allocate_pages: extern "efiapi" fn(u32, u32, usize, *mut u64) -> Status,
    pub free_pages: usize,
    pub get_memory_map: extern "efiapi" fn(*mut usize, *mut u8, *mut usize, *mut usize, *mut u32) -> Status,
    pub allocate_pool: extern "efiapi" fn(u32, usize, *mut *mut c_void) -> Status,
    pub free_pool: extern "efiapi" fn(*mut c_void) -> Status,
    pub create_event: usize,
    pub set_timer: usize,
    pub wait_for_event: usize,
    pub signal_event: usize,
    pub close_event: usize,
    pub check_event: usize,
    pub install_protocol_interface: usize,
    pub reinstall_protocol_interface: usize,
    pub uninstall_protocol_interface: usize,
    pub handle_protocol: extern "efiapi" fn(Handle, *const Guid, *mut *mut c_void) -> Status,
    pub reserved: usize,
    pub register_protocol_notify: usize,
    pub locate_handle: usize,
    pub locate_device_path: extern "efiapi" fn(*const Guid, *mut *const DevicePathNode, *mut Handle) -> Status,
    pub install_configuration_table: usize,
    pub load_image: usize,
    pub start_image: usize,
    pub exit: usize,
    pub unload_image: usize,
    pub exit_boot_services: extern "efiapi" fn(Handle, usize) -> Status,
    pub get_next_monotonic_count: usize,
    pub stall: usize,
    pub set_watchdog_timer: extern "efiapi" fn(usize, u64, usize, *const u16) -> Status,
    pub connect_controller: usize,
    pub disconnect_controller: usize,
    pub open_protocol: usize,
    pub close_protocol: usize,
    pub open_protocol_information: usize,
    pub protocols_per_handle: usize,
    pub locate_handle_buffer: usize,
    pub locate_protocol: extern "efiapi" fn(*const Guid, *mut c_void, *mut *mut c_void) -> Status,
}

pub const ALLOCATE_ANY_PAGES: u32 = 0;
pub const LOADER_DATA: u32 = 2;

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

#[repr(C, packed)]
pub struct DevicePathNode {
    pub kind: u8,
    pub subtype: u8,
    pub length: [u8; 2],
}

/// VenMedia(LINUX_INITRD_MEDIA_GUID)/End
#[repr(C, packed)]
pub struct InitrdDevicePath {
    pub vendor: DevicePathNode,
    pub guid: Guid,
    pub end: DevicePathNode,
}

pub static INITRD_PATH: InitrdDevicePath = InitrdDevicePath {
    vendor: DevicePathNode { kind: 4, subtype: 3, length: [20, 0] },
    guid: LINUX_INITRD_MEDIA_GUID,
    end: DevicePathNode { kind: 0x7F, subtype: 0xFF, length: [4, 0] },
};

#[repr(C)]
pub struct LoadFile2 {
    pub load_file: extern "efiapi" fn(*mut LoadFile2, *const DevicePathNode, bool, *mut usize, *mut c_void) -> Status,
}

#[repr(C)]
pub struct Gop {
    pub query_mode: usize,
    pub set_mode: usize,
    pub blt: usize,
    pub mode: *const GopMode,
}

#[repr(C)]
pub struct GopMode {
    pub max_mode: u32,
    pub mode: u32,
    pub info: *const GopModeInfo,
    pub size_of_info: usize,
    pub frame_buffer_base: u64,
    pub frame_buffer_size: usize,
}

#[repr(C)]
pub struct GopModeInfo {
    pub version: u32,
    pub width: u32,
    pub height: u32,
    /// 0: red-green-blue-reserved bytes, 1: blue-green-red-reserved, 2: bit masks, 3: no framebuffer
    pub pixel_format: u32,
    pub masks: [u32; 4],
    pub pixels_per_scan_line: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct MemoryDescriptor {
    pub kind: u32,
    pub _pad: u32,
    pub physical_start: u64,
    pub virtual_start: u64,
    pub number_of_pages: u64,
    pub attribute: u64,
}

pub const MEM_LOADER_CODE: u32 = 1;
pub const MEM_LOADER_DATA: u32 = 2;
pub const MEM_BOOT_CODE: u32 = 3;
pub const MEM_BOOT_DATA: u32 = 4;
pub const MEM_CONVENTIONAL: u32 = 7;
