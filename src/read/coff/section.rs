use core::{iter, result, slice, str};

use crate::endian::LittleEndian as LE;
use crate::pe;
use crate::read::util::StringTable;
use crate::read::{
    self, CompressedData, CompressedFileRange, Error, ObjectSection, ObjectSegment, ReadError,
    ReadRef, Result, SectionFlags, SectionIndex, SectionKind,
};

use super::{CoffFile, CoffRelocationIterator};

/// The table of section headers in a COFF or PE file.
#[derive(Debug, Default, Clone, Copy)]
pub struct SectionTable<'data> {
    sections: &'data [pe::ImageSectionHeader],
}

impl<'data> SectionTable<'data> {
    /// Parse the section table.
    ///
    /// `data` must be the entire file data.
    /// `offset` must be after the optional file header.
    pub fn parse<R: ReadRef<'data>>(
        header: &pe::ImageFileHeader,
        data: R,
        offset: u64,
    ) -> Result<Self> {
        let sections = data
            .read_slice_at(offset, header.number_of_sections.get(LE).into())
            .read_error("Invalid COFF/PE section headers")?;
        Ok(SectionTable { sections })
    }

    /// Iterate over the section headers.
    ///
    /// Warning: sections indices start at 1.
    #[inline]
    pub fn iter(&self) -> slice::Iter<'data, pe::ImageSectionHeader> {
        self.sections.iter()
    }

    /// Return true if the section table is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.sections.is_empty()
    }

    /// The number of section headers.
    #[inline]
    pub fn len(&self) -> usize {
        self.sections.len()
    }

    /// Return the section header at the given index.
    ///
    /// The index is 1-based.
    pub fn section(&self, index: usize) -> read::Result<&'data pe::ImageSectionHeader> {
        self.sections
            .get(index.wrapping_sub(1))
            .read_error("Invalid COFF/PE section index")
    }

    /// Return the section header with the given name.
    ///
    /// The returned index is 1-based.
    ///
    /// Ignores sections with invalid names.
    pub fn section_by_name(
        &self,
        strings: StringTable<'data>,
        name: &[u8],
    ) -> Option<(usize, &'data pe::ImageSectionHeader)> {
        self.sections
            .iter()
            .enumerate()
            .find(|(_, section)| section.name(strings) == Ok(name))
            .map(|(index, section)| (index + 1, section))
    }
}

/// An iterator over the loadable sections of a `CoffFile`.
#[derive(Debug)]
pub struct CoffSegmentIterator<'data, 'file, R: ReadRef<'data> = &'data [u8]> {
    pub(super) file: &'file CoffFile<'data, R>,
    pub(super) iter: slice::Iter<'data, pe::ImageSectionHeader>,
}

impl<'data, 'file, R: ReadRef<'data>> Iterator for CoffSegmentIterator<'data, 'file, R> {
    type Item = CoffSegment<'data, 'file, R>;

    fn next(&mut self) -> Option<Self::Item> {
        self.iter.next().map(|section| CoffSegment {
            file: self.file,
            section,
        })
    }
}

/// A loadable section of a `CoffFile`.
#[derive(Debug)]
pub struct CoffSegment<'data, 'file, R: ReadRef<'data> = &'data [u8]> {
    pub(super) file: &'file CoffFile<'data, R>,
    pub(super) section: &'data pe::ImageSectionHeader,
}

impl<'data, 'file, R: ReadRef<'data>> CoffSegment<'data, 'file, R> {
    fn bytes(&self) -> Result<&'data [u8]> {
        self.section
            .coff_data(self.file.data)
            .read_error("Invalid COFF section offset or size")
    }
}

impl<'data, 'file, R: ReadRef<'data>> read::private::Sealed for CoffSegment<'data, 'file, R> {}

impl<'data, 'file, R: ReadRef<'data>> ObjectSegment<'data> for CoffSegment<'data, 'file, R> {
    #[inline]
    fn address(&self) -> u64 {
        u64::from(self.section.virtual_address.get(LE))
    }

    #[inline]
    fn size(&self) -> u64 {
        u64::from(self.section.virtual_size.get(LE))
    }

    #[inline]
    fn align(&self) -> u64 {
        self.section.coff_alignment()
    }

    #[inline]
    fn file_range(&self) -> (u64, u64) {
        let (offset, size) = self.section.coff_file_range().unwrap_or((0, 0));
        (u64::from(offset), u64::from(size))
    }

    fn data(&self) -> Result<&'data [u8]> {
        Ok(self.bytes()?)
    }

    fn data_range(&self, address: u64, size: u64) -> Result<Option<&'data [u8]>> {
        Ok(read::util::data_range(
            self.bytes()?,
            self.address(),
            address,
            size,
        ))
    }

    #[inline]
    fn name(&self) -> Result<Option<&str>> {
        let name = self.section.name(self.file.common.symbols.strings())?;
        Ok(Some(
            str::from_utf8(name)
                .ok()
                .read_error("Non UTF-8 COFF section name")?,
        ))
    }
}

