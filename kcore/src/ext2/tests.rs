//! ext2 tests against the reference tools: images made by mke2fs are read, and images changed by
//! this driver must pass `e2fsck -fn`. Skipped (with a note) when e2fsprogs is not installed.
use super::*;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

struct Mem(RefCell<Vec<u8>>);

impl Disk for Mem {
    fn read(&self, off: u64, buf: &mut [u8]) -> KResult<()> {
        let m = self.0.borrow();
        let o = off as usize;
        if o + buf.len() > m.len() {
            return Err(EIO);
        }
        buf.copy_from_slice(&m[o..o + buf.len()]);
        Ok(())
    }
    fn write(&self, off: u64, buf: &[u8]) -> KResult<()> {
        let mut m = self.0.borrow_mut();
        let o = off as usize;
        if o + buf.len() > m.len() {
            return Err(EIO);
        }
        m[o..o + buf.len()].copy_from_slice(buf);
        Ok(())
    }
}

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("citadel-ext2-{}-{}", std::process::id(), name));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn have_tools() -> bool {
    let ok = Command::new("mke2fs").arg("-V").output().is_ok() && Command::new("e2fsck").arg("-V").output().is_ok();
    if !ok {
        eprintln!("e2fsprogs not installed: skipping");
    }
    ok
}

fn mkfs(name: &str, block: u32, mib: u32, populate: Option<&Path>) -> Vec<u8> {
    let dir = scratch(name);
    let img = dir.join("fs.img");
    let mut c = Command::new("mke2fs");
    c.args(["-q", "-F", "-t", "ext2", "-b", &block.to_string(), "-N", "2048"]);
    if let Some(p) = populate {
        c.arg("-d").arg(p);
    }
    c.arg(&img).arg(format!("{mib}M"));
    let st = c.status().expect("mke2fs");
    assert!(st.success(), "mke2fs failed");
    std::fs::read(img).unwrap()
}

fn fsck(name: &str, data: &[u8]) {
    let dir = scratch(&format!("{name}-fsck"));
    let img = dir.join("fs.img");
    std::fs::write(&img, data).unwrap();
    let out = Command::new("e2fsck").arg("-fn").arg(&img).output().expect("e2fsck");
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success(), "e2fsck found problems:\n{text}");
}

fn mount(data: Vec<u8>) -> Ext2<Mem> {
    let mut fs = Ext2::mount(Mem(RefCell::new(data)), false).expect("mount");
    fs.clock = || 1_700_000_000;
    fs
}

fn resolve(fs: &mut Ext2<Mem>, path: &str) -> KResult<u32> {
    let mut ino = ROOT_INO;
    for c in path.split('/').filter(|c| !c.is_empty()) {
        ino = fs.lookup(ino, c)?;
    }
    Ok(ino)
}

fn read_all(fs: &mut Ext2<Mem>, ino: u32) -> Vec<u8> {
    let size = fs.read_inode(ino).unwrap().size() as usize;
    let mut buf = vec![0u8; size];
    assert_eq!(fs.read(ino, 0, &mut buf).unwrap(), size);
    buf
}

fn pattern(n: usize, seed: u32) -> Vec<u8> {
    let mut x = seed.wrapping_mul(2654435761).max(1);
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x as u8
        })
        .collect()
}

