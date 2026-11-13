//! Path helpers.
extern crate alloc;
use alloc::string::String;
use alloc::vec::Vec;

/// Joins `path` to `cwd` (when relative) and removes ".", ".." and repeated slashes, purely
/// textually (symbolic links are resolved by the VFS while walking).
pub fn normalize(cwd: &str, path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    let full: [&str; 2] = if path.starts_with('/') { ["", path] } else { [cwd, path] };
    for piece in full {
        for c in piece.split('/') {
            match c {
                "" | "." => {}
                ".." => {
                    parts.pop();
                }
                c => parts.push(c),
            }
        }
    }
    let mut s = String::from("/");
    s.push_str(&parts.join("/"));
    s
}

/// True if `p` is already what `normalize` would return: absolute, no empty, "." or ".."
/// components and no trailing slash (except "/" itself). Lets callers skip an allocation.
pub fn is_normal(p: &str) -> bool {
    if !p.starts_with('/') {
        return false;
    }
    if p == "/" {
        return true;
    }
    if p.ends_with('/') {
        return false;
    }
    p[1..].split('/').all(|c| !c.is_empty() && c != "." && c != "..")
}

/// ("/a/b", "c") for "/a/b/c"; ("/", "x") for "/x".
pub fn split_last(abs: &str) -> (&str, &str) {
    match abs.rfind('/') {
        Some(0) => ("/", &abs[1..]),
        Some(i) => (&abs[..i], &abs[i + 1..]),
        None => ("/", abs),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_paths() {
        for p in ["/", "/a", "/a/b", "/bin/sh", "/a.b/..c"] {
            assert!(is_normal(p), "{p}");
            assert_eq!(normalize("/", p), p);
        }
        for p in ["", "a", "/a/", "//a", "/a//b", "/./a", "/a/.", "/a/../b", "/.."] {
            assert!(!is_normal(p), "{p}");
        }
    }

    #[test]
    fn normalizes() {
        assert_eq!(normalize("/home/u", "docs/./a.txt"), "/home/u/docs/a.txt");
        assert_eq!(normalize("/home/u", "../../../etc//passwd"), "/etc/passwd");
        assert_eq!(normalize("/x", "/abs/../y/"), "/y");
        assert_eq!(normalize("/", ""), "/");
        assert_eq!(normalize("/", ".."), "/");
    }

    #[test]
    fn splits() {
        assert_eq!(split_last("/a/b/c"), ("/a/b", "c"));
        assert_eq!(split_last("/x"), ("/", "x"));
    }
}