/// An iterator over the sections of a `CoffFile`.
#[derive(Debug)]
pub struct CoffSectionIterator<'data, 'file, R: ReadRef<'data> = &'data [u8]> {
    pub(super) file: &'file CoffFile<'data, R>,
    pub(super) iter: iter::Enumerate<slice::Iter<'data, pe::ImageSectionHeader>>,
}

impl<'data, 'file, R: ReadRef<'data>> Iterator for CoffSectionIterator<'data, 'file, R> {
    type Item = CoffSection<'data, 'file, R>;

    fn next(&mut self) -> Option<Self::Item> {
        self.iter.next().map(|(index, section)| CoffSection {
            file: self.file,
            index: SectionIndex(index + 1),
            section,
        })
    }
}

/// A section of a `CoffFile`.
#[derive(Debug)]
pub struct CoffSection<'data, 'file, R: ReadRef<'data> = &'data [u8]> {
    pub(super) file: &'file CoffFile<'data, R>,
    pub(super) index: SectionIndex,
    pub(super) section: &'data pe::ImageSectionHeader,
}

impl<'data, 'file, R: ReadRef<'data>> CoffSection<'data, 'file, R> {
    fn bytes(&self) -> Result<&'data [u8]> {
        self.section
            .coff_data(self.file.data)
            .read_error("Invalid COFF section offset or size")
    }
}

impl<'data, 'file, R: ReadRef<'data>> read::private::Sealed for CoffSection<'data, 'file, R> {}

impl<'data, 'file, R: ReadRef<'data>> ObjectSection<'data> for CoffSection<'data, 'file, R> {
    type RelocationIterator = CoffRelocationIterator<'data, 'file, R>;

    #[inline]
    fn index(&self) -> SectionIndex {
        self.index
    }

    #[inline]
    fn address(&self) -> u64 {
        u64::from(self.section.virtual_address.get(LE))
    }

    #[inline]
    fn size(&self) -> u64 {
        // TODO: This may need to be the length from the auxiliary symbol for this section.
        u64::from(self.section.size_of_raw_data.get(LE))
    }

    #[inline]
    fn align(&self) -> u64 {
        self.section.coff_alignment()
    }

    #[inline]
    fn file_range(&self) -> Option<(u64, u64)> {
        let (offset, size) = self.section.coff_file_range()?;
        Some((u64::from(offset), u64::from(size)))
    }

    fn data(&self) -> Result<&'data [u8]> {
        Ok(self.bytes()?)
    }

    fn data_range(&self, address: u64, size: u64) -> Result<Option<&'data [u8]>> {
        Ok(read::util::data_range(
            self.bytes()?,
            self.address(),
            address,
            size,
        ))
    }

    #[inline]
    fn compressed_file_range(&self) -> Result<CompressedFileRange> {
        Ok(CompressedFileRange::none(self.file_range()))
    }

    #[inline]
    fn compressed_data(&self) -> Result<CompressedData<'data>> {
        self.data().map(CompressedData::none)
    }

    #[inline]
    fn name(&self) -> Result<&str> {
        let name = self.section.name(self.file.common.symbols.strings())?;
        str::from_utf8(name)
            .ok()
            .read_error("Non UTF-8 COFF section name")
    }

    #[inline]
    fn segment_name(&self) -> Result<Option<&str>> {
        Ok(None)
    }

    #[inline]
    fn kind(&self) -> SectionKind {
        self.section.kind()
    }

    fn relocations(&self) -> CoffRelocationIterator<'data, 'file, R> {
        let relocations = self.section.coff_relocations(self.file.data).unwrap_or(&[]);
        CoffRelocationIterator {
            file: self.file,
            iter: relocations.iter(),
        }
    }

    fn flags(&self) -> SectionFlags {
        SectionFlags::Coff {
            characteristics: self.section.characteristics.get(LE),
        }
    }
}

impl pe::ImageSectionHeader {
    pub(crate) fn kind(&self) -> SectionKind {
        let characteristics = self.characteristics.get(LE);
        if characteristics & (pe::IMAGE_SCN_CNT_CODE | pe::IMAGE_SCN_MEM_EXECUTE) != 0 {
            SectionKind::Text
        } else if characteristics & pe::IMAGE_SCN_CNT_INITIALIZED_DATA != 0 {
            if characteristics & pe::IMAGE_SCN_MEM_DISCARDABLE != 0 {
                SectionKind::Other
            } else if characteristics & pe::IMAGE_SCN_MEM_WRITE != 0 {
                SectionKind::Data
            } else {
                SectionKind::ReadOnlyData
            }
        } else if characteristics & pe::IMAGE_SCN_CNT_UNINITIALIZED_DATA != 0 {
            SectionKind::UninitializedData
        } else if characteristics & pe::IMAGE_SCN_LNK_INFO != 0 {
            SectionKind::Linker
        } else {
            SectionKind::Unknown
        }
    }
}

