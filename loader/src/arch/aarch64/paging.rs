// Copyright (c) 2018 Colin Finck, RWTH Aachen University
//
// MIT License
//
// Permission is hereby granted, free of charge, to any person obtaining
// a copy of this software and associated documentation files (the
// "Software"), to deal in the Software without restriction, including
// without limitation the rights to use, copy, modify, merge, publish,
// distribute, sublicense, and/or sell copies of the Software, and to
// permit persons to whom the Software is furnished to do so, subject to
// the following conditions:
//
// The above copyright notice and this permission notice shall be
// included in all copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND,
// EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
// MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND
// NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE
// LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION
// OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION
// WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.

use core::marker::PhantomData;
use core::usize;
use physicalmem;


/// Pointer to the root page table (called "Level 0" in ARM terminology).
/// Setting the upper bits to zero tells the MMU to use TTBR0 for the base address for the first table.
///
/// See entry.S and ARM Cortex-A Series Programmer's Guide for ARMv8-A, Version 1.0, PDF page 172
const L0TABLE_ADDRESS: *mut PageTable<L0Table> = 0x0000_FFFF_FFFF_F000 as *mut PageTable<L0Table>;

/// Number of Offset bits of a virtual address for a 4 KiB page, which are shifted away to get its Page Frame Number (PFN).
const PAGE_BITS: usize = 12;

/// Number of bits of the index in each table (L0Table, L1Table, L2Table, L3Table).
const PAGE_MAP_BITS: usize = 9;

/// A mask where PAGE_MAP_BITS are set to calculate a table index.
const PAGE_MAP_MASK: usize = 0x1FF;


bitflags! {
	/// Useful flags for an entry in either table (L0Table, L1Table, L2Table, L3Table).
	///
	/// See ARM Architecture Reference Manual, ARMv8, for ARMv8-A Reference Profile, Issue C.a, Chapter D4.3.3
	pub struct PageTableEntryFlags: usize {
		/// Set if this entry is valid.
		const PRESENT = 1 << 0;

		/// Set if this entry points to a table or a 4 KiB page.
		const TABLE_OR_4KIB_PAGE = 1 << 1;

		/// Set if this entry points to device memory (non-gathering, non-reordering, no early write acknowledgement)
		const DEVICE_NGNRNE = 0 << 4 | 0 << 3 | 0 << 2;

		/// Set if this entry points to device memory (non-gathering, non-reordering, early write acknowledgement)
		const DEVICE_NGNRE = 0 << 4 | 0 << 3 | 1 << 2;

		/// Set if this entry points to device memory (gathering, reordering, early write acknowledgement)
		const DEVICE_GRE = 0 << 4 | 1 << 3 | 0 << 2;

		/// Set if this entry points to normal memory (non-cacheable)
		const NORMAL_NC = 0 << 4 | 1 << 3 | 1 << 2;

		/// Set if this entry points to normal memory (cacheable)
		const NORMAL = 1 << 4 | 0 << 3 | 0 << 2;

		/// Set if memory referenced by this entry shall be read-only.
		const READ_ONLY = 1 << 7;

		/// Set if this entry shall be shared between all cores of the system.
		const INNER_SHAREABLE = 1 << 8 | 1 << 9;

		/// Set if software has accessed this entry (for memory access or address translation).
		const ACCESSED = 1 << 10;

		/// Set if code execution shall be disabled for memory referenced by this entry in privileged mode.
		const PRIVILEGED_EXECUTE_NEVER = 1 << 53;

		/// Set if code execution shall be disabled for memory referenced by this entry in unprivileged mode.
		const UNPRIVILEGED_EXECUTE_NEVER = 1 << 54;
    }
}

impl PageTableEntryFlags {
	/// An empty set of flags for unused/zeroed table entries.
	/// Needed as long as empty() is no const function.
	const BLANK: PageTableEntryFlags = PageTableEntryFlags { bits: 0 };

	pub fn device(&mut self) -> &mut Self {
		self.insert(PageTableEntryFlags::DEVICE_NGNRE);
		self
	}

	pub fn normal(&mut self) -> &mut Self {
		self.insert(PageTableEntryFlags::NORMAL);
		self
	}

	pub fn read_only(&mut self) -> &mut Self {
		self.insert(PageTableEntryFlags::READ_ONLY);
		self
	}

	pub fn writable(&mut self) -> &mut Self {
		self.remove(PageTableEntryFlags::READ_ONLY);
		self
	}

