use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use uuid::Uuid;

use super::workroot::clear_stale;

const CASES: u64 = 300;

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ferrfleet-props-{tag}-{}", Uuid::new_v4()));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn populate(rng: &mut Rng, dir: &Path, depth: u32) {
    for _ in 0..rng.below(4) {
        let name = Uuid::from_u128(u128::from(rng.next()) << 64 | u128::from(rng.next()));
        let path = dir.join(name.to_string());
        if depth < 2 && rng.below(2) == 0 {
            fs::create_dir(&path).unwrap();
            populate(rng, &path, depth + 1);
        } else {
            fs::write(&path, b"x").unwrap();
        }
    }
}

fn snapshot(dir: &Path) -> BTreeSet<PathBuf> {
    let mut found = BTreeSet::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        for entry in fs::read_dir(&next).unwrap().filter_map(Result::ok) {
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                pending.push(path.clone());
            }
            found.insert(path.strip_prefix(dir).unwrap().to_path_buf());
        }
    }
    found
}

fn run_directories(rng: &mut Rng, root: &Path) -> Vec<PathBuf> {
    (0..rng.below(5))
        .map(|_| {
            let dir = root.join(Uuid::new_v4().to_string());
            fs::create_dir(&dir).unwrap();
            populate(rng, &dir, 1);
            dir
        })
        .collect()
}

fn foreign_entries(rng: &mut Rng, root: &Path) {
    for index in 0..rng.below(4) {
        let dir = root.join(format!("project-{index}"));
        fs::create_dir(&dir).unwrap();
        populate(rng, &dir, 1);
    }
    fs::write(root.join("stray-file"), b"x").unwrap();
    fs::write(root.join(Uuid::new_v4().to_string() + ".log"), b"x").unwrap();
    fs::write(root.join(Uuid::new_v4().to_string()), b"x").unwrap();
}

#[test]
fn clear_stale_removes_exactly_the_run_directories() {
    for seed in 1..=CASES {
        let mut rng = Rng::new(seed);
        let parent = scratch("exact");
        let root = parent.join("work");
        fs::create_dir(&root).unwrap();
        fs::write(parent.join("sibling"), b"s").unwrap();
        foreign_entries(&mut rng, &root);
        let kept = snapshot(&root);
        let runs = run_directories(&mut rng, &root);
        assert!(runs.iter().all(|run| run.is_dir()));

        clear_stale(&root);

        assert!(root.is_dir(), "seed {seed}: root removed");
        assert_eq!(snapshot(&root), kept, "seed {seed}: wrong entries removed");
        assert_eq!(fs::read(parent.join("sibling")).unwrap(), b"s");
        fs::remove_dir_all(&parent).unwrap();
    }
}

#[test]
fn clear_stale_is_idempotent() {
    for seed in 1..=CASES {
        let mut rng = Rng::new(seed);
        let root = scratch("idempotent");
        foreign_entries(&mut rng, &root);
        run_directories(&mut rng, &root);

        clear_stale(&root);
        let once = snapshot(&root);
        clear_stale(&root);

        assert_eq!(snapshot(&root), once, "seed {seed}: not idempotent");
        fs::remove_dir_all(&root).unwrap();
    }
}

#[cfg(unix)]
#[test]
fn clear_stale_never_follows_a_symlink_out_of_the_root() {
    for seed in 1..=CASES {
        let mut rng = Rng::new(seed);
        let root = scratch("root");
        let outside = scratch("outside");
        populate(&mut rng, &outside, 0);
        fs::write(outside.join("keep"), b"x").unwrap();
        let before = snapshot(&outside);
        std::os::unix::fs::symlink(&outside, root.join(Uuid::new_v4().to_string())).unwrap();
        std::os::unix::fs::symlink(&root, root.join("loop")).unwrap();
        run_directories(&mut rng, &root);

        clear_stale(&root);

        assert_eq!(
            snapshot(&outside),
            before,
            "seed {seed}: outside tree changed"
        );
        assert!(root.is_dir());
        fs::remove_dir_all(&root).unwrap();
        fs::remove_dir_all(&outside).unwrap();
    }
}

#[test]
fn clear_stale_tolerates_degenerate_roots() {
    let dir = scratch("degenerate");
    clear_stale(&dir.join("missing"));
    assert!(!dir.join("missing").exists());
    fs::write(dir.join("file"), b"f").unwrap();
    clear_stale(&dir.join("file"));
    assert_eq!(fs::read(dir.join("file")).unwrap(), b"f");
    fs::create_dir(dir.join("empty")).unwrap();
    clear_stale(&dir.join("empty"));
    assert!(dir.join("empty").is_dir());
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn clear_stale_keeps_directories_not_named_in_the_canonical_form() {
    let root = scratch("names");
    let id = Uuid::new_v4();
    let others = [
        id.simple().to_string(),
        id.braced().to_string(),
        id.urn().to_string(),
        id.hyphenated().to_string().to_uppercase(),
    ];
    for name in &others {
        fs::create_dir(root.join(name)).unwrap();
    }
    let run = root.join(id.hyphenated().to_string());
    fs::create_dir(&run).unwrap();

    clear_stale(&root);

    assert!(!run.exists());
    for name in &others {
        assert!(root.join(name).is_dir(), "{name} was swept");
    }
    fs::remove_dir_all(&root).unwrap();
}
