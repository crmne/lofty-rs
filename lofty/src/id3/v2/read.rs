use super::frame::read::ParsedFrame;
use super::header::Id3v2Header;
use super::tag::Id3v2Tag;
use crate::config::ParseOptions;
use crate::id3::v2::error::{FrameParseError, Id3v2ParseError};
use crate::id3::v2::util::synchsafe::UnsynchronizedStream;
use crate::id3::v2::{Frame, FrameId, FrameList, Id3v2TagFlags, Id3v2Version, TimestampFrame};
use crate::tag::items::Timestamp;

use std::borrow::Cow;
use std::io::Read;

pub(crate) fn parse_id3v2<R>(
	bytes: &mut R,
	header: Id3v2Header,
	parse_options: ParseOptions,
) -> Result<Id3v2Tag, Id3v2ParseError>
where
	R: Read,
{
	log::debug!(
		"Parsing ID3v2 tag, size: {}, version: {:?}",
		header.size,
		header.version
	);

	let mut tag_bytes = bytes.take(u64::from(header.size - header.extended_size));

	// Prior to Id3v2.4, the tag-wide `unsynchronization` flag implied that unsynchronization was applied
	// to the *entire* content of the tag following the header, since there was no frame-level flag.
	// 2.4 added the `unsynchronization` flag to frame headers and made the tag-level flag equivalent to
	// setting flag on every frame.
	let mut ret;
	if header.flags.unsynchronisation && header.version < Id3v2Version::V4 {
		// Unsynchronize the entire tag
		let mut unsynchronized_reader = UnsynchronizedStream::new(tag_bytes);
		ret = read_all_frames_into_tag(&mut unsynchronized_reader, header, parse_options)?;

		// Get the `Take` back from the `UnsynchronizedStream`
		tag_bytes = unsynchronized_reader.into_inner();
	} else {
		ret = read_all_frames_into_tag(&mut tag_bytes, header, parse_options)?;
	}

	// Throw away the rest of the tag (padding, bad frames)
	std::io::copy(&mut tag_bytes, &mut std::io::sink())?;

	// Construct TDRC frame from TYER, TDAT, and TIME frames (TYE, TDA, and TIM in ID3v2.2)
	if parse_options.implicit_conversions {
		match header.version {
			Id3v2Version::V2 => construct_tdrc_from_split_date(
				&mut ret,
				&FrameId::Outdated(Cow::Borrowed("TDA")),
				&FrameId::Outdated(Cow::Borrowed("TIM")),
			),
			Id3v2Version::V3 => construct_tdrc_from_split_date(
				&mut ret,
				&FrameId::Valid(Cow::Borrowed("TDAT")),
				&FrameId::Valid(Cow::Borrowed("TIME")),
			),
			Id3v2Version::V4 => {},
		}
	}

	Ok(ret)
}

/// Merge the date and time frames of ID3v2.2 and ID3v2.3 into the TDRC frame converted from the
/// year frame
fn construct_tdrc_from_split_date(
	tag: &mut Id3v2Tag,
	date_id: &FrameId<'_>,
	time_id: &FrameId<'_>,
) {
	const TDRC: FrameId<'_> = FrameId::Valid(Cow::Borrowed("TDRC"));

	// Our TYER (TYE) frame gets converted to TDRC earlier
	let (year, encoding) = match tag.get(&TDRC) {
		Some(Frame::Timestamp(year_frame)) => {
			// This is not a TYER frame
			if year_frame.timestamp.month.is_some() {
				return;
			}

			(year_frame.timestamp.year, year_frame.encoding)
		},
		Some(_) => {
			log::warn!("TYER frame is not a timestamp frame, retaining.");
			return;
		},
		None => return,
	};

	let mut tdrc = Timestamp {
		year,
		..Timestamp::default()
	};

	let mut time_used = false;
	'build: {
		let Some(date) = tag.get_text(date_id) else {
			return;
		};

		if date.len() != 4 || !date.is_ascii() {
			log::warn!("Invalid {date_id} frame, retaining.");
			return;
		}

		let (Ok(day), Ok(month)) = (date[..2].parse::<u8>(), date[2..].parse::<u8>()) else {
			log::warn!("Invalid {date_id} frame, retaining.");
			return;
		};

		tdrc.month = Some(month);
		tdrc.day = Some(day);

		let Some(time) = tag.get_text(time_id) else {
			break 'build;
		};

		if time.len() != 4 || !time.is_ascii() {
			log::warn!("Invalid {time_id} frame, retaining.");
			break 'build;
		}

		let (Ok(hour), Ok(minute)) = (time[..2].parse::<u8>(), time[2..].parse::<u8>()) else {
			log::warn!("Invalid {time_id} frame, retaining.");
			break 'build;
		};

		tdrc.hour = Some(hour);
		tdrc.minute = Some(minute);
		time_used = true;
	}

	let _ = tag.remove(&TDRC);
	tag.insert(Frame::Timestamp(TimestampFrame::new(TDRC, encoding, tdrc)));

	let _ = tag.remove(date_id);

	if time_used {
		let _ = tag.remove(time_id);
	}
}