	pub fn execute_disable(&mut self) -> &mut Self {
		self.insert(PageTableEntryFlags::PRIVILEGED_EXECUTE_NEVER);
		self.insert(PageTableEntryFlags::UNPRIVILEGED_EXECUTE_NEVER);
		self
	}
}


/// An entry in either table
#[derive(Clone, Copy)]
pub struct PageTableEntry {
	/// Physical memory address this entry refers, combined with flags from PageTableEntryFlags.
	physical_address_and_flags: usize
}

impl PageTableEntry {
	/// Return the stored physical address.
	pub fn address(&self) -> usize {
		self.physical_address_and_flags & !(BasePageSize::SIZE - 1) & !(usize::MAX << 48)
	}

	/// Returns whether this entry is valid (present).
	fn is_present(&self) -> bool {
		(self.physical_address_and_flags & PageTableEntryFlags::PRESENT.bits()) != 0
	}

	/// Mark this as a valid (present) entry and set address translation and flags.
	///
	/// # Arguments
	///
	/// * `physical_address` - The physical memory address this entry shall translate to
	/// * `flags` - Flags from PageTableEntryFlags (note that the PRESENT, INNER_SHAREABLE, and ACCESSED flags are set automatically)
	fn set(&mut self, physical_address: usize, flags: PageTableEntryFlags) {
		// Verify that the offset bits for a 4 KiB page are zero.
		assert!(physical_address % BasePageSize::SIZE == 0, "Physical address is not on a 4 KiB page boundary (physical_address = {:#X})", physical_address);

		let mut flags_to_set = flags;
		flags_to_set.insert(PageTableEntryFlags::PRESENT);
		flags_to_set.insert(PageTableEntryFlags::INNER_SHAREABLE);
		flags_to_set.insert(PageTableEntryFlags::ACCESSED);
		self.physical_address_and_flags = physical_address | flags_to_set.bits();
	}
}

/// A generic interface to support all possible page sizes.
///
/// This is defined as a subtrait of Copy to enable #[derive(Clone, Copy)] for Page.
/// Currently, deriving implementations for these traits only works if all dependent types implement it as well.
pub trait PageSize: Copy {
	/// The page size in bytes.
	const SIZE: usize;

	/// The page table level at which a page of this size is mapped
	const MAP_LEVEL: usize;

	/// Any extra flag that needs to be set to map a page of this size.
	/// For example: PageTableEntryFlags::TABLE_OR_4KIB_PAGE.
	const MAP_EXTRA_FLAG: PageTableEntryFlags;
}

/// A 4 KiB page mapped in the L3Table.
#[derive(Clone, Copy)]
pub enum BasePageSize {}
impl PageSize for BasePageSize {
	const SIZE: usize = 4096;
	const MAP_LEVEL: usize = 3;
	const MAP_EXTRA_FLAG: PageTableEntryFlags = PageTableEntryFlags::TABLE_OR_4KIB_PAGE;
}

/// A 2 MiB page mapped in the L2Table.
#[derive(Clone, Copy)]
pub enum LargePageSize {}
impl PageSize for LargePageSize {
	const SIZE: usize = 2 * 1024 * 1024;
	const MAP_LEVEL: usize = 2;
	const MAP_EXTRA_FLAG: PageTableEntryFlags = PageTableEntryFlags::BLANK;
}

/// A 1 GiB page mapped in the L1Table.
#[derive(Clone, Copy)]
pub enum HugePageSize {}
impl PageSize for HugePageSize {
	const SIZE: usize = 1024 * 1024 * 1024;
	const MAP_LEVEL: usize = 1;
	const MAP_EXTRA_FLAG: PageTableEntryFlags = PageTableEntryFlags::BLANK;
}

/// A memory page of the size given by S.
#[derive(Clone, Copy)]
struct Page<S: PageSize> {
	/// Virtual memory address of this page.
	/// This is rounded to a page size boundary on creation.
	virtual_address: usize,

	/// Required by Rust to support the S parameter.
	size: PhantomData<S>,
}

impl<S: PageSize> Page<S> {
	/// Return the stored virtual address.
	fn address(&self) -> usize {
		self.virtual_address
	}

	/// Flushes this page from the TLB of this CPU.
	fn flush_from_tlb(&self) {
		// See ARM Cortex-A Series Programmer's Guide for ARMv8-A, Version 1.0, PDF page 198
		//
		// We use "vale1is" instead of "vae1is" to always flush the last table level only (performance optimization).
		// The "is" attribute broadcasts the TLB flush to all cores, so we don't need an IPI (unlike x86_64).
		unsafe { asm!("dsb ishst; tlbi vale1is, $0; dsb ish; isb" :: "r"(self.virtual_address) : "memory" : "volatile"); }
	}

