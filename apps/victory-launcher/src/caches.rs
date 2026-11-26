//! The shader cache and the pipeline files below the cache folder: what is
//! there, and clearing them. Nothing outside the cache folder is touched.
//!
//! `<cache>\shaders\xx\...`      one file per translated shader
//! `<cache>\pipelines\`          `known-pipelines.bin`, `known-shaders.bin`
//!                               (every pipeline the game ever made, written
//!                               down by runs), `early-pipelines.txt` (those
//!                               runs drew with), `host-pipelines*.bin` (the
//!                               pipelines as the driver built them),
//!                               `warm-done.txt`, `warm-bad.txt`,
//!                               `slow-pipelines.txt` (what `warm` did)
//! `<cache>\images\`             the game's program, prepared

use std::path::{Path, PathBuf};

use crate::paths;

pub const SHADERS_DIR: &str = "shaders";
pub const PIPELINES_DIR: &str = "pipelines";

/// What the driver built and what `warm` noted: made again by a warm-up.
const BUILT_FILES: &[&str] = &[
    "host-pipelines.bin",
    "host-pipelines.rejected.bin",
    "host-pipelines.next",
    "warm-done.txt",
    "warm-bad.txt",
    "slow-pipelines.txt",
];
/// What runs wrote down of the game's pipelines: only playing brings it back.
const BOOK_FILES: &[&str] = &[
    "known-pipelines.bin",
    "known-shaders.bin",
    "early-pipelines.txt",
];

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    pub shader_files: u64,
    pub shader_bytes: u64,
    /// All of `pipelines\`.
    pub pipeline_bytes: u64,
    /// The driver-built library alone.
    pub library_bytes: u64,
    /// Pipelines earlier sessions drew with (what a warm-up makes).
    pub used: u64,
    /// Pipelines warm-ups have made so far (for any build).
    pub warmed: u64,
    /// Pipelines the compiler gave up on.
    pub bad: u64,
    pub image_bytes: u64,
}

fn tree(dir: &Path) -> (u64, u64) {
    let (mut files, mut bytes) = (0, 0);
    let mut stack = vec![dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                stack.push(entry.path());
            } else {
                files += 1;
                bytes += meta.len();
            }
        }
    }
    (files, bytes)
}

fn lines(path: &Path) -> u64 {
    std::fs::read(path).map_or(0, |data| {
        data.split(|&b| b == b'\n')
            .filter(|l| !l.trim_ascii().is_empty())
            .count() as u64
    })
}

/// Looks at the cache folder (walks the shader files: a second or two on a
/// full cache, so call it off the window's thread).
pub fn scan(cache: &Path) -> Status {
    let (shader_files, shader_bytes) = tree(&cache.join(SHADERS_DIR));
    let pipelines = cache.join(PIPELINES_DIR);
    let (_, pipeline_bytes) = tree(&pipelines);
    let size = |name: &str| std::fs::metadata(pipelines.join(name)).map_or(0, |m| m.len());
    // (The done list has `<build> <key>` lines; a key made for two builds
    // counts twice, which is what was done.)
    Status {
        shader_files,
        shader_bytes,
        pipeline_bytes,
        library_bytes: size("host-pipelines.bin"),
        used: lines(&pipelines.join("early-pipelines.txt")),
        warmed: lines(&pipelines.join("warm-done.txt")),
        bad: lines(&pipelines.join("warm-bad.txt")),
        image_bytes: tree(&cache.join("images")).1,
    }
}

/// Why this cache folder must not be cleared, if so: it has to be a real
/// folder of its own, not a drive, not the game, not the program.
pub fn refuse_reason(cache: &Path, game_dir: &str, program_dir: &Path) -> Option<String> {
    if !cache.is_absolute() {
        return Some(format!(
            "The cache folder {} is not a full path.",
            cache.display()
        ));
    }
    if cache.components().count() < 4 {
        return Some(format!(
            "The cache folder {} is a drive or right below one; nothing is deleted there.",
            cache.display()
        ));
    }
    let game = Path::new(game_dir.trim());
    if !game_dir.trim().is_empty()
        && (paths::is_within(cache, game) || paths::is_within(game, cache))
    {
        return Some(format!(
            "The cache folder {} and the game folder {} are inside one another; nothing is deleted there.",
            cache.display(),
            game.display()
        ));
    }
    if paths::is_within(program_dir, cache) {
        return Some(format!(
            "The program is installed inside the cache folder {}; nothing is deleted there.",
            cache.display()
        ));
    }
    None
}

