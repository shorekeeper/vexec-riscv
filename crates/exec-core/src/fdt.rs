//! Flattened device tree.
//!
//! Reads the blob the firmware leaves in memory. The input is untrusted: a
//! malformed or hostile tree must produce an error, never a panic and never a
//! read outside the blob. Every access here is bounds checked and every
//! failure is a returned error, so the module contains no indexing that can
//! trap and no arithmetic that can overflow into a valid offset.
//!
//! Structure of the format, all integers big endian:
//!
//! ```text
//!   header       fixed 40 bytes, gives the offsets of the blocks below
//!   reservations array of (address, size) pairs, terminated by a zero pair
//!   structure    stream of tokens describing the tree
//!   strings      property names, referenced by offset from the tokens
//! ```
//!
//! Termination of the walk rests on one fact: every iteration reads at least
//! one token and a token is at least four bytes, so the cursor strictly
//! increases and is bounded by the block length. The explicit step limit is
//! redundant given that, and is kept because it turns a defect in the argument
//! into a diagnosable error rather than a hang.

/// Value in the first word of a valid blob.
pub const FDT_MAGIC: u32 = 0xd00d_feed;

/// Size of the header in bytes.
const HEADER_SIZE: usize = 40;

/// Lowest format version this parser accepts.
///
/// Version 16 introduced the token layout used here. Everything since is
/// backward compatible, which is what last_comp_version records.
const MIN_COMPATIBLE_VERSION: u32 = 16;

const TOKEN_BEGIN_NODE: u32 = 1;
const TOKEN_END_NODE: u32 = 2;
const TOKEN_PROP: u32 = 3;
const TOKEN_NOP: u32 = 4;
const TOKEN_END: u32 = 9;

/// Deepest nesting the walk accepts.
///
/// Real trees are four or five deep. The limit exists so the cell stack is a
/// fixed array rather than a recursion that a crafted blob could drive.
pub const MAX_DEPTH: usize = 24;

/// Why a blob could not be read.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FdtError {
    /// The first word is not the magic value.
    BadMagic,
    /// A block extends past the end of the blob.
    Truncated,
    /// The format version is older than this parser accepts.
    BadVersion,
    /// A token, offset or length is not valid.
    Malformed,
    /// Nesting exceeded MAX_DEPTH.
    TooDeep,
    /// The walk ran longer than the structure block can justify.
    Runaway,
    /// The structure block ended without an end token.
    NoEnd,
}

/// Number of cells used for addresses and sizes in a reg property.
///
/// These are declared by a node and apply to its children, so the pair carried
/// with a property event is the one belonging to the parent of the node the
/// property is on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Cells {
    /// Cells per address.
    pub address: u32,
    /// Cells per size.
    pub size: u32,
}

impl Cells {
    /// Values that apply when no ancestor declares them.
    ///
    /// From the specification. Real trees always declare both on the root, so
    /// reaching these means the tree is unusual and the result is likely
    /// wrong; it is still better than refusing to parse.
    pub const DEFAULT: Cells = Cells {
        address: 2,
        size: 1,
    };
}

/// Region listed in the memory reservation block.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Reservation {
    /// First byte.
    pub address: u64,
    /// Length in bytes.
    pub size: u64,
}

