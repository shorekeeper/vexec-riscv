//! Object namespace.
//!
//! Names for handles, in the form the NT object manager uses: a path such as
//! \Device\Serial0, resolved one component at a time. A flat table of entries
//! with a parent index rather than a tree, so a lookup is a bounded scan and
//! there is no structure to prove things about.
//!
//! Plain Rust and not verified. Every bound here is checked and every failure
//! returns, but the interesting property of a namespace is that a name resolves
//! to what was registered under it, and stating that needs a model of strings
//! that costs more than it is worth. The handles the names produce carry their
//! own proof.
//!
//! Names are interned into one byte pool. An entry holds an offset and a
//! length, so a name costs no allocation and comparing two is comparing two
//! slices.

use exec_abi::{Handle, Status};

/// Entries the namespace holds.
pub const MAX_ENTRIES: usize = 64;
/// Bytes of interned name text.
pub const NAME_POOL: usize = 1024;
/// Longest single component.
pub const MAX_COMPONENT: usize = 32;
/// Deepest path the resolver walks.
pub const MAX_DEPTH: usize = 8;

/// Separator between path components.
pub const SEPARATOR: u8 = b'\\';

/// Index meaning no parent, that is a name directly under the root.
const ROOT: u16 = u16::MAX;

/// One name.
#[derive(Clone, Copy)]
struct Entry {
    /// Offset into the pool, and length. Zero length means the slot is free.
    offset: u16,
    length: u16,
    /// Index of the containing entry, or ROOT.
    parent: u16,
    /// What the name refers to. Null for a directory, which exists to hold
    /// other entries and refers to nothing itself.
    handle: Handle,
}

impl Entry {
    const EMPTY: Entry = Entry {
        offset: 0,
        length: 0,
        parent: ROOT,
        handle: exec_abi::HANDLE_NULL,
    };
}

/// Names and what they refer to.
pub struct Namespace {
    entries: [Entry; MAX_ENTRIES],
    pool: [u8; NAME_POOL],
    used: usize,
    count: usize,
}

impl Namespace {
    /// Empty namespace.
    pub const fn new() -> Self {
        Namespace {
            entries: [Entry::EMPTY; MAX_ENTRIES],
            pool: [0; NAME_POOL],
            used: 0,
            count: 0,
        }
    }

    /// Names registered.
    pub const fn len(&self) -> usize {
        self.count
    }

    /// True when nothing is registered.
    pub const fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Text of an entry.
    fn text(&self, index: usize) -> &[u8] {
        let entry = self.entries[index];
        let start = entry.offset as usize;
        let end = start + entry.length as usize;
        match self.pool.get(start..end) {
            Some(slice) => slice,
            // Cannot happen while every entry was written by intern, which
            // bounds both. An empty slice is a name nothing matches.
            None => &[],
        }
    }

    /// Copies a name into the pool and returns its span.
    fn intern(&mut self, name: &[u8]) -> Option<(u16, u16)> {
        if name.is_empty() || name.len() > MAX_COMPONENT {
            return None;
        }
        let start = self.used;
        let end = start.checked_add(name.len())?;
        if end > NAME_POOL {
            return None;
        }
        let mut i = 0;
        while i < name.len() {
            self.pool[start + i] = name[i];
            i += 1;
        }
        self.used = end;
        Some((start as u16, name.len() as u16))
    }

    /// Finds a direct child of a parent by name.
    fn child_of(&self, parent: u16, name: &[u8]) -> Option<usize> {
        let mut i = 0;
        while i < MAX_ENTRIES {
            if self.entries[i].length != 0
                && self.entries[i].parent == parent
                && self.text(i) == name
            {
                return Some(i);
            }
            i += 1;
        }
        None
    }

    /// Adds a name under a parent.
    ///
    /// The name is interned even when the entry table is full, which wastes
    /// pool space on a failure. Checking the table first would avoid it; the
    /// order is this way because a full table is a configuration fault that
    /// stops the machine early, and the wasted bytes are never reclaimed by
    /// anything that survives it.
    fn add(&mut self, parent: u16, name: &[u8], handle: Handle) -> Result<usize, Status> {
        if self.child_of(parent, name).is_some() {
            return Err(Status::Exists);
        }

        let mut slot = MAX_ENTRIES;
        let mut i = 0;
        while i < MAX_ENTRIES {
            if self.entries[i].length == 0 {
                slot = i;
                break;
            }
            i += 1;
        }
        if slot == MAX_ENTRIES {
            return Err(Status::NoResources);
        }

        let (offset, length) = match self.intern(name) {
            Some(span) => span,
            None => return Err(Status::NoResources),
        };

        self.entries[slot] = Entry {
            offset,
            length,
            parent,
            handle,
        };
        self.count += 1;
        Ok(slot)
    }

