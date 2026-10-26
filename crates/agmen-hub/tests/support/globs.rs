//! Lane globs shared by the hub's unit tests (`src/lanes_tests.rs`, through
//! `#[path]`) and its integration tests.

/// A distinct anchored literal glob of nearly `max_bytes` bytes (pass the
/// hub's 256-byte glob limit). Its many distinct bytes give it a DFA of
/// about 260 KiB, under the 1 MiB per-glob limit.
pub fn wide_glob(seed: usize, max_bytes: usize) -> String {
    let chars: Vec<char> = ('0'..='9')
        .chain('a'..='z')
        .chain('A'..='Z')
        .chain('\u{a1}'..='\u{ff}')
        .collect();
    let mut glob = String::from("/");
    for i in seed.. {
        let next = chars[i % chars.len()];
        if glob.len() + next.len_utf8() > max_bytes {
            break;
        }
        glob.push(next);
    }
    glob
}