pub(super) fn read_all_frames_into_tag<R>(
	reader: &mut R,
	header: Id3v2Header,
	parse_options: ParseOptions,
) -> Result<Id3v2Tag, FrameParseError>
where
	R: Read,
{
	let mut tag = Id3v2Tag::default();
	tag.original_version = header.version;
	tag.set_flags(header.flags);
	tag.frames = read_all_frames_into_list(reader, header.version, header.flags, parse_options)?;

	Ok(tag)
}

pub(super) fn read_all_frames_into_list(
	reader: &mut dyn Read,
	version: Id3v2Version,
	flags: Id3v2TagFlags,
	parse_options: ParseOptions,
) -> Result<FrameList<'static>, FrameParseError> {
	let mut list = FrameList::default();

	loop {
		match ParsedFrame::read(reader, version, flags, parse_options)? {
			ParsedFrame::Next(frame) => {
				let frame_value_is_empty = frame.is_empty();
				if let Some(replaced_frame) = list.insert(frame) {
					// Duplicate frames are not allowed. But if this occurs we try
					// to keep the frame with the non-empty content. Superfluous,
					// duplicate frames that follow the first frame are often empty.
					if frame_value_is_empty == Some(true)
						&& replaced_frame.is_empty() == Some(false)
					{
						log::warn!(
							"Restoring non-empty frame with ID \"{id}\" that has been replaced by \
							 an empty frame with the same ID",
							id = replaced_frame.id()
						);
						drop(list.insert(replaced_frame));
					} else {
						log::warn!(
							"Replaced frame with ID \"{id}\" by a frame with the same ID",
							id = replaced_frame.id()
						);
					}
				}
			},
			// No frame content found or ignored due to errors, but we can expect more frames
			ParsedFrame::Skip => {},
			// No frame content found, and we can expect there are no more frames
			ParsedFrame::Eof => break,
		}
	}

	Ok(list)
}

#[cfg(test)]
mod tests {
	use super::parse_id3v2;
	use crate::config::ParseOptions;
	use crate::tag::items::Timestamp;

	#[test_log::test]
	fn zero_size_id3v2() {
		use crate::config::ParsingMode;
		use crate::id3::v2::header::Id3v2Header;

		use std::io::Cursor;

		let mut f = Cursor::new(std::fs::read("tests/tags/assets/id3v2/zero.id3v2").unwrap());
		let header = Id3v2Header::parse(&mut f).unwrap();
		assert!(
			parse_id3v2(
				&mut f,
				header,
				ParseOptions::new().parsing_mode(ParsingMode::Strict)
			)
			.is_ok()
		);
	}

	#[test_log::test]
	fn bad_frame_id_relaxed_id3v2() {
		use crate::config::ParsingMode;
		use crate::id3::v2::header::Id3v2Header;
		use crate::prelude::*;

		use std::io::Cursor;

		// Contains a frame with a "+" in the ID, which is invalid.
		// All other frames in the tag are valid, however.
		let mut f = Cursor::new(
			std::fs::read("tests/tags/assets/id3v2/bad_frame_otherwise_valid.id3v24").unwrap(),
		);
		let header = Id3v2Header::parse(&mut f).unwrap();
		let id3v2 = parse_id3v2(
			&mut f,
			header,
			ParseOptions::new().parsing_mode(ParsingMode::Relaxed),
		);
		assert!(id3v2.is_ok());

		let id3v2 = id3v2.unwrap();

		// There are 6 valid frames and 1 invalid frame
		assert_eq!(id3v2.len(), 6);

		assert_eq!(id3v2.title().as_deref(), Some("Foo title"));
		assert_eq!(id3v2.artist().as_deref(), Some("Bar artist"));
		assert_eq!(id3v2.comment().as_deref(), Some("Qux comment"));
		assert_eq!(
			id3v2.date(),
			Some(Timestamp {
				year: 1984,
				..Default::default()
			})
		);
		assert_eq!(id3v2.track(), Some(1));
		assert_eq!(id3v2.genre().as_deref(), Some("Classical"));
	}
}
