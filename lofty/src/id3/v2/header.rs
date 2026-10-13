use crate::error::FakeTagError;
use crate::id3::v2::error::{Id3v2HeaderError, Id3v2ParseError};
use crate::id3::v2::restrictions::TagRestrictions;
use crate::id3::v2::util::synchsafe::SynchsafeInteger;

use std::fmt::Display;
use std::io::Read;

use byteorder::{BigEndian, ByteOrder, ReadBytesExt};

/// The ID3v2 version
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Id3v2Version {
	/// ID3v2.2
	V2,
	/// ID3v2.3
	V3,
	/// ID3v2.4
	V4,
}

impl Display for Id3v2Version {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			Id3v2Version::V2 => write!(f, "ID3v2.2"),
			Id3v2Version::V3 => write!(f, "ID3v2.3"),
			Id3v2Version::V4 => write!(f, "ID3v2.4"),
		}
	}
}

/// Flags that apply to the entire tag
#[derive(Default, Copy, Clone, Debug, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct Id3v2TagFlags {
	/// Whether or not all frames are unsynchronised. See [`FrameFlags::unsynchronisation`](crate::id3::v2::FrameFlags::unsynchronisation)
	pub unsynchronisation: bool,
	/// Indicates if the tag is in an experimental stage
	pub experimental: bool,
	/// Indicates that the tag includes a footer
	///
	/// A footer will be created if the tag is written as ID3v2.4. ID3v2.3 has no footer.
	pub footer: bool,
	/// Whether or not to include a CRC-32 in the extended header
	///
	/// This is calculated if the tag is written
	pub crc: bool,
	/// Restrictions on the tag, written in the extended header
	///
	/// In addition to being setting this flag, all restrictions must be provided. See [`TagRestrictions`]
	pub restrictions: Option<TagRestrictions>,
}

impl Id3v2TagFlags {
	/// Get the **ID3v2.4** byte representation of the flags
	///
	/// NOTE: This does not include the extended header flags
	pub fn as_id3v24_byte(&self) -> u8 {
		let mut byte = 0;

		if self.unsynchronisation {
			byte |= 0x80;
		}

		if self.experimental {
			byte |= 0x20;
		}

		if self.footer {
			byte |= 0x10;
		}

		byte
	}

	/// Get the **ID3v2.3** byte representation of the flags
	///
	/// NOTE: This does not include the extended header flags. ID3v2.3 has no footer, so
	///       [`Self::footer`] is not represented.
	pub fn as_id3v23_byte(&self) -> u8 {
		let mut byte = 0;

		if self.experimental {
			byte |= 0x20;
		}

		byte
	}
}

#[derive(Copy, Clone, Debug)]
pub(crate) struct Id3v2Header {
	pub version: Id3v2Version,
	pub flags: Id3v2TagFlags,
	/// The size of the tag contents (**DOES NOT INCLUDE THE HEADER/FOOTER**)
	pub size: u32,
	pub extended_size: u32,
}

impl Id3v2Header {
	pub(crate) fn parse<R>(bytes: &mut R) -> Result<Self, Id3v2ParseError>
	where
		R: Read,
	{
		log::debug!("Parsing ID3v2 header");

		let mut header = [0; 10];
		bytes.read_exact(&mut header)?;

		if &header[..3] != b"ID3" {
			return Err(FakeTagError.into());
		}

		// Version is stored as [major, minor], but here we don't care about minor revisions unless there's an error.
		let version = match header[3] {
			2 => Id3v2Version::V2,
			3 => Id3v2Version::V3,
			4 => Id3v2Version::V4,
			major => {
				return Err(Id3v2HeaderError::BadId3v2Version(major, header[4]).into());
			},
		};

		let flags = header[5];

		// Compression was a flag only used in ID3v2.2 (bit 2).
		// At the time the ID3v2.2 specification was written, a compression scheme wasn't decided.
		// The spec recommends just ignoring the tag in this case.
		if version == Id3v2Version::V2 && flags & 0x40 == 0x40 {
			return Err(Id3v2HeaderError::V2Compression.into());
		}

		let mut flags_parsed = Id3v2TagFlags {
			unsynchronisation: flags & 0x80 == 0x80,
			experimental: (version == Id3v2Version::V4 || version == Id3v2Version::V3)
				&& flags & 0x20 == 0x20,
			// ID3v2.3 has no footer
			footer: version == Id3v2Version::V4 && flags & 0x10 == 0x10,
			crc: false,         // Retrieved later if applicable
			restrictions: None, // Retrieved later if applicable
		};

		let size = BigEndian::read_u32(&header[6..]).unsynch();

		// The number of bytes the extended header occupies in the tag, including its size field.
		// The tag size includes the extended header, so the frames follow in `size - extended_size`
		// bytes.
		let mut extended_size = 0;

		let extended_header =
			(version == Id3v2Version::V4 || version == Id3v2Version::V3) && flags & 0x40 == 0x40;

		if extended_header {
			extended_size = match version {
				// In ID3v2.3, the extended header is considered separate from the frame header, and
				// thus subject to unsynchronization
				Id3v2Version::V3 => read_id3v23_extended_header(bytes, &mut flags_parsed, size)?,
				// In ID3v2.4, they seem to be considered one big header (?) so no flags apply to it
				Id3v2Version::V4 => read_id3v24_extended_header(bytes, &mut flags_parsed, size)?,
				Id3v2Version::V2 => unreachable!(),
			};
		}

		if extended_size > 0 && extended_size >= size {
			return Err(Id3v2HeaderError::BadExtendedHeaderSize.into());
		}

		Ok(Id3v2Header {
			version,
			flags: flags_parsed,
			size,
			extended_size,
		})
	}

