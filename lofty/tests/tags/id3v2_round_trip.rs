//! End-to-end ID3v2 round trips through `MpegFile`, `WavFile` and `AiffFile`
//!
//! Every input is built from synthetic bytes in the test. A test whose name ends in `_by_design`
//! asserts Lofty's current, intended behaviour, which callers that save a tag they read rely on.

use std::borrow::Cow;
use std::fmt::Write as _;
use std::io::Cursor;

use lofty::TextEncoding;
use lofty::config::{ParseOptions, ParsingMode, WriteOptions};
use lofty::file::AudioFile;
use lofty::id3::v2::{Frame, FrameId, Id3v2Tag, Id3v2Version, TextInformationFrame};
use lofty::iff::aiff::AiffFile;
use lofty::iff::wav::WavFile;
use lofty::mpeg::MpegFile;
use lofty::tag::{Accessor, TagExt};

// ---------------------------------------------------------------- helpers

fn syncsafe(n: u32) -> [u8; 4] {
	assert!(n < 1 << 28);
	[
		((n >> 21) & 0x7F) as u8,
		((n >> 14) & 0x7F) as u8,
		((n >> 7) & 0x7F) as u8,
		(n & 0x7F) as u8,
	]
}

/// One frame in the layout of ID3v2.`major`.
fn frame(major: u8, id: &[u8], payload: &[u8]) -> Vec<u8> {
	let mut out = id.to_vec();
	let len = payload.len() as u32;
	match major {
		2 => out.extend(&len.to_be_bytes()[1..]),
		3 => out.extend(len.to_be_bytes()),
		4 => out.extend(syncsafe(len)),
		_ => unreachable!(),
	}
	if major != 2 {
		out.extend([0, 0]);
	}
	out.extend(payload);
	out
}

/// A Latin-1 text frame payload.
fn text(value: &str) -> Vec<u8> {
	let mut out = vec![0];
	out.extend(value.as_bytes());
	out
}

/// A complete tag: header followed by `body` (extended header and frames).
fn tag(major: u8, flags: u8, body: &[u8]) -> Vec<u8> {
	let mut out = b"ID3".to_vec();
	out.extend([major, 0, flags]);
	out.extend(syncsafe(body.len() as u32));
	out.extend(body);
	out
}

/// Ten MPEG-1 Layer III frames (128 kbit/s, 44.1 kHz, stereo, 417 bytes),
/// filled with 0xAA.
fn mpeg_audio() -> Vec<u8> {
	let mut frame = vec![0xAA; 417];
	frame[..4].copy_from_slice(&[0xFF, 0xFB, 0x90, 0x00]);
	frame.repeat(10)
}

fn options(mode: ParsingMode) -> ParseOptions {
	ParseOptions::new()
		.read_properties(false)
		.parsing_mode(mode)
}

fn read_mpeg(bytes: Vec<u8>, mode: ParsingMode) -> Result<MpegFile, lofty::error::FileParseError> {
	MpegFile::read_from(&mut Cursor::new(bytes), options(mode))
}

/// Parse `id3` (one or more tags) in front of MPEG audio, strictly.
fn parse(id3: &[u8]) -> Id3v2Tag {
	let file = read_mpeg([id3, &mpeg_audio()].concat(), ParsingMode::Strict).unwrap();
	file.id3v2().cloned().expect("an ID3v2 tag")
}

/// Encode `tag` alone, reporting an error with its whole source chain.
fn dump(tag: &Id3v2Tag, v23: bool) -> Result<Vec<u8>, String> {
	let mut out = Vec::new();
	match tag.dump_to(&mut out, WriteOptions::new().use_id3v23(v23)) {
		Ok(()) => Ok(out),
		Err(e) => {
			let mut message = e.to_string();
			let mut source = std::error::Error::source(&e);
			while let Some(cause) = source {
				let _ = write!(message, ": {cause}");
				source = cause.source();
			}
			Err(message)
		},
	}
}

