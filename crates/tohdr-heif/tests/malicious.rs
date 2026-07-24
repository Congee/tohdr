//! Hand-crafted hostile files. Found by adversarial review; each of these
//! reproduced a real defect before the fix.
//!
//! The existing fuzzers mutate a *valid* file, which cannot reach these: a
//! truncated or bit-flipped `iloc` still has a plausible item count, and the
//! byte-flip sweep never lands the specific 8-byte pattern needed to overflow
//! an offset addition. These are constructed to hit exactly those paths.

use tohdr_heif::HeifFile;

/// Build `ftyp` + `meta` wrapping the given `meta` children.
fn file_with_meta_children(children: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&16u32.to_be_bytes());
    out.extend_from_slice(b"ftyp");
    out.extend_from_slice(b"heic");
    out.extend_from_slice(&0u32.to_be_bytes());

    let meta_len = 8 + 4 + children.len();
    out.extend_from_slice(&(meta_len as u32).to_be_bytes());
    out.extend_from_slice(b"meta");
    out.extend_from_slice(&0u32.to_be_bytes()); // FullBox version+flags
    out.extend_from_slice(children);
    out
}

fn box_with(ty: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&((8 + body.len()) as u32).to_be_bytes());
    b.extend_from_slice(ty);
    b.extend_from_slice(body);
    b
}

/// `iloc` declared 0xFFFFFFFF items and the parser called
/// `Vec::with_capacity(0xFFFFFFFF)` — about 137 GiB for `IlocItem` — before
/// reading a single entry. An allocation failure there is `handle_alloc_error`,
/// an abort the caller cannot catch, from four attacker-controlled bytes.
#[test]
fn iloc_with_an_absurd_item_count_is_rejected_not_preallocated() {
    let mut body = Vec::new();
    body.push(2u8); // version 2
    body.extend_from_slice(&[0, 0, 0]); // flags
    body.push(0x00); // offset_size=0, length_size=0
    body.push(0x00); // base_offset_size=0, index_size=0
    body.extend_from_slice(&0xFFFF_FFFFu32.to_be_bytes()); // item_count
    // and then nothing: no entries follow.
    let file = file_with_meta_children(&box_with(b"iloc", &body));

    // Must return an error rather than aborting the process or hanging.
    assert!(HeifFile::parse(&file).is_err());
}

/// Same shape for `ipma`, which reads its association count the same way.
#[test]
fn ipma_with_an_absurd_count_is_rejected_not_preallocated() {
    let mut body = Vec::new();
    body.push(1u8); // version 1 -> 32-bit item ids
    body.extend_from_slice(&[0, 0, 0]); // flags
    body.extend_from_slice(&0xFFFF_FFFFu32.to_be_bytes()); // entry count
    let file = file_with_meta_children(&box_with(b"iprp", &box_with(b"ipma", &body)));

    assert!(HeifFile::parse(&file).is_err());
}

/// `base_offset + ext_offset` was a plain `+` on two attacker-controlled
/// `u64`s. With `base_offset = u64::MAX` and any nonzero extent offset this
/// panics in a debug build ("attempt to add with overflow") and silently wraps
/// in release, yielding an offset that may land in-bounds and return the wrong
/// bytes as item data.
#[test]
fn iloc_offset_addition_cannot_overflow() {
    let mut body = Vec::new();
    body.push(1u8); // version 1
    body.extend_from_slice(&[0, 0, 0]); // flags
    body.push(0x88); // offset_size=8, length_size=8
    body.push(0x80); // base_offset_size=8, index_size=0
    body.extend_from_slice(&1u16.to_be_bytes()); // item_count = 1
    body.extend_from_slice(&1u16.to_be_bytes()); // item_id
    body.extend_from_slice(&0u16.to_be_bytes()); // construction_method = 0
    body.extend_from_slice(&0u16.to_be_bytes()); // data_reference_index
    body.extend_from_slice(&u64::MAX.to_be_bytes()); // base_offset
    body.extend_from_slice(&1u16.to_be_bytes()); // extent_count = 1
    body.extend_from_slice(&1u64.to_be_bytes()); // extent offset -> overflow
    body.extend_from_slice(&1u64.to_be_bytes()); // extent length

    let file = file_with_meta_children(&box_with(b"iloc", &body));

    // The only requirement is that this does not panic. Whether it parses and
    // then errors at `item_data`, or errors during parse, is immaterial.
    match HeifFile::parse(&file) {
        Err(_) => {}
        Ok(f) => {
            for item in f.items() {
                let _ = f.item_data(item.id);
            }
        }
    }
}

/// A large but not absurd count, with no entries behind it, must also error
/// rather than allocate proportionally to the claim.
#[test]
fn iloc_count_beyond_the_body_is_rejected() {
    let mut body = Vec::new();
    body.push(0u8);
    body.extend_from_slice(&[0, 0, 0]);
    body.push(0x44); // offset_size=4, length_size=4
    body.push(0x00);
    body.extend_from_slice(&10_000u16.to_be_bytes()); // claims 10k items
    let file = file_with_meta_children(&box_with(b"iloc", &body));
    assert!(HeifFile::parse(&file).is_err());
}

/// An item whose extent length claims more bytes than the file holds must be
/// reported, never sliced.
#[test]
fn extent_longer_than_the_file_is_reported() {
    let mut body = Vec::new();
    body.push(1u8);
    body.extend_from_slice(&[0, 0, 0]);
    body.push(0x44);
    body.push(0x00);
    body.extend_from_slice(&1u16.to_be_bytes());
    body.extend_from_slice(&1u16.to_be_bytes()); // item_id 1
    body.extend_from_slice(&0u16.to_be_bytes()); // construction_method 0
    body.extend_from_slice(&0u16.to_be_bytes()); // data_reference_index
    body.extend_from_slice(&1u16.to_be_bytes()); // extent_count
    body.extend_from_slice(&0u32.to_be_bytes()); // offset 0
    body.extend_from_slice(&0xFFFF_FF00u32.to_be_bytes()); // length ~4 GiB

    let file = file_with_meta_children(&box_with(b"iloc", &body));
    if let Ok(f) = HeifFile::parse(&file) {
        assert!(
            f.item_data(1).is_err(),
            "an extent longer than the file must not resolve"
        );
    }
}