impl pe::ImageSectionHeader {
    /// Return the section name.
    ///
    /// This handles decoding names that are offsets into the symbol string table.
    pub fn name<'data>(&'data self, strings: StringTable<'data>) -> Result<&'data [u8]> {
        let bytes = &self.name;
        Ok(if bytes[0] == b'/' {
            let mut offset = 0;
            if bytes[1] == b'/' {
                for byte in bytes[2..].iter() {
                    let digit = match byte {
                        b'A'..=b'Z' => byte - b'A',
                        b'a'..=b'z' => byte - b'a' + 26,
                        b'0'..=b'9' => byte - b'0' + 52,
                        b'+' => 62,
                        b'/' => 63,
                        _ => return Err(Error("Invalid COFF section name base-64 offset")),
                    };
                    offset = offset * 64 + digit as u32;
                }
            } else {
                for byte in bytes[1..].iter() {
                    let digit = match byte {
                        b'0'..=b'9' => byte - b'0',
                        0 => break,
                        _ => return Err(Error("Invalid COFF section name base-10 offset")),
                    };
                    offset = offset * 10 + digit as u32;
                }
            };
            strings
                .get(offset)
                .read_error("Invalid COFF section name offset")?
        } else {
            self.raw_name()
        })
    }

    /// Return the raw section name.
    pub fn raw_name(&self) -> &[u8] {
        let bytes = &self.name;
        match memchr::memchr(b'\0', bytes) {
            Some(end) => &bytes[..end],
            None => &bytes[..],
        }
    }

    /// Return the offset and size of the section in a COFF file.
    ///
    /// Returns `None` for sections that have no data in the file.
    pub fn coff_file_range(&self) -> Option<(u32, u32)> {
        if self.characteristics.get(LE) & pe::IMAGE_SCN_CNT_UNINITIALIZED_DATA != 0 {
            None
        } else {
            let offset = self.pointer_to_raw_data.get(LE);
            // Note: virtual size is not used for COFF.
            let size = self.size_of_raw_data.get(LE);
            Some((offset, size))
        }
    }

    /// Return the section data in a COFF file.
    ///
    /// Returns `Ok(&[])` if the section has no data.
    /// Returns `Err` for invalid values.
    pub fn coff_data<'data, R: ReadRef<'data>>(&self, data: R) -> result::Result<&'data [u8], ()> {
        if let Some((offset, size)) = self.coff_file_range() {
            data.read_bytes_at(offset.into(), size.into())
        } else {
            Ok(&[])
        }
    }

    /// Return the section alignment in bytes.
    ///
    /// This is only valid for sections in a COFF file.
    pub fn coff_alignment(&self) -> u64 {
        match self.characteristics.get(LE) & pe::IMAGE_SCN_ALIGN_MASK {
            pe::IMAGE_SCN_ALIGN_1BYTES => 1,
            pe::IMAGE_SCN_ALIGN_2BYTES => 2,
            pe::IMAGE_SCN_ALIGN_4BYTES => 4,
            pe::IMAGE_SCN_ALIGN_8BYTES => 8,
            pe::IMAGE_SCN_ALIGN_16BYTES => 16,
            pe::IMAGE_SCN_ALIGN_32BYTES => 32,
            pe::IMAGE_SCN_ALIGN_64BYTES => 64,
            pe::IMAGE_SCN_ALIGN_128BYTES => 128,
            pe::IMAGE_SCN_ALIGN_256BYTES => 256,
            pe::IMAGE_SCN_ALIGN_512BYTES => 512,
            pe::IMAGE_SCN_ALIGN_1024BYTES => 1024,
            pe::IMAGE_SCN_ALIGN_2048BYTES => 2048,
            pe::IMAGE_SCN_ALIGN_4096BYTES => 4096,
            pe::IMAGE_SCN_ALIGN_8192BYTES => 8192,
            _ => 16,
        }
    }

    /// Read the relocations in a COFF file.
    ///
    /// `data` must be the entire file data.
    pub fn coff_relocations<'data, R: ReadRef<'data>>(
        &self,
        data: R,
    ) -> read::Result<&'data [pe::ImageRelocation]> {
        let pointer = self.pointer_to_relocations.get(LE).into();
        let number = self.number_of_relocations.get(LE).into();
        data.read_slice_at(pointer, number)
            .read_error("Invalid COFF relocation offset or number")
    }
}