	/// Returns whether the given virtual address is a valid one in the AArch64 memory model.
	///
	/// Current AArch64 supports only 48-bit for virtual memory addresses.
	/// The upper bits must always be 0 or 1 and indicate whether TBBR0 or TBBR1 contains the
	/// base address. So always enforce 0 here.
	fn is_valid_address(virtual_address: usize) -> bool {
		virtual_address < 0x1_0000_0000_0000
	}

	/// Returns a Page including the given virtual address.
	/// That means, the address is rounded down to a page size boundary.
	fn including_address(virtual_address: usize) -> Self {
		assert!(Self::is_valid_address(virtual_address), "Virtual address {:#X} is invalid", virtual_address);

		Self {
			virtual_address: align_down!(virtual_address, S::SIZE),
			size: PhantomData,
		}
	}

	/// Returns a PageIter to iterate from the given first Page to the given last Page (inclusive).
	fn range(first: Self, last: Self) -> PageIter<S> {
		assert!(first.virtual_address <= last.virtual_address);
		PageIter { current: first, last: last }
	}

	/// Returns the index of this page in the table given by L.
	fn table_index<L: PageTableLevel>(&self) -> usize {
		assert!(L::LEVEL <= S::MAP_LEVEL);
		self.virtual_address >> PAGE_BITS >> (3 - L::LEVEL) * PAGE_MAP_BITS & PAGE_MAP_MASK
	}
}

/// An iterator to walk through a range of pages of size S.
struct PageIter<S: PageSize> {
	current: Page<S>,
	last: Page<S>,
}

impl<S: PageSize> Iterator for PageIter<S> {
	type Item = Page<S>;

	fn next(&mut self) -> Option<Page<S>> {
		if self.current.virtual_address <= self.last.virtual_address {
			let p = self.current;
			self.current.virtual_address += S::SIZE;
			Some(p)
		} else {
			None
		}
	}
}

/// An interface to allow for a generic implementation of struct PageTable for all 4 page tables.
/// Must be implemented by all page tables.
trait PageTableLevel {
	/// Numeric page table level
	const LEVEL: usize;
}

/// An interface for page tables with sub page tables (all except L3Table).
/// Having both PageTableLevel and PageTableLevelWithSubtables leverages Rust's typing system to provide
/// a subtable method only for those that have sub page tables.
///
/// Kudos to Philipp Oppermann for the trick!
trait PageTableLevelWithSubtables: PageTableLevel {
	type SubtableLevel;
}

/// The Level 0 Table
enum L0Table {}
impl PageTableLevel for L0Table {
	const LEVEL: usize = 0;
}

impl PageTableLevelWithSubtables for L0Table {
	type SubtableLevel = L1Table;
}

/// The Level 1 Table (can map 1 GiB pages)
enum L1Table {}
impl PageTableLevel for L1Table {
	const LEVEL: usize = 1;
}

impl PageTableLevelWithSubtables for L1Table {
	type SubtableLevel = L2Table;
}

/// The Level 2 Table (can map 2 MiB pages)
enum L2Table {}
impl PageTableLevel for L2Table {
	const LEVEL: usize = 2;
}

impl PageTableLevelWithSubtables for L2Table {
	type SubtableLevel = L3Table;
}

/// The Level 3 Table (can map 4 KiB pages)
enum L3Table {}
impl PageTableLevel for L3Table {
	const LEVEL: usize = 3;
}

/// Representation of any page table in memory.
/// Parameter L supplies information for Rust's typing system to distinguish between the different tables.
struct PageTable<L> {
	/// Each page table has 512 entries (can be calculated using PAGE_MAP_BITS).
	entries: [PageTableEntry; 1 << PAGE_MAP_BITS],

	/// Required by Rust to support the L parameter.
	level: PhantomData<L>,
}

/// A trait defining methods every page table has to implement.
/// This additional trait is necessary to make use of Rust's specialization feature and provide a default
/// implementation of some methods.
trait PageTableMethods {
	fn map_page_in_this_table<S: PageSize>(&mut self, page: Page<S>, physical_address: usize, flags: PageTableEntryFlags);
	fn map_page<S: PageSize>(&mut self, page: Page<S>, physical_address: usize, flags: PageTableEntryFlags);
}