    /// Splits a path into components.
    ///
    /// A leading separator is optional and a trailing one is ignored, so
    /// \Device\Serial0 and Device\Serial0 name the same thing. An empty
    /// component is a malformed path rather than something to skip: two
    /// separators together usually mean a name was concatenated wrongly.
    fn split<'a>(path: &'a [u8], out: &mut [&'a [u8]; MAX_DEPTH]) -> Result<usize, Status> {
        let mut count = 0;
        let mut start = 0;

        if !path.is_empty() && path[0] == SEPARATOR {
            start = 1;
        }

        let mut i = start;
        while i <= path.len() {
            let at_end = i == path.len();
            if at_end || path[i] == SEPARATOR {
                if i > start {
                    if count >= MAX_DEPTH {
                        return Err(Status::InvalidArgument);
                    }
                    match path.get(start..i) {
                        Some(component) => out[count] = component,
                        None => return Err(Status::InvalidArgument),
                    }
                    count += 1;
                } else if !at_end {
                    // Two separators together, or a component of no length.
                    return Err(Status::InvalidArgument);
                }
                start = i + 1;
            }
            i += 1;
        }

        if count == 0 {
            return Err(Status::InvalidArgument);
        }
        Ok(count)
    }

    /// Registers a handle under a path.
    ///
    /// Intermediate components are created as directories if absent, which is
    /// what makes registering \Device\Serial0 work without \Device having been
    /// declared first. A component that exists and refers to something is
    /// reused as a parent, so a path may pass through a named object.
    pub fn insert(&mut self, path: &[u8], handle: Handle) -> Status {
        let mut parts = [&[] as &[u8]; MAX_DEPTH];
        let count = match Self::split(path, &mut parts) {
            Ok(count) => count,
            Err(status) => return status,
        };

        let mut parent = ROOT;
        let mut i = 0;
        while i + 1 < count {
            let index = match self.child_of(parent, parts[i]) {
                Some(index) => index,
                None => match self.add(parent, parts[i], exec_abi::HANDLE_NULL) {
                    Ok(index) => index,
                    Err(status) => return status,
                },
            };
            parent = index as u16;
            i += 1;
        }

        match self.add(parent, parts[count - 1], handle) {
            Ok(_) => Status::Success,
            Err(status) => status,
        }
    }

    /// Returns the handle registered under a path.
    pub fn lookup(&self, path: &[u8]) -> Option<Handle> {
        let mut parts = [&[] as &[u8]; MAX_DEPTH];
        let count = Self::split(path, &mut parts).ok()?;

        let mut parent = ROOT;
        let mut i = 0;
        while i < count {
            let index = self.child_of(parent, parts[i])?;
            if i + 1 == count {
                let handle = self.entries[index].handle;
                if handle.is_null() {
                    // A directory. Naming one is not an error, but it refers to
                    // nothing, so there is nothing to return.
                    return None;
                }
                return Some(handle);
            }
            parent = index as u16;
            i += 1;
        }
        None
    }

    /// Removes a name.
    ///
    /// The entry is freed and its pool bytes are not. Reclaiming them needs
    /// compaction, which moves every offset; names are registered at boot and
    /// removed when a device goes away, so the pool is sized for the total
    /// rather than the peak.
    pub fn remove(&mut self, path: &[u8]) -> Status {
        let mut parts = [&[] as &[u8]; MAX_DEPTH];
        let count = match Self::split(path, &mut parts) {
            Ok(count) => count,
            Err(status) => return status,
        };

        let mut parent = ROOT;
        let mut i = 0;
        while i < count {
            let index = match self.child_of(parent, parts[i]) {
                Some(index) => index,
                None => return Status::NotFound,
            };
            if i + 1 == count {
                // A directory with children is not removed: the children would
                // become unreachable while still occupying entries.
                if self.child_of(index as u16, &[]).is_some() || self.has_children(index as u16) {
                    return Status::AccessDenied;
                }
                self.entries[index] = Entry::EMPTY;
                self.count -= 1;
                return Status::Success;
            }
            parent = index as u16;
            i += 1;
        }
        Status::NotFound
    }

    /// True when anything names this entry as its parent.
    fn has_children(&self, parent: u16) -> bool {
        let mut i = 0;
        while i < MAX_ENTRIES {
            if self.entries[i].length != 0 && self.entries[i].parent == parent {
                return true;
            }
            i += 1;
        }
        false
    }

    /// Calls the visitor for every registered name.
    ///
    /// The path is rebuilt into the caller's buffer, walking up through the
    /// parents and reversing. The depth bound is what makes the walk terminate
    /// on a table that somehow contains a cycle.
    pub fn for_each(&self, visitor: &mut impl FnMut(&[u8], Handle)) {
        let mut buffer = [0u8; MAX_DEPTH * (MAX_COMPONENT + 1)];

        let mut i = 0;
        while i < MAX_ENTRIES {
            if self.entries[i].length == 0 {
                i += 1;
                continue;
            }

            // Collect the chain from the entry up to the root.
            let mut chain = [0usize; MAX_DEPTH];
            let mut depth = 0;
            let mut current = i;
            loop {
                if depth >= MAX_DEPTH {
                    break;
                }
                chain[depth] = current;
                depth += 1;
                let parent = self.entries[current].parent;
                if parent == ROOT || parent as usize >= MAX_ENTRIES {
                    break;
                }
                current = parent as usize;
            }

            // Write it out, outermost first.
            let mut length = 0;
            let mut level = depth;
            while level > 0 {
                level -= 1;
                if length < buffer.len() {
                    buffer[length] = SEPARATOR;
                    length += 1;
                }
                let text = self.text(chain[level]);
                let mut k = 0;
                while k < text.len() && length < buffer.len() {
                    buffer[length] = text[k];
                    length += 1;
                    k += 1;
                }
            }

            visitor(&buffer[..length], self.entries[i].handle);
            i += 1;
        }
    }
}

impl Default for Namespace {
    fn default() -> Self {
        Self::new()
    }
}