/// Bounds checked cursor over a byte slice.
///
/// Every read returns None rather than trapping, and leaves the position
/// unchanged when it fails. That second half is what lets a caller try a read
/// and fall back without having to save and restore the position.
struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(data: &'a [u8]) -> Self {
        Cursor { data, pos: 0 }
    }

    fn at(data: &'a [u8], pos: usize) -> Self {
        Cursor { data, pos }
    }

    /// Reads a big endian word and advances four bytes.
    fn u32(&mut self) -> Option<u32> {
        let end = self.pos.checked_add(4)?;
        let bytes = self.data.get(self.pos..end)?;
        // The slice has exactly four bytes, so the conversion cannot fail.
        let array: [u8; 4] = bytes.try_into().ok()?;
        self.pos = end;
        Some(u32::from_be_bytes(array))
    }

    /// Reads a big endian double word and advances eight bytes.
    fn u64(&mut self) -> Option<u64> {
        let end = self.pos.checked_add(8)?;
        let bytes = self.data.get(self.pos..end)?;
        let array: [u8; 8] = bytes.try_into().ok()?;
        self.pos = end;
        Some(u64::from_be_bytes(array))
    }

    /// Returns the bytes of a null terminated string and advances past the
    /// terminator. The terminator itself is not included.
    fn cstr(&mut self) -> Option<&'a [u8]> {
        let start = self.pos;
        let mut end = start;
        loop {
            let byte = *self.data.get(end)?;
            if byte == 0 {
                break;
            }
            end = end.checked_add(1)?;
        }
        let text = self.data.get(start..end)?;
        // Past the terminator.
        self.pos = end.checked_add(1)?;
        Some(text)
    }

    /// Returns the next `len` bytes and advances past them.
    fn bytes(&mut self, len: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(len)?;
        let slice = self.data.get(self.pos..end)?;
        self.pos = end;
        Some(slice)
    }

    /// Rounds the position up to the next four byte boundary.
    ///
    /// Names and property data are padded to that boundary; the padding is not
    /// part of either.
    fn align4(&mut self) -> Option<()> {
        let aligned = self.pos.checked_add(3)? & !3usize;
        if aligned > self.data.len() {
            return None;
        }
        self.pos = aligned;
        Some(())
    }
}

/// Header of a device tree blob.
#[derive(Clone, Copy, Debug)]
pub struct Header {
    /// Total length of the blob, header included.
    pub total_size: u32,
    /// Offset of the structure block from the start of the blob.
    pub struct_offset: u32,
    /// Offset of the strings block.
    pub strings_offset: u32,
    /// Offset of the memory reservation block.
    pub reserve_offset: u32,
    /// Format version.
    pub version: u32,
    /// Oldest version this blob is compatible with.
    pub last_compatible_version: u32,
    /// Hart the firmware booted on.
    pub boot_cpu: u32,
    /// Length of the strings block.
    pub strings_size: u32,
    /// Length of the structure block.
    pub struct_size: u32,
}

/// A parsed and validated blob.
///
/// Holding one of these means the header is consistent with the slice: every
/// block lies inside it and the structure block is aligned. Nothing beyond
/// that is guaranteed, so the walk still checks every token.
pub struct Fdt<'a> {
    blob: &'a [u8],
    header: Header,
}

impl<'a> Fdt<'a> {
    /// Validates a blob.
    pub fn new(blob: &'a [u8]) -> Result<Fdt<'a>, FdtError> {
        if blob.len() < HEADER_SIZE {
            return Err(FdtError::Truncated);
        }

        let mut cursor = Cursor::new(blob);
        let magic = cursor.u32().ok_or(FdtError::Truncated)?;
        if magic != FDT_MAGIC {
            return Err(FdtError::BadMagic);
        }

        let header = Header {
            total_size: cursor.u32().ok_or(FdtError::Truncated)?,
            struct_offset: cursor.u32().ok_or(FdtError::Truncated)?,
            strings_offset: cursor.u32().ok_or(FdtError::Truncated)?,
            reserve_offset: cursor.u32().ok_or(FdtError::Truncated)?,
            version: cursor.u32().ok_or(FdtError::Truncated)?,
            last_compatible_version: cursor.u32().ok_or(FdtError::Truncated)?,
            boot_cpu: cursor.u32().ok_or(FdtError::Truncated)?,
            strings_size: cursor.u32().ok_or(FdtError::Truncated)?,
            struct_size: cursor.u32().ok_or(FdtError::Truncated)?,
        };

        if header.last_compatible_version > MIN_COMPATIBLE_VERSION {
            return Err(FdtError::BadVersion);
        }

        // The blob may be longer than the tree claims, which happens when the
        // firmware pads it. Shorter is a truncated read.
        let total = header.total_size as usize;
        if total > blob.len() || total < HEADER_SIZE {
            return Err(FdtError::Truncated);
        }

        // Every block must lie inside the region the header declares.
        let struct_end = (header.struct_offset as usize)
            .checked_add(header.struct_size as usize)
            .ok_or(FdtError::Malformed)?;
        let strings_end = (header.strings_offset as usize)
            .checked_add(header.strings_size as usize)
            .ok_or(FdtError::Malformed)?;
        if struct_end > total || strings_end > total {
            return Err(FdtError::Truncated);
        }
        if (header.reserve_offset as usize) > total {
            return Err(FdtError::Truncated);
        }

        // Tokens are word sized and word aligned. An unaligned block would
        // make every offset in the walk wrong.
        if header.struct_offset % 4 != 0 || header.struct_size % 4 != 0 {
            return Err(FdtError::Malformed);
        }

        Ok(Fdt { blob, header })
    }

    /// Header fields.
    pub fn header(&self) -> &Header {
        &self.header
    }

    /// The structure block.
    fn structure(&self) -> Result<&'a [u8], FdtError> {
        let start = self.header.struct_offset as usize;
        let end = start + self.header.struct_size as usize;
        self.blob.get(start..end).ok_or(FdtError::Truncated)
    }