fn remove_file(path: &Path, removed: &mut u64) -> Result<(), String> {
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_file() => {
            std::fs::remove_file(path)
                .map_err(|e| format!("Could not delete {}: {e}", path.display()))?;
            *removed += meta.len();
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Deletes `<cache>\shaders`. Returns the bytes freed.
pub fn clear_shaders(cache: &Path, game_dir: &str, program_dir: &Path) -> Result<u64, String> {
    if let Some(why) = refuse_reason(cache, game_dir, program_dir) {
        return Err(why);
    }
    let dir = cache.join(SHADERS_DIR);
    if !dir.exists() {
        return Ok(0);
    }
    let (_, bytes) = tree(&dir);
    std::fs::remove_dir_all(&dir)
        .map_err(|e| format!("Could not delete {}: {e}", dir.display()))?;
    Ok(bytes)
}

/// Deletes the built pipelines in `<cache>\pipelines` (by name; nothing
/// else in the folder), and with `forget_book` also what runs wrote down.
pub fn clear_pipelines(
    cache: &Path,
    game_dir: &str,
    program_dir: &Path,
    forget_book: bool,
) -> Result<u64, String> {
    if let Some(why) = refuse_reason(cache, game_dir, program_dir) {
        return Err(why);
    }
    let dir = cache.join(PIPELINES_DIR);
    let mut removed = 0;
    for name in BUILT_FILES {
        remove_file(&dir.join(name), &mut removed)?;
    }
    if forget_book {
        for name in BOOK_FILES {
            remove_file(&dir.join(name), &mut removed)?;
        }
    }
    Ok(removed)
}

/// The folders "remove my data too" deletes at uninstall: the cache's own
/// three subfolders, and the saves folder only if it is the launcher's own.
pub fn data_removal_plan(cache: &Path, data: &Path, home: &Path) -> (Vec<PathBuf>, Vec<String>) {
    let (mut delete, mut kept) = (Vec::new(), Vec::new());
    for sub in [SHADERS_DIR, PIPELINES_DIR, "images"] {
        delete.push(cache.join(sub));
    }
    if paths::is_within(data, home) && !paths::same_path(data, home) {
        delete.push(data.to_path_buf());
    } else {
        kept.push(format!(
            "Your saves in {} are outside the launcher's own folder and were left alone.",
            data.display()
        ));
    }
    for sub in ["logs", "updates"] {
        delete.push(home.join(sub));
    }
    (delete, kept)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filled(root: &Path) -> PathBuf {
        let cache = root.join("VIctory").join("cache");
        std::fs::create_dir_all(cache.join("shaders").join("0a")).unwrap();
        std::fs::create_dir_all(cache.join("pipelines")).unwrap();
        std::fs::create_dir_all(cache.join("images")).unwrap();
        std::fs::write(cache.join("shaders").join("0a").join("one"), [0u8; 100]).unwrap();
        std::fs::write(cache.join("shaders").join("0a").join("two"), [0u8; 50]).unwrap();
        std::fs::write(cache.join("images").join("image"), [0u8; 7]).unwrap();
        let p = cache.join("pipelines");
        std::fs::write(p.join("host-pipelines.bin"), [0u8; 1000]).unwrap();
        std::fs::write(p.join("known-pipelines.bin"), [0u8; 300]).unwrap();
        std::fs::write(p.join("early-pipelines.txt"), "a\nb\nc\n").unwrap();
        std::fs::write(p.join("warm-done.txt"), "f a\nf b\n").unwrap();
        std::fs::write(p.join("warm-bad.txt"), "c hung for 180 s\n").unwrap();
        std::fs::write(p.join("somebody-elses.txt"), "keep").unwrap();
        cache
    }

    #[test]
    fn scan_counts_what_is_there() {
        let dir = tempfile::tempdir().unwrap();
        let cache = filled(dir.path());
        let s = scan(&cache);
        assert_eq!((s.shader_files, s.shader_bytes), (2, 150));
        assert_eq!(s.library_bytes, 1000);
        assert_eq!((s.used, s.warmed, s.bad), (3, 2, 1));
        assert_eq!(s.image_bytes, 7);
        assert!(s.pipeline_bytes > 1300);
        assert_eq!(scan(&dir.path().join("nothing")), Status::default());
    }

    #[test]
    fn clearing_shaders_removes_only_the_shader_folder() {
        let dir = tempfile::tempdir().unwrap();
        let cache = filled(dir.path());
        let program = dir.path().join("program");
        assert_eq!(clear_shaders(&cache, "", &program).unwrap(), 150);
        assert!(!cache.join("shaders").exists());
        assert!(cache.join("pipelines").join("host-pipelines.bin").is_file());
        assert!(cache.join("images").join("image").is_file());
        assert_eq!(clear_shaders(&cache, "", &program).unwrap(), 0);
    }

    #[test]
    fn clearing_pipelines_keeps_the_book_unless_asked_and_strangers_always() {
        let dir = tempfile::tempdir().unwrap();
        let cache = filled(dir.path());
        let program = dir.path().join("program");
        let p = cache.join("pipelines");
        assert!(clear_pipelines(&cache, "", &program, false).unwrap() >= 1000);
        assert!(!p.join("host-pipelines.bin").exists() && !p.join("warm-done.txt").exists());
        assert!(p.join("known-pipelines.bin").is_file() && p.join("early-pipelines.txt").is_file());
        clear_pipelines(&cache, "", &program, true).unwrap();
        assert!(!p.join("known-pipelines.bin").exists() && !p.join("early-pipelines.txt").exists());
        assert!(p.join("somebody-elses.txt").is_file());
        assert!(cache.join("shaders").join("0a").join("one").is_file());
    }

    #[test]
    fn dangerous_cache_folders_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let cache = filled(dir.path());
        let program = dir.path().join("program");
        let game = dir.path().join("game");
        std::fs::create_dir_all(&game).unwrap();
        assert!(refuse_reason(&cache, &game.display().to_string(), &program).is_none());
        // A drive, a relative path, the game inside the cache, the cache inside the game, the program inside the cache.
        assert!(clear_shaders(Path::new(r"C:\"), "", &program).is_err());
        assert!(clear_shaders(Path::new(r"C:\cache"), "", &program).is_err());
        assert!(clear_shaders(Path::new("cache"), "", &program).is_err());
        assert!(
            clear_shaders(&cache, &cache.join("game").display().to_string(), &program).is_err()
        );
        assert!(
            clear_pipelines(
                &game.join("cache"),
                &game.display().to_string(),
                &program,
                true
            )
            .is_err()
        );
        assert!(clear_shaders(&cache, "", &cache.join("program")).is_err());
        assert!(cache.join("shaders").join("0a").join("one").is_file());
    }

    #[test]
    fn data_removal_leaves_saves_kept_elsewhere() {
        let home = Path::new(r"C:\Users\someone\AppData\Local\VIctory");
        let (delete, kept) = data_removal_plan(&home.join("cache"), &home.join("data"), home);
        assert!(
            delete.contains(&home.join("data"))
                && delete.contains(&home.join("cache").join("shaders"))
        );
        assert!(kept.is_empty());
        let (delete, kept) =
            data_removal_plan(Path::new(r"E:\vcache"), Path::new(r"E:\my saves"), home);
        assert!(!delete.iter().any(|p| p.starts_with(r"E:\my saves")));
        assert!(delete.contains(&PathBuf::from(r"E:\vcache\pipelines")));
        assert_eq!(kept.len(), 1);
        assert!(
            delete
                .iter()
                .all(|p| p != home && p != Path::new(r"E:\vcache"))
        );
    }
}
