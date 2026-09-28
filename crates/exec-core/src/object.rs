//! Object manager.
//!
//! Ties the proved tables to the namespace and to the handle type the rest of
//! the system uses. Plain Rust: everything here is a call into one of the two
//! layers below, and both carry their own guarantees.
//!
//! One table per type, sized at compile time. A single table holding a variant
//! would make every object the size of the largest, and would put a match in
//! front of every access; separate tables cost a type tag in the handle, which
//! is already there.

use exec_abi::{Handle, ObjectType, Status};

use crate::namespace::Namespace;
use crate::objects::ObjectTable;

/// A device object.
///
/// What an I/O request is addressed to. The driver field names the driver that
/// services it, which is how a request reaches code rather than data.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DeviceObject {
    /// Handle of the driver that services this device.
    pub driver: Handle,
    /// Register block, or zero for a device with none.
    pub mmio_base: u64,
    /// Interrupt source, or zero.
    pub irq: u32,
    /// Set by a driver, meaningful only to it.
    pub context: usize,
}

impl DeviceObject {
    /// A device referring to nothing.
    pub const NONE: DeviceObject = DeviceObject {
        driver: exec_abi::HANDLE_NULL,
        mmio_base: 0,
        irq: 0,
        context: 0,
    };
}

/// A loaded driver.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DriverObject {
    /// Interface version the module was built against. A mismatch is refused
    /// rather than adapted: the target has no compiler, so a module is checked
    /// on load and never rebuilt.
    pub abi_version: u32,
    /// Address of the routine that starts the driver.
    pub entry: usize,
    /// Devices this driver services.
    pub device_count: u32,
}

impl DriverObject {
    /// A driver referring to nothing.
    pub const NONE: DriverObject = DriverObject {
        abi_version: 0,
        entry: 0,
        device_count: 0,
    };
}

/// The object manager.
pub struct ObjectManager {
    /// Devices.
    pub devices: ObjectTable<DeviceObject>,
    /// Drivers.
    pub drivers: ObjectTable<DriverObject>,
    /// Names.
    pub names: Namespace,
}

impl ObjectManager {
    /// Empty manager.
    pub const fn new() -> Self {
        ObjectManager {
            devices: ObjectTable::new(ObjectType::Device as u8, DeviceObject::NONE),
            drivers: ObjectTable::new(ObjectType::Driver as u8, DriverObject::NONE),
            names: Namespace::new(),
        }
    }

    /// Creates a device and registers it under a name.
    ///
    /// The object is released if the name cannot be registered, so a failure
    /// leaves nothing behind. Doing it the other way round would leave a name
    /// pointing at a handle that was never issued.
    pub fn create_device(&mut self, path: &[u8], device: DeviceObject) -> Result<Handle, Status> {
        let raw = match self.devices.open(device) {
            Some(raw) => raw,
            None => return Err(Status::NoResources),
        };
        let handle = Handle(raw);

        let status = self.names.insert(path, handle);
        if !status.is_ok() {
            self.devices.release(raw);
            return Err(status);
        }
        Ok(handle)
    }

    /// Returns the device a handle names.
    pub fn device(&self, handle: Handle) -> Option<DeviceObject> {
        self.devices.lookup(handle.0)
    }

    /// Replaces the device a handle names.
    pub fn set_device(&mut self, handle: Handle, device: DeviceObject) -> Status {
        if self.devices.replace(handle.0, device) {
            Status::Success
        } else {
            Status::InvalidHandle
        }
    }

    /// Creates a driver.
    pub fn create_driver(&mut self, path: &[u8], driver: DriverObject) -> Result<Handle, Status> {
        let raw = match self.drivers.open(driver) {
            Some(raw) => raw,
            None => return Err(Status::NoResources),
        };
        let handle = Handle(raw);

        let status = self.names.insert(path, handle);
        if !status.is_ok() {
            self.drivers.release(raw);
            return Err(status);
        }
        Ok(handle)
    }

    /// Returns the driver a handle names.
    pub fn driver(&self, handle: Handle) -> Option<DriverObject> {
        self.drivers.lookup(handle.0)
    }

    /// Returns the handle registered under a path.
    pub fn lookup(&self, path: &[u8]) -> Option<Handle> {
        self.names.lookup(path)
    }

    /// Drops a reference to whatever a handle names.
    ///
    /// The type tag selects the table, so one entry point serves every type and
    /// a handle of an unknown tag is rejected rather than guessed at.
    pub fn release(&mut self, handle: Handle) -> Status {
        let released = if handle.type_tag() == ObjectType::Device as u8 {
            self.devices.release(handle.0)
        } else if handle.type_tag() == ObjectType::Driver as u8 {
            self.drivers.release(handle.0)
        } else {
            return Status::InvalidHandle;
        };

        if released {
            Status::Success
        } else {
            Status::InvalidHandle
        }
    }

    /// Objects held, by type.
    pub fn counts(&self) -> (usize, usize) {
        (self.devices.live_count(), self.drivers.live_count())
    }
}

impl Default for ObjectManager {
    fn default() -> Self {
        Self::new()
    }
}

// The proved table encodes handles itself, standalone, because a verified file
// is checked on its own and cannot import this crate. This confirms the two
// encodings agree, at compile time, so the duplication cannot drift.
const _: () = {
    let mine = Handle::new(0x1234, 0x5678, ObjectType::Device).0;
    let theirs = 0x1234u64 | (0x5678u64 << 16) | ((ObjectType::Device as u64) << 32);
    assert!(mine == theirs);
};