    /// A property name from the strings block.
    fn string_at(&self, offset: u32) -> Result<&'a [u8], FdtError> {
        let start = self.header.strings_offset as usize;
        let end = start + self.header.strings_size as usize;
        let block = self.blob.get(start..end).ok_or(FdtError::Truncated)?;
        let mut cursor = Cursor::at(block, offset as usize);
        cursor.cstr().ok_or(FdtError::Malformed)
    }

    /// Regions the firmware asks to be left alone.
    ///
    /// The list is terminated by a pair of zeros. An entry here means the
    /// memory is in use by something the kernel did not place, and the
    /// allocator must not hand it out.
    pub fn reservations(&self, out: &mut [Reservation]) -> Result<usize, FdtError> {
        let start = self.header.reserve_offset as usize;
        let block = self.blob.get(start..).ok_or(FdtError::Truncated)?;
        let mut cursor = Cursor::new(block);
        let mut count = 0;
        loop {
            let address = cursor.u64().ok_or(FdtError::Truncated)?;
            let size = cursor.u64().ok_or(FdtError::Truncated)?;
            if address == 0 && size == 0 {
                return Ok(count);
            }
            // A full output array stops the scan rather than failing: the
            // entries recorded are still correct, and the caller can tell it
            // is full by comparing the count with the length.
            match out.get_mut(count) {
                Some(slot) => *slot = Reservation { address, size },
                None => return Ok(count),
            }
            count += 1;
        }
    }

    /// Walks the tree, calling the visitor for every node and property.
    pub fn walk(&self, visitor: &mut impl Visitor) -> Result<(), FdtError> {
        let structure = self.structure()?;
        let mut cursor = Cursor::new(structure);

        // cells[d] is the pair that applies to the children of the node at
        // depth d. Index zero holds the specification defaults so that the
        // root itself has something to inherit.
        let mut cells = [Cells::DEFAULT; MAX_DEPTH + 1];
        let mut depth: usize = 0;

        // One token is four bytes at minimum, so this bounds the walk. The
        // cursor also strictly increases, which is the real termination
        // argument; this catches a defect in that argument instead of hanging.
        let max_steps = structure.len() / 4 + 1;
        let mut steps = 0;

        loop {
            steps += 1;
            if steps > max_steps {
                return Err(FdtError::Runaway);
            }

            let token = cursor.u32().ok_or(FdtError::NoEnd)?;
            match token {
                TOKEN_BEGIN_NODE => {
                    let name = cursor.cstr().ok_or(FdtError::Malformed)?;
                    cursor.align4().ok_or(FdtError::Malformed)?;

                    depth = depth.checked_add(1).ok_or(FdtError::TooDeep)?;
                    if depth > MAX_DEPTH {
                        return Err(FdtError::TooDeep);
                    }
                    // Inherit until the node declares its own.
                    cells[depth] = cells[depth - 1];

                    visitor.begin_node(depth, name);
                }

                TOKEN_END_NODE => {
                    if depth == 0 {
                        return Err(FdtError::Malformed);
                    }
                    visitor.end_node(depth);
                    depth -= 1;
                }

                TOKEN_PROP => {
                    if depth == 0 {
                        return Err(FdtError::Malformed);
                    }
                    let len = cursor.u32().ok_or(FdtError::Truncated)? as usize;
                    let name_offset = cursor.u32().ok_or(FdtError::Truncated)?;
                    let value = cursor.bytes(len).ok_or(FdtError::Truncated)?;
                    cursor.align4().ok_or(FdtError::Malformed)?;

                    let name = self.string_at(name_offset)?;

                    // A node's own cell counts govern its children, so they
                    // are recorded before the visitor sees anything that might
                    // depend on them.
                    if name == b"#address-cells" {
                        if let Some(v) = read_u32(value) {
                            cells[depth].address = v;
                        }
                    } else if name == b"#size-cells" {
                        if let Some(v) = read_u32(value) {
                            cells[depth].size = v;
                        }
                    }

                    // The pair passed is the parent's: a reg on this node is
                    // expressed in the units its parent declared.
                    visitor.property(depth, name, value, cells[depth - 1]);
                }

                TOKEN_NOP => {}

                TOKEN_END => {
                    if depth != 0 {
                        return Err(FdtError::Malformed);
                    }
                    return Ok(());
                }

                _ => return Err(FdtError::Malformed),
            }
        }
    }
}

