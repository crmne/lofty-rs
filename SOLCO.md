# Solco's lofty branch

This branch is lofty 0.25.4 with the fixes Solco needs to read a tag from a
file and save it back without losing or corrupting anything. It is maintained
at `crmne/lofty-rs` on the `solco` branch, and Solco uses it through
`[patch.crates-io]`:

```toml
[patch.crates-io]
lofty = { git = "https://github.com/crmne/lofty-rs", rev = "<commit>" }
```

The crate version stays the upstream release version (0.25.4), so Cargo uses
this branch in place of crates.io 0.25.x.

## Carried commits

Each fix is one commit with its own regression tests, so it can be offered
upstream on its own. The extended header, tag flag and TDA/TIM tests use the
`raw_tag` test helpers added by "Fix reading extended headers"; offered alone,
those commits need the helpers too.

| Commit | Problem it fixes | Upstream |
| --- | --- | --- |
| ID3v2: Allow writing ETCO and OWNE frames | `verify_frame` did not accept `Frame::EventTimingCodes` or `Frame::Ownership`, so saving any tag that contained an ETCO or OWNE frame failed. | Issue #189 |
| ID3v2: Write invalid frame languages as `XXX` | A COMM or USLT frame whose language is not three ASCII letters (often `00 00 00`) was read, but made every write of the tag fail. Such languages are now written as `XXX`; reading keeps the raw bytes. SYLT reads and writes the same way. | None |
| ID3v2: Fix reading extended headers | ID3v2.3 extended headers were parsed with the ID3v2.4 layout, losing the frames that followed. In both versions, unread extended header bytes (such as the ID3v2.4 "update" flag data) shifted the frames. `full_tag_size` counted the extended header twice. | None |
| ID3v2: Fix ID3v2.3 tag flags | The ID3v2.3 writer put "experimental" in 0x40 (the extended header bit) and wrote a footer, which ID3v2.3 does not have. It now skips the extended header, since it would have the ID3v2.4 layout. | None |
| ID3v2: Set the extended header flag when writing one | An ID3v2.4 tag written with the CRC or restrictions flags had an extended header but not the header bit for it, so it could not be read back. | None |
| ID3v2: Write reserved ETCO event types back unchanged | Reserved event types were written as 0xFF ("one more byte of events follows"). The frame now keeps their original bytes, without changing the public `Event` or `EventType` types. Events stay sorted, as the spec requires. | None |
| ID3v2: Escape ID3v2.3 genre refinements that begin with "(" | `((Drum & Bass)` was read as the genre `(Drum & Bass)` but written as `(Drum & Bass)`, which reads back as `Drum & Bass`. | None |
| ID3v2: Merge ID3v2.2 TDA and TIM frames into TDRC | ID3v2.2 date and time frames were left as outdated frames that cannot be written, so a save kept only the year. They are merged into TDRC like ID3v2.3 TDAT and TIME. A TDRC frame that already has a month is no longer discarded. | None |
| Add end-to-end ID3v2 round trip tests | Solco's reproducers, built from synthetic bytes and run through `MpegFile`, `WavFile` and `AiffFile`. Tests ending in `_by_design` pin behaviour Solco relies on. | Solco only |
| MP4: Preserve integer atoms wider than the numeric representation | A legacy `----:com.apple.iTunes:tool` value marked as an integer blocked strict parsing and was dropped by best-attempt parsing. Unsupported nonempty widths now remain opaque bytes, including their integer type, through an unrelated edit. Strict malformed-string checks stay. | None |
| Add SOLCO.md | This file. | Solco only |

## Left unchanged

These are deliberate upstream behaviours, or still under discussion, and the
branch does not change them:

- Repeated TXXX or COMM frames with the same key keep only the last one, even
  in strict mode.
- The ID3v2.3 writer discards ID3v2.4-only frames (TSOP, TSOA, TSOT, a TDRC
  that is not a timestamp).
- ETCO events are sorted by time when read and written.
- An MPEG file with a valid ID3v2 tag and a malformed APE footer fails to
  read (upstream issue #685). The round trip test for it is ignored.

## Offering a fix upstream

lofty's `CLAUDE.md` applies: AI must not open issues or pull requests or write
comments on them, AI use must be disclosed in the pull request, and the human
contributor writes every documentation comment. Before offering a commit,
rewrite its documentation comments yourself, add a CHANGELOG entry, and open
the pull request yourself.

## Rebasing onto a new lofty release

1. `git fetch origin --tags` and check the new tag, for example `0.25.5`.
2. Tag the revision Solco currently pins, so it stays reachable after the
   branch is rewritten: `git tag solco-0.25.4-<n> solco && git push fork solco-0.25.4-<n>`.
3. Rebase only the carried commits: `git rebase --onto 0.25.5 0.25.4 solco`.
   Drop any commit whose fix the release already contains (`git rebase` skips
   identical patches; read the upstream CHANGELOG for the rest), and update
   the table above.
4. Keep the crate version the release's own: none of the carried commits
   touch `Cargo.toml`.
5. Run `cargo fmt -- --check`, `cargo clippy --all-targets`, `cargo test`
   and `cargo doc --no-deps` for the whole workspace. All must be clean.
   `probe::tests` fail intermittently on unmodified 0.25.4 too: a test
   lowers the global allocation limit while other tests run.
6. `git push --force-with-lease fork solco`, then update the `rev` in Solco's
   `Cargo.toml` and run Solco's tests.