fn id(name: &'static str) -> FrameId<'static> {
	FrameId::Valid(Cow::Borrowed(name))
}

fn count(tag: &Id3v2Tag, name: &str) -> usize {
	tag.iter().filter(|f| f.id_str() == name).count()
}

// ETCO and OWNE ---------------------------------------------------------------

fn etco_payload(events: &[(u8, u32)]) -> Vec<u8> {
	// Timestamp format 2: milliseconds.
	let mut out = vec![2];
	for (event_type, time) in events {
		out.push(*event_type);
		out.extend(time.to_be_bytes());
	}
	out
}

fn owne_payload() -> Vec<u8> {
	let mut out = vec![0];
	out.extend(b"USD1.00\0");
	out.extend(b"20200101");
	out.extend(b"Seller");
	out
}

#[test]
fn etco_read_from_a_tag_can_be_written_back() {
	let body = [
		frame(4, b"TIT2", &text("Title")),
		frame(4, b"ETCO", &etco_payload(&[(0x01, 10), (0x03, 20)])),
	]
	.concat();
	let parsed = parse(&tag(4, 0, &body));
	assert!(matches!(
		parsed.get(&id("ETCO")),
		Some(Frame::EventTimingCodes(_))
	));

	// Used to fail with "a value of type 'EventTimingCodes' cannot be written under this ID"
	dump(&parsed, false).expect("ID3v2.4 write");
	dump(&parsed, true).expect("ID3v2.3 write");
}

#[test]
fn owne_read_from_a_tag_can_be_written_back() {
	let body = [
		frame(4, b"TIT2", &text("Title")),
		frame(4, b"OWNE", &owne_payload()),
	]
	.concat();
	let parsed = parse(&tag(4, 0, &body));
	assert!(matches!(parsed.get(&id("OWNE")), Some(Frame::Ownership(_))));

	// Used to fail with "a value of type 'Ownership' cannot be written under this ID"
	dump(&parsed, false).expect("ID3v2.4 write");
	dump(&parsed, true).expect("ID3v2.3 write");
}

// COMM/USLT with a 00 00 00 language ------------------------------------------

fn language_payload(language: [u8; 3], description: &str, content: &str) -> Vec<u8> {
	let mut out = vec![0];
	out.extend(language);
	out.extend(description.as_bytes());
	out.push(0);
	out.extend(content.as_bytes());
	out
}

#[test]
fn comment_with_zero_language_is_read_and_can_be_written_back() {
	let body = [
		frame(4, b"TIT2", &text("Title")),
		frame(4, b"COMM", &language_payload([0; 3], "", "A comment")),
		frame(4, b"USLT", &language_payload([0; 3], "", "Some lyrics")),
	]
	.concat();
	// Reading succeeds even in strict mode, and keeps the zero bytes.
	let parsed = parse(&tag(4, 0, &body));
	let comment = parsed.comments().next().expect("COMM is read");
	assert_eq!(comment.language, [0; 3]);
	assert_eq!(parsed.unsync_text().count(), 1);

	// Used to fail with "invalid frame language found: \x00\x00\x00 (expected 3 ascii characters)"
	let written = dump(&parsed, false).expect("the tag that was read can be written");
	let reread = parse(&written);
	assert_eq!(reread.comments().next().unwrap().content, "A comment");
}

// growing an end-of-file ID3 chunk in WAV/AIFF --------------------------------

fn small_id3() -> Vec<u8> {
	tag(4, 0, &frame(4, b"TIT2", &text("T")))
}

fn wav_with_trailing_id3() -> Vec<u8> {
	let mut body = b"WAVE".to_vec();
	body.extend(b"fmt ");
	body.extend(16u32.to_le_bytes());
	body.extend(1u16.to_le_bytes()); // PCM
	body.extend(1u16.to_le_bytes()); // mono
	body.extend(44100u32.to_le_bytes());
	body.extend(88200u32.to_le_bytes());
	body.extend(2u16.to_le_bytes());
	body.extend(16u16.to_le_bytes());
	body.extend(b"data");
	body.extend(4u32.to_le_bytes());
	body.extend([0; 4]);
	let id3 = small_id3();
	body.extend(b"ID3 ");
	body.extend((id3.len() as u32).to_le_bytes());
	body.extend(&id3);
	if id3.len() % 2 == 1 {
		body.push(0);
	}
	let mut out = b"RIFF".to_vec();
	out.extend((body.len() as u32).to_le_bytes());
	out.extend(body);
	out
}

fn aiff_with_trailing_id3() -> Vec<u8> {
	let mut body = b"AIFF".to_vec();
	body.extend(b"COMM");
	body.extend(18u32.to_be_bytes());
	body.extend(1u16.to_be_bytes()); // channels
	body.extend(2u32.to_be_bytes()); // sample frames
	body.extend(16u16.to_be_bytes()); // sample size
	body.extend([0x40, 0x0E, 0xAC, 0x44, 0, 0, 0, 0, 0, 0]); // 44100 Hz
	body.extend(b"SSND");
	body.extend(12u32.to_be_bytes());
	body.extend([0; 12]);
	let id3 = small_id3();
	body.extend(b"ID3 ");
	body.extend((id3.len() as u32).to_be_bytes());
	body.extend(&id3);
	if id3.len() % 2 == 1 {
		body.push(0);
	}
	let mut out = b"FORM".to_vec();
	out.extend((body.len() as u32).to_be_bytes());
	out.extend(body);
	out
}

fn grow(id3: &Id3v2Tag, file: &mut Cursor<Vec<u8>>) {
	let mut grown = id3.clone();
	grown.set_title("A much longer title".repeat(100));
	file.set_position(0);
	grown.save_to(file, WriteOptions::default()).unwrap();
}

#[test]
fn growing_trailing_id3_chunk_keeps_riff_size_valid() {
	let mut file = Cursor::new(wav_with_trailing_id3());
	let wav = WavFile::read_from(&mut file, options(ParsingMode::Strict)).unwrap();
	grow(wav.id3v2().unwrap(), &mut file);

	let bytes = file.into_inner();
	let declared = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
	assert_eq!(declared + 8, bytes.len());
	let reread = WavFile::read_from(&mut Cursor::new(bytes), options(ParsingMode::Strict)).unwrap();
	assert_eq!(reread.id3v2().unwrap().title().unwrap().len(), 1900);
}

#[test]
fn growing_trailing_id3_chunk_keeps_form_size_valid() {
	let mut file = Cursor::new(aiff_with_trailing_id3());
	let aiff = AiffFile::read_from(&mut file, options(ParsingMode::Strict)).unwrap();
	grow(aiff.id3v2().unwrap(), &mut file);

	let bytes = file.into_inner();
	let declared = u32::from_be_bytes(bytes[4..8].try_into().unwrap()) as usize;
	assert_eq!(declared + 8, bytes.len());
	let reread =
		AiffFile::read_from(&mut Cursor::new(bytes), options(ParsingMode::Strict)).unwrap();
	assert_eq!(reread.id3v2().unwrap().title().unwrap().len(), 1900);
}

// extended headers ------------------------------------------------------------

fn two_frames(major: u8) -> Vec<u8> {
	[
		frame(major, b"TIT2", &text("Title")),
		frame(major, b"TPE1", &text("Artist")),
	]
	.concat()
}

#[test]
fn id3v23_extended_header_without_crc_keeps_following_frames() {
	// ID3v2.3 extended header: size (4, excluding itself) = 6, flags (2),
	// size of padding (4).
	let mut body = 6u32.to_be_bytes().to_vec();
	body.extend([0, 0]);
	body.extend(0u32.to_be_bytes());
	body.extend(two_frames(3));
	let parsed = parse(&tag(3, 0x40, &body));
	assert_eq!(parsed.title().as_deref(), Some("Title"));
	assert_eq!(parsed.artist().as_deref(), Some("Artist"));
}

#[test]
fn id3v23_extended_header_with_crc_keeps_following_frames() {
	// Size 10: flags 0x8000 (CRC present), size of padding, CRC (4).
	let mut body = 10u32.to_be_bytes().to_vec();
	body.extend([0x80, 0]);
	body.extend(0u32.to_be_bytes());
	body.extend([0x12, 0x34, 0x56, 0x78]);
	body.extend(two_frames(3));
	let parsed = parse(&tag(3, 0x40, &body));
	assert_eq!(parsed.title().as_deref(), Some("Title"));
	assert_eq!(parsed.artist().as_deref(), Some("Artist"));
}

/// Control: the ID3v2.4 layout (size includes itself, one flag byte) works.
#[test]
fn id3v24_extended_header_keeps_following_frames() {
	let mut body = syncsafe(6).to_vec();
	body.extend([1, 0]);
	body.extend(two_frames(4));
	let parsed = parse(&tag(4, 0x40, &body));
	assert_eq!(parsed.title().as_deref(), Some("Title"));
	assert_eq!(parsed.artist().as_deref(), Some("Artist"));
}

/// An ID3v2.4 extended header whose "tag is an update" flag (0x40)
/// carries its one-byte flag data (length 0) is 7 bytes long; only 6 are read.
#[test]
fn id3v24_extended_header_with_update_flag_keeps_following_frames() {
	let mut body = syncsafe(7).to_vec();
	body.extend([1, 0x40, 0]);
	body.extend(two_frames(4));
	let parsed = parse(&tag(4, 0x40, &body));
	assert_eq!(parsed.title().as_deref(), Some("Title"));
	assert_eq!(parsed.artist().as_deref(), Some("Artist"));
}

/// The ID3v2.3 writer used to put the experimental flag in bit 0x40, which
/// is the extended-header bit in ID3v2.3 (experimental is 0x20).
#[test]
fn id3v23_experimental_flag_is_written_as_experimental() {
	let mut source = Id3v2Tag::new();
	source.set_title("Title".to_owned());
	let mut flags = *source.flags();
	flags.experimental = true;
	source.set_flags(flags);

	let written = dump(&source, true).unwrap();
	assert_eq!(written[5] & 0x60, 0x20, "header flags {:#04x}", written[5]);
	let reread = read_mpeg([written, mpeg_audio()].concat(), ParsingMode::Strict)
		.expect("the written tag can be read back");
	assert_eq!(reread.id3v2().unwrap().title().as_deref(), Some("Title"));
}

// repeated frames with one key ------------------------------------------------

#[test]
fn duplicate_txxx_and_comm_keep_only_the_last_even_in_strict_mode_by_design() {
	let txxx = |value: &str| {
		let mut payload = vec![0];
		payload.extend(b"CATALOG\0");
		payload.extend(value.as_bytes());
		frame(4, b"TXXX", &payload)
	};
	let body = [
		txxx("CAT-001"),
		txxx("CAT-002"),
		frame(4, b"COMM", &language_payload(*b"eng", "", "first")),
		frame(4, b"COMM", &language_payload(*b"eng", "", "second")),
	]
	.concat();
	// Strict mode accepts the tag and reports nothing.
	let parsed = parse(&tag(4, 0, &body));
	assert_eq!(count(&parsed, "TXXX"), 1);
	assert_eq!(parsed.get_user_text("CATALOG"), Some("CAT-002"));
	assert_eq!(count(&parsed, "COMM"), 1);
	assert_eq!(parsed.comments().next().unwrap().content, "second");
	// So a read-then-save drops CAT-001 and "first".
	let reread = parse(&dump(&parsed, false).unwrap());
	assert_eq!(count(&reread, "TXXX"), 1);
}

// ETCO events -----------------------------------------------------------------

fn etco_bytes_after_read(events: &[(u8, u32)]) -> (Vec<u8>, Vec<u8>) {
	let payload = etco_payload(events);
	let parsed = parse(&tag(4, 0, &frame(4, b"ETCO", &payload)));
	let Some(Frame::EventTimingCodes(etco)) = parsed.get(&id("ETCO")) else {
		panic!("ETCO is read");
	};
	(payload, etco.as_bytes().unwrap())
}

#[test]
fn etco_unknown_event_types_are_written_back_unchanged() {
	// 0x17 and 0xF0 are reserved event types; Lofty reads both as
	// `EventType::Reserved`, whose discriminant is 0xFF, the spec's "one more
	// byte of events follows" marker, and used to write them as such.
	let (stored, written) = etco_bytes_after_read(&[(0x17, 10), (0xF0, 20)]);
	assert_eq!(written, stored);
}

#[test]
fn etco_events_are_sorted_by_time_by_design() {
	// The spec requires chronological order; Lofty sorts on read and write.
	let (stored, written) = etco_bytes_after_read(&[(0x03, 20), (0x02, 10)]);
	assert_ne!(written, stored);
	assert_eq!(written, etco_payload(&[(0x02, 10), (0x03, 20)]));
}

// ID3v2.3 writer drops ID3v2.4-only frames ------------------------------------

#[test]
fn id3v23_writer_discards_v24_only_frames_by_design() {
	let body = [
		frame(3, b"TIT2", &text("Title")),
		frame(3, b"TSOP", &text("Artist, The")),
		frame(3, b"TSOA", &text("Album, The")),
		frame(3, b"TSOT", &text("Title, The")),
	]
	.concat();
	// An ID3v2.3 tag as iTunes and others write it: the sort frames are read...
	let parsed = parse(&tag(3, 0, &body));
	assert_eq!(parsed.original_version(), Id3v2Version::V3);
	assert_eq!(count(&parsed, "TSOP"), 1);
	// ...and written back as ID3v2.3 without them (only a log warning).
	let reread = parse(&dump(&parsed, true).unwrap());
	assert_eq!(reread.title().as_deref(), Some("Title"));
	for name in ["TSOP", "TSOA", "TSOT"] {
		assert_eq!(count(&reread, name), 0, "{name}");
	}
	// ID3v2.4 keeps them.
	let reread = parse(&dump(&parsed, false).unwrap());
	assert_eq!(count(&reread, "TSOP"), 1);
}

#[test]
fn id3v23_writer_discards_text_date_frames_by_design() {
	// A recording date that is not a timestamp, e.g. from `Tag` conversion
	// of `ItemKey::RecordingDate` with free text.
	let mut source = Id3v2Tag::new();
	source.set_title("Title".to_owned());
	source.insert(Frame::Text(TextInformationFrame::new(
		id("TDRC"),
		TextEncoding::UTF8,
		"1999?".to_owned(),
	)));
	// ID3v2.4 writes it as a text frame.
	assert!(
		dump(&source, false)
			.unwrap()
			.windows(5)
			.any(|w| w == b"1999?")
	);
	let v23 = parse(&dump(&source, true).unwrap());
	assert_eq!(count(&v23, "TDRC") + count(&v23, "TYER"), 0);
}

// ID3v2.3 genre (TCON) --------------------------------------------------------

fn genres_after_v23_round_trip(stored: &str) -> (Vec<String>, String, Vec<String>) {
	let parsed = parse(&tag(3, 0, &frame(3, b"TCON", &text(stored))));
	let before = parsed.genres().unwrap().map(str::to_owned).collect();
	let reread = parse(&dump(&parsed, true).unwrap());
	let raw = reread.get_text(&id("TCON")).unwrap().to_owned();
	let after = reread.genres().unwrap().map(str::to_owned).collect();
	(before, raw, after)
}

#[test]
fn id3v23_genre_text_is_canonicalised_but_genres_are_kept_by_design() {
	// "140" becomes the ID3v2.3 reference "(140)": the same genre.
	let (before, raw, after) = genres_after_v23_round_trip("140");
	assert_eq!(raw, "(140)");
	assert_eq!(before, after);

	// "(Drum & Bass)" is an ID3v2.3 reference syntax that Lofty reads as the
	// genre "Drum & Bass", and writes as plain "Drum & Bass": the same genre.
	let (before, raw, after) = genres_after_v23_round_trip("(Drum & Bass)");
	assert_eq!(raw, "Drum & Bass");
	assert_eq!(before, vec!["Drum & Bass"]);
	assert_eq!(before, after);
}

#[test]
fn id3v23_escaped_genre_keeps_its_leading_parenthesis() {
	// ID3v2.3 escapes a refinement that starts with "(" as "((". Lofty reads
	// "((Drum & Bass)" as the genre "(Drum & Bass)", and used to write it
	// unescaped, so it read back as "Drum & Bass".
	let (before, raw, after) = genres_after_v23_round_trip("((Drum & Bass)");
	assert_eq!(before, vec!["(Drum & Bass)"]);
	assert_eq!(after, before, "written as {raw:?}");
}

// ID3v2.2 date and time -------------------------------------------------------

#[test]
fn id3v22_date_and_time_survive_a_save() {
	let body = [
		frame(2, b"TT2", &text("Title")),
		frame(2, b"TYE", &text("1999")),
		frame(2, b"TDA", &text("1503")),
		frame(2, b"TIM", &text("1230")),
	]
	.concat();
	let parsed = parse(&tag(2, 0, &body));
	assert_eq!(parsed.original_version(), Id3v2Version::V2);
	// TDA and TIM are merged into TDRC, like ID3v2.3 TDAT and TIME.
	assert!(!parsed.iter().any(|f| f.id_str() == "TDA"));
	assert!(!parsed.iter().any(|f| f.id_str() == "TIM"));

	let reread = parse(&dump(&parsed, false).unwrap());
	assert_eq!(reread.title().as_deref(), Some("Title"));
	let Some(Frame::Timestamp(tdrc)) = reread.get(&id("TDRC")) else {
		panic!("TDRC is written");
	};
	// Used to be "1999", with the day, month and time gone.
	assert_eq!(
		(
			tdrc.timestamp.month,
			tdrc.timestamp.day,
			tdrc.timestamp.hour
		),
		(Some(3), Some(15), Some(12))
	);
}

// malformed APE trailer -------------------------------------------------------

#[test]
#[ignore = "how to handle a malformed APE footer is under discussion upstream (#685)"]
fn mpeg_with_valid_id3v2_and_malformed_ape_footer_is_readable() {
	let mut bytes = parse_input_with_title();
	// An APEv2 footer claiming a 142-byte tag of 3 items, where what precedes
	// it is audio (the shape in issue #685).
	bytes.extend(b"APETAGEX");
	bytes.extend(2000u32.to_le_bytes());
	bytes.extend(142u32.to_le_bytes());
	bytes.extend(3u32.to_le_bytes());
	bytes.extend(0xA000_0000u32.to_le_bytes());
	bytes.extend([0; 8]);

	let failures: Vec<String> = [
		ParsingMode::Strict,
		ParsingMode::BestAttempt,
		ParsingMode::Relaxed,
	]
	.into_iter()
	.filter_map(|mode| match read_mpeg(bytes.clone(), mode) {
		Ok(file) => {
			assert_eq!(file.id3v2().unwrap().title().as_deref(), Some("Title"));
			None
		},
		Err(e) => Some(format!(
			"{mode:?}: {e}: {}",
			std::error::Error::source(&e).map_or(String::new(), ToString::to_string)
		)),
	})
	.collect();
	assert!(failures.is_empty(), "{failures:#?}");
}

fn parse_input_with_title() -> Vec<u8> {
	[tag(4, 0, &frame(4, b"TIT2", &text("Title"))), mpeg_audio()].concat()
}