/// Receives events from a walk.
///
/// Default implementations ignore everything, so a visitor states only what it
/// cares about.
pub trait Visitor {
    /// A node opened. Depth one is the root.
    fn begin_node(&mut self, depth: usize, name: &[u8]) {
        let _ = (depth, name);
    }

    /// A property of the node currently open. `parent` holds the cell counts
    /// that apply to a reg property here.
    fn property(&mut self, depth: usize, name: &[u8], value: &[u8], parent: Cells) {
        let _ = (depth, name, value, parent);
    }

    /// The node at this depth closed.
    fn end_node(&mut self, depth: usize) {
        let _ = depth;
    }
}

/// Reads a property holding one word.
pub fn read_u32(value: &[u8]) -> Option<u32> {
    let array: [u8; 4] = value.get(0..4)?.try_into().ok()?;
    Some(u32::from_be_bytes(array))
}

/// Reads a property holding one double word.
pub fn read_u64(value: &[u8]) -> Option<u64> {
    let array: [u8; 8] = value.get(0..8)?.try_into().ok()?;
    Some(u64::from_be_bytes(array))
}

/// Reads `count` cells as a single number and returns it with the bytes used.
///
/// More than two cells is legal and means an address wider than 64 bits. The
/// high cells are folded in and the low 64 bits are kept, which is correct for
/// any machine this kernel can address and detectably wrong for one it cannot.
pub fn read_cells(value: &[u8], count: u32) -> Option<(u64, usize)> {
    let bytes = (count as usize).checked_mul(4)?;
    let field = value.get(0..bytes)?;
    let mut result: u64 = 0;
    let mut offset = 0;
    while offset < bytes {
        let array: [u8; 4] = field.get(offset..offset + 4)?.try_into().ok()?;
        result = (result << 32) | u32::from_be_bytes(array) as u64;
        offset += 4;
    }
    Some((result, bytes))
}

/// True when a null separated string list contains the given entry.
///
/// The compatible property is a list of names in that form. Matching a whole
/// entry rather than a substring matters: "ns16550" must not match
/// "ns16550a" by accident.
pub fn list_contains(value: &[u8], wanted: &[u8]) -> bool {
    for entry in value.split(|&b| b == 0) {
        if entry == wanted {
            return true;
        }
    }
    false
}

/// True when the name before the unit address equals the given text.
///
/// Node names are "name@address". Comparing the part before the at sign is how
/// a node is recognised without knowing where it sits in memory.
pub fn name_is(name: &[u8], wanted: &[u8]) -> bool {
    let base = match name.iter().position(|&b| b == b'@') {
        Some(at) => &name[..at],
        None => name,
    };
    base == wanted
}