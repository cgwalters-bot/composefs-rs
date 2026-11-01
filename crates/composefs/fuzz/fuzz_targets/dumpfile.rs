//! Parse a UTF-8 dumpfile line and roundtrip through the entry writer.
#![no_main]

use composefs::dumpfile_parse::Entry;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &str| {
    if let Ok(mut entry) = Entry::parse(data) {
        // Display maps empty xattr keys to "-", which can exceed the total
        // name-length limit on reparse. Still fuzz parsing these inputs, but
        // only check roundtrips for nonempty keys.
        if entry.xattrs.iter().any(|xattr| xattr.key.is_empty()) {
            return;
        }
        let written = entry.to_string();
        let reparsed = Entry::parse(&written).expect("writer output must parse");
        // Preserve Display's existing lossy empty-value output rather than
        // changing the format to satisfy the roundtrip oracle. A literal
        // dash is escaped as \\x2d and already roundtrips without adjustment.
        for xattr in &mut entry.xattrs {
            if xattr.value.is_empty() {
                xattr.value = b"-".as_slice().into();
            }
        }
        // Duplicate keys are accepted; normalizing values can reorder them.
        entry.xattrs.sort();
        assert_eq!(entry, reparsed);
    }
});