	/// The total size of the tag, including the header, footer, and extended header
	pub(crate) fn full_tag_size(&self) -> u32 {
		// The tag size already includes the extended header
		self.size + 10 + if self.flags.footer { 10 } else { 0 }
	}
}

/// Read `len` bytes of the tag, removing unsynchronization if `unsynchronised`
///
/// Returns the bytes and the number of bytes consumed from `reader`. This reads byte by byte, so
/// unlike [`UnsynchronizedStream`](crate::id3::v2::util::synchsafe::UnsynchronizedStream), it consumes nothing past the requested bytes.
fn read_tag_bytes<R: Read>(
	reader: &mut R,
	len: u32,
	unsynchronised: bool,
) -> Result<(Vec<u8>, u32), Id3v2ParseError> {
	if !unsynchronised {
		let mut content = Vec::new();
		reader.take(u64::from(len)).read_to_end(&mut content)?;
		if content.len() != len as usize {
			return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof).into());
		}

		return Ok((content, len));
	}

	let mut content = Vec::new();
	let mut consumed = 0_u32;
	let mut previous = 0_u8;
	while content.len() < len as usize {
		let byte = reader.read_u8()?;
		consumed += 1;

		// An unsynchronization byte, inserted after every 0xFF
		if previous == 0xFF && byte == 0x00 {
			previous = byte;
			continue;
		}

		content.push(byte);
		previous = byte;
	}

	// A trailing 0xFF is followed by its unsynchronization byte, which belongs to these bytes
	if previous == 0xFF {
		if reader.read_u8()? != 0x00 {
			return Err(Id3v2HeaderError::BadExtendedHeaderSize.into());
		}
		consumed += 1;
	}

	Ok((content, consumed))
}

/// Read an ID3v2.3 extended header, returning the number of bytes it occupies in the tag
///
/// ```text
/// Extended header size   $xx xx xx xx (excludes itself: 6 or 10)
/// Extended Flags         $xx xx (%x0000000 00000000, where x = CRC data present)
/// Size of padding        $xx xx xx xx
/// Total frame CRC        $xx xx xx xx (if the CRC flag is set)
/// ```
fn read_id3v23_extended_header<R: Read>(
	reader: &mut R,
	flags: &mut Id3v2TagFlags,
	tag_size: u32,
) -> Result<u32, Id3v2ParseError> {
	let unsynchronised = flags.unsynchronisation;
	let (size_bytes, size_len) = read_tag_bytes(reader, 4, unsynchronised)?;
	let header_size = BigEndian::read_u32(&size_bytes);
	if header_size < 6 || header_size >= tag_size {
		return Err(Id3v2HeaderError::BadExtendedHeaderSize.into());
	}

	let (content, content_len) = read_tag_bytes(reader, header_size, unsynchronised)?;

	// The only flag is CRC data present. The padding size and the CRC itself are not needed.
	if content[0] & 0x80 == 0x80 {
		flags.crc = true;
	}

	Ok(size_len + content_len)
}

/// Read an ID3v2.4 extended header, returning the number of bytes it occupies in the tag
///
/// ```text
/// Extended header size   4 * %0xxxxxxx (synchsafe, includes itself)
/// Number of flag bytes   $01
/// Extended Flags         $xx (%0bcd0000)
/// ```
///
/// Each set flag is followed, in order, by a length byte and that many bytes of data:
///
/// * b - Tag is an update (no data)
/// * c - CRC data present (5 bytes)
/// * d - Tag restrictions (1 byte)
fn read_id3v24_extended_header<R: Read>(
	reader: &mut R,
	flags: &mut Id3v2TagFlags,
	tag_size: u32,
) -> Result<u32, Id3v2ParseError> {
	let header_size = reader.read_u32::<BigEndian>()?.unsynch();
	if header_size < 6 || header_size >= tag_size {
		return Err(Id3v2HeaderError::BadExtendedHeaderSize.into());
	}

	// Everything after the size is read, so that any flag data we skip is not left for the frames
	let (content, _) = read_tag_bytes(reader, header_size - 4, false)?;

	// Useless byte since there's only 1 byte for flags
	let _num_flag_bytes = content[0];
	let extended_flags = content[1];

	let mut flag_data = &content[2..];
	for flag in [0x40, 0x20, 0x10] {
		if extended_flags & flag == 0 {
			continue;
		}

		let Some((&data_length, rest)) = flag_data.split_first() else {
			return Err(Id3v2HeaderError::BadExtendedHeaderSize.into());
		};
		let Some((data, rest)) = rest.split_at_checked(usize::from(data_length)) else {
			return Err(Id3v2HeaderError::BadExtendedHeaderSize.into());
		};
		flag_data = rest;

		match flag {
			// We don't care about the existing CRC
			0x20 => flags.crc = true,
			0x10 => {
				if let Some(&restrictions) = data.first() {
					flags.restrictions = Some(TagRestrictions::from_byte(restrictions));
				}
			},
			_ => {},
		}
	}

	Ok(header_size)
}
