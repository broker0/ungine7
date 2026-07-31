use files::mul::{IndexEntry, MulIndex};
use u_io::{BinaryWriter, Encode, LE};

/// Build a [`MulIndex`] from a slice of entries (shared test helper).
pub fn make_index(entries: &[IndexEntry]) -> MulIndex {
    let mut w = BinaryWriter::<LE>::new();
    for e in entries {
        e.offset.encode(&mut w);
        e.length.encode(&mut w);
        e.extra.encode(&mut w);
    }
    MulIndex::from_bytes(&w.finish()).unwrap()
}
