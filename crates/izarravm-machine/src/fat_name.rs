//! Shared 8.3 short-name folding for the synthesized FAT volumes. The FAT12
//! floppy (fat12.rs) and the FAT32 hard disk (fat32.rs) both turn host file
//! names into the canonical 11-byte 8.3 directory-entry name through this
//! module, so the folding rules live in one place rather than being duplicated.

use std::path::Path;

/// Compose a unique 8.3 name for `path`, recording it in `used` so later
/// siblings in the same directory collide against it and get a `~n` suffix.
pub(crate) fn unique_name(path: &Path, is_dir: bool, used: &mut Vec<[u8; 11]>) -> [u8; 11] {
    let raw = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let base = make_83(raw, is_dir);
    if !used.iter().any(|u| u == &base) {
        used.push(base);
        return base;
    }
    // Resolve with ~1, ~2, ... suffixes on the base name.
    for n in 1..=999u32 {
        let candidate = make_83_with_tilde(raw, is_dir, n);
        if !used.iter().any(|u| u == &candidate) {
            used.push(candidate);
            return candidate;
        }
    }
    // Exhausted; reuse the base (extremely unlikely with a 224/512-entry dir).
    used.push(base);
    base
}

/// Map a host name to a padded 8.3 field: 8 bytes name, 3 bytes extension,
/// uppercased, illegal characters stripped.
fn make_83(raw: &str, is_dir: bool) -> [u8; 11] {
    let (stem, ext) = split_stem_ext(raw, is_dir);
    pack_83(&stem, &ext)
}

/// Like `make_83`, but force a `~n` suffix into the stem (truncating to fit).
fn make_83_with_tilde(raw: &str, is_dir: bool, n: u32) -> [u8; 11] {
    let (stem, ext) = split_stem_ext(raw, is_dir);
    let tail = format!("~{n}");
    let keep = 8usize.saturating_sub(tail.len());
    let mut stem2: String = stem.chars().take(keep).collect();
    stem2.push_str(&tail);
    pack_83(&stem2, &ext)
}

/// Split a raw name into a cleaned uppercase stem and extension.
fn split_stem_ext(raw: &str, is_dir: bool) -> (String, String) {
    // Directories ignore any dot for the extension split; treat the whole name
    // as the stem so "my.dir" does not get a bogus extension.
    let (stem_raw, ext_raw) = if is_dir {
        (raw, "")
    } else {
        match raw.rfind('.') {
            Some(i) if i > 0 => (&raw[..i], &raw[i + 1..]),
            _ => (raw, ""),
        }
    };
    let stem = clean(stem_raw);
    let ext = clean(ext_raw);
    (stem, ext)
}

/// Strip characters illegal in an 8.3 name and uppercase the rest.
fn clean(s: &str) -> String {
    s.chars()
        .filter_map(|c| {
            let c = c.to_ascii_uppercase();
            if is_legal_83(c) {
                Some(c)
            } else if c == ' ' || c == '.' {
                None
            } else {
                // Replace any other stray byte with an underscore.
                Some('_')
            }
        })
        .collect()
}

fn is_legal_83(c: char) -> bool {
    c.is_ascii_alphanumeric()
        || matches!(
            c,
            '!' | '#'
                | '$'
                | '%'
                | '&'
                | '\''
                | '('
                | ')'
                | '-'
                | '@'
                | '^'
                | '_'
                | '`'
                | '{'
                | '}'
                | '~'
        )
}

/// Pad a stem (<=8) and extension (<=3) into the canonical 11-byte field.
fn pack_83(stem: &str, ext: &str) -> [u8; 11] {
    let mut out = [b' '; 11];
    let stem: Vec<u8> = stem.bytes().take(8).collect();
    let ext: Vec<u8> = ext.bytes().take(3).collect();
    if stem.is_empty() {
        // A name that cleaned away to nothing still needs a stem.
        out[0] = b'_';
    } else {
        out[..stem.len()].copy_from_slice(&stem);
    }
    out[8..8 + ext.len()].copy_from_slice(&ext);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_name_and_extension_uppercased_and_padded() {
        let n = unique_name(Path::new("readme.txt"), false, &mut Vec::new());
        assert_eq!(&n, b"README  TXT");
    }

    #[test]
    fn a_directory_name_with_a_dot_keeps_it_in_the_stem() {
        // A directory must not split a bogus extension off the dot.
        let n = unique_name(Path::new("my.dir"), true, &mut Vec::new());
        assert_eq!(&n, b"MYDIR      ");
    }

    #[test]
    fn siblings_collide_into_tilde_suffixes() {
        let mut used = Vec::new();
        let a = unique_name(Path::new("longname.txt"), false, &mut used);
        let b = unique_name(Path::new("longname.txt"), false, &mut used);
        assert_eq!(&a, b"LONGNAMETXT");
        assert_eq!(&b, b"LONGNA~1TXT", "the second sibling gets a ~1 suffix");
        assert_ne!(a, b);
    }

    #[test]
    fn illegal_characters_become_underscores() {
        let n = unique_name(Path::new("a+b=c.dat"), false, &mut Vec::new());
        assert_eq!(&n, b"A_B_C   DAT");
    }
}