#[test]
fn reads_an_image_made_by_mke2fs() {
    if !have_tools() {
        return;
    }
    let src = scratch("src");
    std::fs::write(src.join("hello.txt"), b"hello, ext2\n").unwrap();
    std::fs::write(src.join("big.bin"), pattern(700_000, 7)).unwrap(); // double indirect at 1 KiB
    std::fs::create_dir_all(src.join("a/b/c")).unwrap();
    std::fs::write(src.join("a/b/c/deep.txt"), b"deep").unwrap();
    std::os::unix::fs::symlink("hello.txt", src.join("short")).unwrap();
    let long_target = "x".repeat(100);
    std::os::unix::fs::symlink(&long_target, src.join("long")).unwrap();
    for block in [1024, 4096] {
        let mut fs = mount(mkfs(&format!("read{block}"), block, 16, Some(&src)));
        assert_eq!(fs.block_size, block);
        let h = resolve(&mut fs, "hello.txt").unwrap();
        assert_eq!(read_all(&mut fs, h), b"hello, ext2\n");
        let b = resolve(&mut fs, "big.bin").unwrap();
        assert_eq!(read_all(&mut fs, b), pattern(700_000, 7));
        let d = resolve(&mut fs, "a/b/c/deep.txt").unwrap();
        assert_eq!(read_all(&mut fs, d), b"deep");
        let s = resolve(&mut fs, "short").unwrap();
        assert_eq!(fs.readlink(s).unwrap(), "hello.txt");
        let l = resolve(&mut fs, "long").unwrap();
        assert_eq!(fs.readlink(l).unwrap(), long_target);
        let mut names: Vec<String> = fs.readdir(ROOT_INO).unwrap().into_iter().map(|e| e.name).collect();
        names.sort();
        assert!(names.iter().any(|n| n == "lost+found"));
        for n in [".", "..", "a", "big.bin", "hello.txt", "long", "short"] {
            assert!(names.iter().any(|x| x == n), "missing {n}: {names:?}");
        }
        // Partial reads at odd offsets.
        let mut buf = vec![0u8; 5000];
        assert_eq!(fs.read(b, 123_457, &mut buf).unwrap(), 5000);
        assert_eq!(buf, pattern(700_000, 7)[123_457..128_457]);
    }
}

#[test]
fn changes_pass_e2fsck_and_survive_a_remount() {
    if !have_tools() {
        return;
    }
    for block in [1024u32, 4096] {
        let name = format!("write{block}");
        let mut fs = mount(mkfs(&name, block, 32, None));
        let mut model: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        let docs = fs.mkdir(ROOT_INO, "docs", 0o755).unwrap();
        let deep = fs.mkdir(docs, "deep", 0o700).unwrap();
        // Files of awkward sizes: empty, tiny, block boundaries, indirect, double indirect.
        for (i, size) in [0usize, 1, 1023, 1024, 1025, 4096 * 12 + 1, 300_000, 2_000_000].iter().enumerate() {
            let ino = fs.create(docs, &format!("f{i}"), S_IFREG | 0o644, 0).unwrap();
            let data = pattern(*size, i as u32 + 1);
            // Write in uneven pieces.
            let mut off = 0;
            let mut step = 777;
            while off < data.len() {
                let n = step.min(data.len() - off);
                assert_eq!(fs.write(ino, off as u64, &data[off..off + n]).unwrap(), n);
                off += n;
                step = step * 3 % 50_000 + 1;
            }
            model.insert(format!("docs/f{i}"), data);
        }
        // Many entries: several directory blocks, then remove every other one.
        for i in 0..400 {
            let ino = fs.create(deep, &format!("entry-with-a-longish-name-{i:04}"), S_IFREG | 0o600, 0).unwrap();
            fs.write(ino, 0, format!("{i}").as_bytes()).unwrap();
        }
        for i in (0..400).step_by(2) {
            let (ino, links) = fs.unlink(deep, &format!("entry-with-a-longish-name-{i:04}")).unwrap();
            assert_eq!(links, 0);
            fs.release(ino).unwrap();
        }
        for i in (1..400).step_by(2) {
            model.insert(format!("docs/deep/entry-with-a-longish-name-{i:04}"), format!("{i}").into_bytes());
        }
        // Truncation: shrink into the indirect area, then grow (the gap must read as zeros).
        let f6 = fs.lookup(docs, "f6").unwrap();
        fs.truncate(f6, 20_000).unwrap();
        fs.truncate(f6, 50_000).unwrap();
        let mut m = model["docs/f6"][..20_000].to_vec();
        m.resize(50_000, 0);
        model.insert("docs/f6".into(), m);
        // Sparse write far out (holes in the middle).
        let sp = fs.create(ROOT_INO, "sparse", S_IFREG | 0o644, 0).unwrap();
        fs.write(sp, 5_000_000, b"tail").unwrap();
        let mut sdata = vec![0u8; 5_000_004];
        sdata[5_000_000..].copy_from_slice(b"tail");
        model.insert("sparse".into(), sdata);
        // Symbolic links, hard links, renames.
        fs.symlink(ROOT_INO, "ln-short", "docs/f1").unwrap();
        fs.symlink(ROOT_INO, "ln-long", &"y/".repeat(70)).unwrap();
        let f2 = fs.lookup(docs, "f2").unwrap();
        fs.link(ROOT_INO, "hard", f2).unwrap();
        model.insert("hard".into(), model["docs/f2"].clone());
        assert_eq!(fs.read_inode(f2).unwrap().links(), 2);
        // Rename a file over another (the replaced one loses its last link).
        let orphan = fs.rename(docs, "f3", docs, "f4").unwrap().expect("replaced inode");
        fs.release(orphan).unwrap();
        let d3 = model.remove("docs/f3").unwrap();
        model.insert("docs/f4".into(), d3);
        // Move a directory to another parent.
        let other = fs.mkdir(ROOT_INO, "other", 0o755).unwrap();
        assert!(fs.rename(docs, "deep", other, "moved").unwrap().is_none());
        let moved: Vec<(String, Vec<u8>)> = model.iter().filter(|(k, _)| k.starts_with("docs/deep/")).map(|(k, v)| (k.replace("docs/deep/", "other/moved/"), v.clone())).collect();
        model.retain(|k, _| !k.starts_with("docs/deep/"));
        model.extend(moved);
        assert_eq!(fs.lookup(deep, "..").unwrap(), other);
        assert_eq!(fs.rename(ROOT_INO, "other", other, "x"), Err(EINVAL)); // into itself
        // rmdir rules.
        assert_eq!(fs.rmdir(ROOT_INO, "other"), Err(ENOTEMPTY));
        let empty = fs.mkdir(ROOT_INO, "empty", 0o755).unwrap();
        let _ = empty;
        fs.rmdir(ROOT_INO, "empty").unwrap();
        assert_eq!(fs.lookup(ROOT_INO, "empty"), Err(ENOENT));
        assert_eq!(fs.create(ROOT_INO, "sparse", S_IFREG | 0o644, 0), Err(EEXIST));
        fs.sync().unwrap();

        let image = fs.dev.0.into_inner();
        fsck(&name, &image);

        // Remount and compare everything with the model.
        let mut fs = mount(image);
        for (path, data) in &model {
            let ino = resolve(&mut fs, path).unwrap_or_else(|e| panic!("{path}: {e}"));
            assert_eq!(&read_all(&mut fs, ino), data, "{path}");
        }
        let ls = resolve(&mut fs, "ln-short").unwrap();
        assert_eq!(fs.readlink(ls).unwrap(), "docs/f1");
        let ll = resolve(&mut fs, "ln-long").unwrap();
        assert_eq!(fs.readlink(ll).unwrap(), "y/".repeat(70));
        assert_eq!(resolve(&mut fs, "docs/f3"), Err(ENOENT));
        let moved_dir = resolve(&mut fs, "other/moved").unwrap();
        let n = fs.readdir(moved_dir).unwrap().len();
        assert_eq!(n, 200 + 2);
    }
}