impl<L: PageTableLevel> PageTableMethods for PageTable<L> {
	/// Maps a single page in this table to the given physical address.
	///
	/// Must only be called if a page of this size is mapped at this page table level!
	fn map_page_in_this_table<S: PageSize>(&mut self, page: Page<S>, physical_address: usize, flags: PageTableEntryFlags) {
		assert!(L::LEVEL == S::MAP_LEVEL);
		let index = page.table_index::<L>();
		let flush = self.entries[index].is_present();

		self.entries[index].set(physical_address, S::MAP_EXTRA_FLAG | flags);

		if flush {
			page.flush_from_tlb();
		}
	}

	/// Maps a single page to the given physical address.
	///
	/// This is the default implementation that just calls the map_page_in_this_table method.
	/// It is overridden by a specialized implementation for all tables with sub tables (all except L3Table).
	default fn map_page<S: PageSize>(&mut self, page: Page<S>, physical_address: usize, flags: PageTableEntryFlags) {
		self.map_page_in_this_table::<S>(page, physical_address, flags)
	}
}

impl<L: PageTableLevelWithSubtables> PageTableMethods for PageTable<L> where L::SubtableLevel: PageTableLevel {
	/// Maps a single page to the given physical address.
	///
	/// This is the implementation for all tables with subtables (L0Table, L1Table, L2Table).
	/// It overrides the default implementation above.
	fn map_page<S: PageSize>(&mut self, page: Page<S>, physical_address: usize, flags: PageTableEntryFlags) {
		assert!(L::LEVEL <= S::MAP_LEVEL);

		if L::LEVEL < S::MAP_LEVEL {
			let index = page.table_index::<L>();

			// Does the table exist yet?
			if !self.entries[index].is_present() {
				// Allocate a single 4 KiB page for the new entry and mark it as a valid, writable subtable.
				let physical_address = physicalmem::allocate(BasePageSize::SIZE);
				self.entries[index].set(physical_address, PageTableEntryFlags::NORMAL | PageTableEntryFlags::TABLE_OR_4KIB_PAGE);

				// Mark all entries as unused in the newly created table.
				let subtable = self.subtable::<S>(page);
				for entry in subtable.entries.iter_mut() {
					entry.physical_address_and_flags = 0;
				}
			}

			let subtable = self.subtable::<S>(page);
			subtable.map_page::<S>(page, physical_address, flags)
		} else {
			// Calling the default implementation from a specialized one is not supported (yet),
			// so we have to resort to an extra function.
			self.map_page_in_this_table::<S>(page, physical_address, flags)
		}
	}
}

impl<L: PageTableLevelWithSubtables> PageTable<L> where L::SubtableLevel: PageTableLevel {
	/// Returns the next subtable for the given page in the page table hierarchy.
	///
	/// Must only be called if a page of this size is mapped in a subtable!
	fn subtable<S: PageSize>(&self, page: Page<S>) -> &mut PageTable<L::SubtableLevel> {
		assert!(L::LEVEL < S::MAP_LEVEL);

		// Calculate the address of the subtable.
		let index = page.table_index::<L>();
		let table_address = self as *const PageTable<L> as usize;
		let subtable_address = (table_address << PAGE_MAP_BITS) & !(usize::MAX << 48) | (index << PAGE_BITS);
		unsafe { &mut *(subtable_address as *mut PageTable<L::SubtableLevel>) }
	}

	/// Maps a continuous range of pages.
	///
	/// # Arguments
	///
	/// * `range` - The range of pages of size S
	/// * `physical_address` - First physical address to map these pages to
	/// * `flags` - Flags from PageTableEntryFlags to set for the page table entry (e.g. WRITABLE or EXECUTE_DISABLE).
	///             The PRESENT and ACCESSED are already set automatically.
	fn map_pages<S: PageSize>(&mut self, range: PageIter<S>, physical_address: usize, flags: PageTableEntryFlags) {
		let mut current_physical_address = physical_address;

		for page in range {
			self.map_page::<S>(page, current_physical_address, flags);
			current_physical_address += S::SIZE;
		}
	}
}

#[inline]
fn get_page_range<S: PageSize>(virtual_address: usize, count: usize) -> PageIter<S> {
	let first_page = Page::<S>::including_address(virtual_address);
	let last_page = Page::<S>::including_address(virtual_address + (count - 1) * S::SIZE);
	Page::range(first_page, last_page)
}

pub fn map<S: PageSize>(virtual_address: usize, physical_address: usize, count: usize, flags: PageTableEntryFlags) {
	let range = get_page_range::<S>(virtual_address, count);
	let root_pagetable = unsafe { &mut *L0TABLE_ADDRESS };
	root_pagetable.map_pages(range, physical_address, flags);
}