#[test]
fn rejects_what_it_cannot_handle() {
    if !have_tools() {
        return;
    }
    let dir = scratch("ext4");
    let img = dir.join("fs.img");
    let st = Command::new("mke2fs").args(["-q", "-F", "-t", "ext4"]).arg(&img).arg("8M").status().unwrap();
    assert!(st.success());
    let data = std::fs::read(&img).unwrap();
    assert!(Ext2::mount(Mem(RefCell::new(data)), false).is_err(), "ext4 (extents) must be refused");
    assert!(Ext2::mount(Mem(RefCell::new(vec![0u8; 1 << 20])), false).is_err());
}

#[test]
fn fills_up_cleanly() {
    if !have_tools() {
        return;
    }
    let mut fs = mount(mkfs("full", 1024, 2, None));
    let ino = fs.create(ROOT_INO, "big", S_IFREG | 0o644, 0).unwrap();
    let chunk = vec![0xabu8; 64 * 1024];
    let mut off = 0u64;
    loop {
        match fs.write(ino, off, &chunk) {
            Ok(n) if n == chunk.len() => off += n as u64,
            Ok(n) => {
                off += n as u64;
                break;
            }
            Err(e) => {
                assert_eq!(e, ENOSPC);
                break;
            }
        }
    }
    assert!(off > 1_000_000, "wrote only {off}");
    assert_eq!(fs.free_blocks(), 0);
    // Deleting it gives the space back.
    let (ino2, _) = fs.unlink(ROOT_INO, "big").unwrap();
    assert_eq!(ino2, ino);
    fs.release(ino).unwrap();
    fs.sync().unwrap();
    assert!(fs.free_blocks() > 1000);
    fsck("full", &fs.dev.0.into_inner());
}
