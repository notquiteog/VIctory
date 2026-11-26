//! Updates from GitHub Releases: look, download, verify, stage, swap.
//!
//! 1. `GET https://api.github.com/repos/<repo>/releases/latest` (the
//!    Stable channel) or `.../releases/tags/nightly` (the Latest build
//!    channel: a rolling pre-release rebuilt from the head of `main`). No
//!    token, a generic User-Agent, nothing about the user.
//! 2. If it is newer than this program (Stable: by version; Latest build:
//!    by the commit its `build.json` names): the asset `*-windows-x64.zip`
//!    is downloaded to `%LOCALAPPDATA%\VIctory\updates`. Going from a
//!    latest build back to Stable offers the release whatever its age.
//! 3. Its SHA-256 must equal the one published with the release (the
//!    `SHA256SUMS` asset, else the digest GitHub reports for the asset).
//!    No published hash, or another one: refused, nothing is unpacked.
//! 4. It is unpacked to `updates\staged` and `updates\pending.txt` names it.
//! 5. At the next start of the installed launcher the staged files take
//!    the place of the program files (the old ones are renamed first: a
//!    running program can be renamed, not overwritten), the launcher starts
//!    itself again, and the renamed files are deleted at the start after.
//!
//! Settings, saves and caches are not in the program folder and are not
//! touched; the caches are marked as possibly out of date.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::paths;
use crate::settings::Channel;

pub const REPO: &str = "notquiteog/victory-nvidia";
/// Another place to look, for tests: a URL that answers with release JSON,
/// or a folder holding `latest.json` and the files it names.
pub const FEED_ENV: &str = "VICTORY_UPDATE_FEED";
const USER_AGENT: &str = "victory-launcher";
const ASSET_SUFFIX: &str = "-windows-x64.zip";
const SUMS_NAME: &str = "SHA256SUMS";
/// Renamed program files waiting to be deleted.
const OLD_SUFFIX: &str = ".old";
const DAY: u64 = 24 * 60 * 60;

/// The tag of the rolling pre-release the Latest build channel follows.
pub const NIGHTLY_TAG: &str = "nightly";
/// The asset that says which commit a release was built from.
const BUILD_NAME: &str = "build.json";
/// The launcher under the name the download gives it.
pub const SETUP_EXE: &str = "VI-Setup.exe";

/// The launcher's own version.
pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// The commit this launcher was built from ("unknown" outside git).
pub fn current_commit() -> &'static str {
    env!("VICTORY_COMMIT")
}

/// That commit's date, `2026-11-26`.
pub fn current_date() -> &'static str {
    env!("VICTORY_COMMIT_DATE")
}

/// `VI 0.1.0 (abc1234, 2026-11-26)`.
pub fn describe_build(version: &str, commit: &str, date: &str) -> String {
    format!(
        "{} {version} ({}, {date})",
        paths::DISPLAY_NAME,
        &commit[..commit.len().min(7)]
    )
}

/// Which commit a release was built from (its `build.json`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BuildInfo {
    pub version: String,
    pub commit: String,
    pub date: String,
}

pub fn parse_build(json: &str) -> Option<BuildInfo> {
    let v: serde_json::Value = serde_json::from_str(json.trim_start_matches('\u{feff}')).ok()?;
    let text = |name: &str| v[name].as_str().map(|t| t.trim().to_owned());
    let commit =
        text("commit").filter(|c| c.len() >= 7 && c.bytes().all(|b| b.is_ascii_hexdigit()))?;
    Some(BuildInfo {
        version: text("version").unwrap_or_default(),
        commit,
        date: text("date").unwrap_or_default(),
    })
}

/// What to do about a release that was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Offer {
    /// Nothing: this program is as new, in these words.
    UpToDate(String),
    /// It is newer: offer it.
    Newer,
    /// It is the stable release and this program is a latest build:
    /// offer it, saying that it may be older.
    BackToStable,
}

/// This program, as the updater sees it.
#[derive(Debug, Clone, Copy)]
pub struct Own<'a> {
    pub version: &'a str,
    pub commit: &'a str,
    pub date: &'a str,
    /// The channel the installed program came from.
    pub from: Channel,
}

impl Own<'static> {
    pub fn current(from: Channel) -> Self {
        Own {
            version: current_version(),
            commit: current_commit(),
            date: current_date(),
            from,
        }
    }
}

/// Stable: a later version is newer, and from a latest build the release
/// is offered whatever it is. Latest build: another commit that is not
/// older than this one is newer.
pub fn decide(channel: Channel, release: &Release, own: Own) -> Offer {
    let same_commit = |b: &BuildInfo| {
        let n = b.commit.len().min(own.commit.len());
        n >= 7 && b.commit[..n].eq_ignore_ascii_case(&own.commit[..n])
    };
    match channel {
        Channel::Stable => {
            if is_newer(&release.tag, own.version) {
                Offer::Newer
            } else if own.from == Channel::Nightly
                && !release.build.as_ref().is_some_and(same_commit)
            {
                Offer::BackToStable
            } else {
                Offer::UpToDate(format!(
                    "You have the newest version (the latest release is {}).",
                    release.tag
                ))
            }
        }
        Channel::Nightly => match &release.build {
            None => Offer::UpToDate(
                "The latest build does not say which commit it is, so it cannot be compared."
                    .to_owned(),
            ),
            Some(b) if same_commit(b) => Offer::UpToDate(format!(
                "You have the latest build ({}).",
                describe_build(&b.version, &b.commit, &b.date)
            )),
            Some(b)
                if !b.date.is_empty() && own.date != "unknown" && b.date.as_str() < own.date =>
            {
                Offer::UpToDate(format!(
                    "The published build ({}) is older than this one.",
                    describe_build(&b.version, &b.commit, &b.date)
                ))
            }
            Some(_) => Offer::Newer,
        },
    }
}

/// The numbers of a version or tag: `v0.2.1`, `VI-0.2.1`, `0.2.1-rc1`
/// (what follows the numbers is not compared).
pub fn parse_version(text: &str) -> Option<Vec<u64>> {
    let start = text.find(|c: char| c.is_ascii_digit())?;
    let rest = &text[start..];
    let end = rest
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(rest.len());
    let parts: Vec<u64> = rest[..end]
        .split('.')
        .filter(|p| !p.is_empty())
        .map(|p| p.parse().ok())
        .collect::<Option<_>>()?;
    (!parts.is_empty()).then_some(parts)
}

/// Whether `tag` names a later version than `current`. Missing numbers
/// count as zero; a tag without numbers is never newer.
pub fn is_newer(tag: &str, current: &str) -> bool {
    let (Some(mut a), Some(mut b)) = (parse_version(tag), parse_version(current)) else {
        return false;
    };
    let len = a.len().max(b.len());
    a.resize(len, 0);
    b.resize(len, 0);
    a > b
}

/// Whether a check at start is due: asked for, and none in the last day.
pub fn check_due(check_on_start: bool, last_check: u64, now: u64) -> bool {
    check_on_start && now.saturating_sub(last_check) >= DAY
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Feed {
    Url(String),
    Folder(PathBuf),
}

impl Feed {
    /// The environment's, else the hidden setting's, else GitHub (the
    /// channel's release). A URL given this way is asked as it is for
    /// either channel; a folder holds `latest.json` and `nightly.json`.
    pub fn choose(env: Option<String>, setting: &str, channel: Channel) -> Feed {
        let text = env
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| setting.to_owned());
        let text = text.trim();
        if text.is_empty() {
            Feed::Url(match channel {
                Channel::Stable => format!("https://api.github.com/repos/{REPO}/releases/latest"),
                Channel::Nightly => {
                    format!("https://api.github.com/repos/{REPO}/releases/tags/{NIGHTLY_TAG}")
                }
            })
        } else if text.starts_with("http://") || text.starts_with("https://") {
            Feed::Url(text.to_owned())
        } else {
            Feed::Folder(PathBuf::from(
                text.strip_prefix("file:///")
                    .or(text.strip_prefix("file://"))
                    .unwrap_or(text),
            ))
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Feed::Url(url) => url.clone(),
            Feed::Folder(dir) => dir.display().to_string(),
        }
    }
}

/// A published release, as far as the launcher cares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub tag: String,
    pub notes: String,
    /// The program zip, if the release has one.
    pub asset: Option<Asset>,
    /// Where `SHA256SUMS` is, if published.
    pub sums_url: Option<String>,
    /// Where `build.json` is, if published.
    pub build_url: Option<String>,
    /// What it says (read by [`fetch_latest`]).
    pub build: Option<BuildInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
    /// `sha256:<hex>`, as GitHub's API reports it.
    pub digest: Option<String>,
}

/// Reads GitHub's release JSON.
pub fn parse_release(json: &str) -> Result<Release, String> {
    let v: serde_json::Value = serde_json::from_str(json)
        .map_err(|e| format!("The release information could not be read ({e})."))?;
    let tag = v["tag_name"]
        .as_str()
        .ok_or("The release information names no version (tag_name).")?
        .to_owned();
    let text = |v: &serde_json::Value| v.as_str().map(str::to_owned);
    let assets = v["assets"].as_array().cloned().unwrap_or_default();
    let asset = assets.iter().find_map(|a| {
        let name = text(&a["name"])?;
        name.to_ascii_lowercase()
            .ends_with(ASSET_SUFFIX)
            .then(|| Asset {
                url: text(&a["browser_download_url"]).unwrap_or_default(),
                size: a["size"].as_u64().unwrap_or(0),
                digest: text(&a["digest"]),
                name,
            })
    });
    let named = |name: &str| {
        assets
            .iter()
            .find(|a| a["name"].as_str() == Some(name))
            .and_then(|a| text(&a["browser_download_url"]))
    };
    Ok(Release {
        tag,
        notes: text(&v["body"]).unwrap_or_default(),
        asset,
        sums_url: named(SUMS_NAME),
        build_url: named(BUILD_NAME),
        build: None,
    })
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .user_agent(USER_AGENT)
        .timeout_connect(Some(Duration::from_secs(20)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        .build()
        .into()
}

/// What looking for a release came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found {
    Release(Box<Release>),
    /// The project has published nothing yet.
    Nothing,
}

/// Asks the feed for the channel's release, and that for its `build.json`.
pub fn fetch_latest(feed: &Feed, channel: Channel) -> Result<Found, String> {
    let json = match feed {
        Feed::Url(url) => {
            let answer = agent()
                .get(url)
                .header("Accept", "application/vnd.github+json")
                .call();
            match answer {
                Ok(mut response) => response
                    .body_mut()
                    .read_to_string()
                    .map_err(|e| format!("The answer from {url} could not be read ({e})."))?,
                Err(ureq::Error::StatusCode(404)) => return Ok(Found::Nothing),
                Err(ureq::Error::StatusCode(code @ (403 | 429))) => {
                    return Err(format!(
                        "GitHub is not answering update checks from this network right now (status {code}). Try again in an hour."
                    ));
                }
                Err(e) => {
                    return Err(format!(
                        "Could not reach {url} ({e}). Check the internet connection."
                    ));
                }
            }
        }
        Feed::Folder(dir) => {
            let file = dir.join(match channel {
                Channel::Stable => "latest.json",
                Channel::Nightly => "nightly.json",
            });
            match std::fs::read_to_string(&file) {
                Ok(text) => text,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Found::Nothing),
                Err(e) => return Err(format!("Could not read {}: {e}", file.display())),
            }
        }
    };
    let mut release = parse_release(&json)?;
    if let Some(url) = &release.build_url
        && let Ok((reader, _)) = open(feed, url)
    {
        let mut text = String::new();
        if reader.take(1 << 16).read_to_string(&mut text).is_ok() {
            release.build = parse_build(&text);
        }
    }
    Ok(Found::Release(Box::new(release)))
}

/// Opens something a release names: a URL, or (folder feeds) a file name
/// or path relative to the folder.
fn open(feed: &Feed, location: &str) -> Result<(Box<dyn Read>, Option<u64>), String> {
    if location.starts_with("http://") || location.starts_with("https://") {
        let response = agent()
            .get(location)
            .call()
            .map_err(|e| format!("Could not download {location} ({e})."))?;
        let length = response.body().content_length();
        return Ok((Box::new(response.into_body().into_reader()), length));
    }
    let Feed::Folder(dir) = feed else {
        return Err(format!(
            "The release names a download that is not a web address: {location}"
        ));
    };
    let path = dir.join(location);
    let file = std::fs::File::open(&path)
        .map_err(|e| format!("Could not open {}: {e}", path.display()))?;
    let length = file.metadata().ok().map(|m| m.len());
    Ok((Box::new(file), length))
}

/// How far a download is; the window reads it while a worker writes it.
#[derive(Default)]
pub struct Transfer {
    pub received: AtomicU64,
    pub total: AtomicU64,
    pub cancel: AtomicBool,
}

fn download(feed: &Feed, location: &str, dest: &Path, transfer: &Transfer) -> Result<(), String> {
    let (mut reader, length) = open(feed, location)?;
    transfer.total.store(length.unwrap_or(0), Ordering::Relaxed);
    transfer.received.store(0, Ordering::Relaxed);
    let failed = |e: std::io::Error| format!("Could not write {}: {e}", dest.display());
    let mut out = std::fs::File::create(dest).map_err(failed)?;
    let mut buffer = vec![0u8; 256 * 1024];
    loop {
        if transfer.cancel.load(Ordering::Relaxed) {
            drop(out);
            let _ = std::fs::remove_file(dest);
            return Err("The download was cancelled.".to_owned());
        }
        let n = reader
            .read(&mut buffer)
            .map_err(|e| format!("The download of {location} broke off ({e})."))?;
        if n == 0 {
            break;
        }
        out.write_all(&buffer[..n]).map_err(failed)?;
        transfer.received.fetch_add(n as u64, Ordering::Relaxed);
    }
    out.flush().map_err(failed)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
pub fn sha256_bytes(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(hex(&hasher.finalize()))
}

fn is_sha256(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The hash a `SHA256SUMS` file gives for `name` (`<hex>  <name>` lines, as
/// `sha256sum` writes them; `*name` for binary mode).
pub fn hash_from_sums(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (hash, file) = line.trim().split_once(char::is_whitespace)?;
        let file = file.trim().trim_start_matches('*');
        (file.eq_ignore_ascii_case(name) && is_sha256(hash)).then(|| hash.to_ascii_lowercase())
    })
}

/// The hash in GitHub's `digest` field (`sha256:<hex>`).
pub fn hash_from_digest(digest: &str) -> Option<String> {
    let hash = digest.trim().strip_prefix("sha256:")?;
    is_sha256(hash).then(|| hash.to_ascii_lowercase())
}

/// Checks a downloaded file against the published hash. `Err` says which
/// of the two it is: no hash to check against, or a file that differs.
pub fn verify(file: &Path, expected: Option<&str>) -> Result<(), String> {
    let Some(expected) = expected else {
        return Err(format!(
            "The release publishes no SHA-256 for {}, so it cannot be checked and was not installed.",
            file.file_name().unwrap_or_default().to_string_lossy()
        ));
    };
    let actual =
        sha256_file(file).map_err(|e| format!("Could not read {}: {e}", file.display()))?;
    if actual.eq_ignore_ascii_case(expected) {
        Ok(())
    } else {
        Err(format!(
            "The downloaded file {} is not the one that was published (SHA-256 {actual}, expected {expected}). It was not installed.",
            file.display()
        ))
    }
}

/// Unpacks a release zip into an empty `dest`. Only plain files with safe
/// relative names; one folder around everything is taken off.
pub fn unpack(zip_path: &Path, dest: &Path) -> Result<Vec<PathBuf>, String> {
    let bad = |e: &dyn std::fmt::Display| {
        format!(
            "The update {} could not be unpacked ({e}).",
            zip_path.display()
        )
    };
    let file = std::fs::File::open(zip_path).map_err(|e| bad(&e))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| bad(&e))?;
    let mut names = Vec::new();
    for i in 0..archive.len() {
        let entry = archive.by_index(i).map_err(|e| bad(&e))?;
        if entry.is_dir() {
            continue;
        }
        let name = entry
            .enclosed_name()
            .ok_or_else(|| bad(&format!("unsafe file name {:?}", entry.name())))?;
        names.push((i, name));
    }
    // `VI-0.2.0-windows-x64/victory.exe` and the like: without the folder.
    let first = names
        .first()
        .and_then(|(_, n)| n.components().next().map(|c| c.as_os_str().to_owned()));
    let wrapped = first.as_ref().is_some_and(|first| {
        names.iter().all(|(_, n)| {
            n.components().count() > 1
                && n.components().next().map(|c| c.as_os_str()) == Some(first)
        })
    });
    let _ = std::fs::remove_dir_all(dest);
    std::fs::create_dir_all(dest)
        .map_err(|e| format!("Could not create {}: {e}", dest.display()))?;
    let mut written = Vec::new();
    for (i, name) in names {
        let relative: PathBuf = if wrapped {
            name.components().skip(1).collect()
        } else {
            name
        };
        let target = dest.join(&relative);
        if let Some(dir) = target.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("Could not create {}: {e}", dir.display()))?;
        }
        let mut entry = archive.by_index(i).map_err(|e| bad(&e))?;
        let mut out = std::fs::File::create(&target)
            .map_err(|e| format!("Could not write {}: {e}", target.display()))?;
        std::io::copy(&mut entry, &mut out).map_err(|e| bad(&e))?;
        written.push(relative);
    }
    // The download names the launcher `VI-Setup.exe`; installed, it is
    // `victory-launcher.exe`.
    if let Some(setup) = written
        .iter_mut()
        .find(|p| p.as_path() == Path::new(SETUP_EXE))
    {
        std::fs::rename(dest.join(SETUP_EXE), dest.join(paths::LAUNCHER_EXE))
            .map_err(|e| bad(&e))?;
        *setup = PathBuf::from(paths::LAUNCHER_EXE);
    }
    if !written.iter().any(|p| p == Path::new(paths::LAUNCHER_EXE)) {
        let _ = std::fs::remove_dir_all(dest);
        return Err(bad(&format!("it does not contain {}", paths::LAUNCHER_EXE)));
    }
    Ok(written)
}

fn staged_dir(updates: &Path) -> PathBuf {
    updates.join("staged")
}

fn pending_file(updates: &Path) -> PathBuf {
    updates.join("pending.txt")
}

/// The version waiting to be swapped in, if any, and the channel it is of.
pub fn pending(updates: &Path) -> Option<(String, Channel)> {
    let text = std::fs::read_to_string(pending_file(updates)).ok()?;
    let mut lines = text.lines();
    let tag = lines.next()?.trim().to_owned();
    let channel = Channel::from_word(lines.next().unwrap_or_default());
    (!tag.is_empty() && staged_dir(updates).join(paths::LAUNCHER_EXE).is_file())
        .then_some((tag, channel))
}

/// What a release is called in the window: its tag, or for the rolling
/// one the build it holds.
pub fn release_name(release: &Release) -> String {
    match &release.build {
        Some(b) if release.tag == NIGHTLY_TAG => format!(
            "latest build {} of {}",
            &b.commit[..b.commit.len().min(7)],
            b.date
        ),
        _ => release.tag.clone(),
    }
}

/// Steps 2 to 4: download, verify, unpack, mark as pending.
pub fn download_and_stage(
    feed: &Feed,
    release: &Release,
    channel: Channel,
    updates: &Path,
    transfer: &Transfer,
) -> Result<(), String> {
    let asset = release.asset.as_ref().ok_or_else(|| {
        format!(
            "Release {} has no file ending in {ASSET_SUFFIX} to download.",
            release.tag
        )
    })?;
    std::fs::create_dir_all(updates)
        .map_err(|e| format!("Could not create {}: {e}", updates.display()))?;
    let _ = std::fs::remove_file(pending_file(updates));
    // (The name is the release's: keep only its last part.)
    let file_name = Path::new(&asset.name)
        .file_name()
        .ok_or("The release's file has no name.")?;
    let zip_path = updates.join(file_name);
    // The published hash first: nothing is downloaded that cannot be checked.
    let expected = match &release.sums_url {
        Some(url) => {
            let (mut reader, _) = open(feed, url)?;
            let mut sums = String::new();
            reader
                .by_ref()
                .take(1 << 20)
                .read_to_string(&mut sums)
                .map_err(|e| format!("Could not read {SUMS_NAME} ({e})."))?;
            hash_from_sums(&sums, &asset.name)
        }
        None => None,
    }
    .or_else(|| asset.digest.as_deref().and_then(hash_from_digest));
    if expected.is_none() {
        return verify(&zip_path, None);
    }
    download(feed, &asset.url, &zip_path, transfer)?;
    if let Err(e) = verify(&zip_path, expected.as_deref()) {
        let _ = std::fs::remove_file(&zip_path);
        return Err(e);
    }
    unpack(&zip_path, &staged_dir(updates))?;
    let _ = std::fs::remove_file(&zip_path);
    std::fs::write(
        pending_file(updates),
        format!("{}\n{}\n", release_name(release), channel.word()),
    )
    .map_err(|e| format!("Could not write {}: {e}", pending_file(updates).display()))
}

/// Step 5: puts the staged files in the place of the program files.
/// Returns the version swapped in and the files written (relative), or
/// `None` if nothing was pending. Files in use are renamed to `*.old`.
pub fn apply_pending(
    updates: &Path,
    program_dir: &Path,
) -> Result<Option<(String, Channel, Vec<PathBuf>)>, String> {
    let Some((tag, channel)) = pending(updates) else {
        return Ok(None);
    };
    let staged = staged_dir(updates);
    let mut files = Vec::new();
    let mut stack = vec![staged.clone()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)
            .map_err(|e| format!("Could not read {}: {e}", dir.display()))?
            .flatten()
        {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(relative) = path.strip_prefix(&staged) {
                files.push(relative.to_path_buf());
            }
        }
    }
    // The launcher last: if anything before it fails, the old one still starts.
    files.sort_by_key(|f| f == Path::new(paths::LAUNCHER_EXE));
    for relative in &files {
        let (from, to) = (staged.join(relative), program_dir.join(relative));
        if let Some(dir) = to.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("Could not create {}: {e}", dir.display()))?;
        }
        if to.exists() {
            let mut old = to.clone().into_os_string();
            old.push(OLD_SUFFIX);
            let old = PathBuf::from(old);
            let _ = std::fs::remove_file(&old);
            std::fs::rename(&to, &old)
                .map_err(|e| format!("Could not move the old {} out of the way: {e}. Close the game and start the launcher again.", to.display()))?;
        }
        std::fs::copy(&from, &to).map_err(|e| format!("Could not write {}: {e}", to.display()))?;
    }
    let _ = std::fs::remove_file(pending_file(updates));
    let _ = std::fs::remove_dir_all(&staged);
    Ok(Some((tag, channel, files)))
}

/// Deletes the `*.old` files an earlier swap left in the program folder
/// (those still running stay for the next time).
pub fn remove_old_files(program_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(program_dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().ends_with(OLD_SUFFIX) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn versions_compare_by_number() {
        assert_eq!(parse_version("v0.2.1"), Some(vec![0, 2, 1]));
        assert_eq!(parse_version("VI-1.10.0-rc2"), Some(vec![1, 10, 0]));
        assert_eq!(parse_version("nightly"), None);
        assert!(is_newer("v0.1.1", "0.1.0"));
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(is_newer("v1", "0.99.99"));
        assert!(is_newer("v0.1.0.1", "0.1.0"));
        assert!(!is_newer("v0.1.0", "0.1.0"));
        assert!(!is_newer("v0.1", "0.1.0"));
        assert!(!is_newer("v0.0.9", "0.1.0"));
        assert!(!is_newer("latest", "0.1.0"));
    }

    #[test]
    fn a_check_is_due_once_a_day_and_only_if_wanted() {
        let now = 1_800_000_000;
        assert!(check_due(true, 0, now));
        assert!(check_due(true, now - DAY, now));
        assert!(!check_due(true, now - DAY + 1, now));
        assert!(!check_due(false, 0, now));
        // A clock set back does not make it check on every start.
        assert!(!check_due(true, now + 5, now));
    }

    #[test]
    fn feeds() {
        let s = Channel::Stable;
        assert_eq!(
            Feed::choose(None, "", s),
            Feed::Url(
                "https://api.github.com/repos/notquiteog/victory-nvidia/releases/latest".into()
            )
        );
        assert_eq!(
            Feed::choose(None, "", Channel::Nightly),
            Feed::Url(
                "https://api.github.com/repos/notquiteog/victory-nvidia/releases/tags/nightly"
                    .into()
            )
        );
        assert_eq!(
            Feed::choose(Some("http://127.0.0.1:8123/latest".into()), "ignored", s),
            Feed::Url("http://127.0.0.1:8123/latest".into())
        );
        assert_eq!(
            Feed::choose(Some(String::new()), r"D:\feed", s),
            Feed::Folder(PathBuf::from(r"D:\feed"))
        );
        assert_eq!(
            Feed::choose(Some("file:///D:/feed".into()), "", s),
            Feed::Folder(PathBuf::from("D:/feed"))
        );
    }

    const GITHUB_JSON: &str = r#"{
        "tag_name": "v0.2.0", "name": "VI 0.2.0", "body": "Faster streets.\n- one\n- two",
        "assets": [
          {"name": "SHA256SUMS", "size": 99, "browser_download_url": "https://github.com/o/r/releases/download/v0.2.0/SHA256SUMS"},
          {"name": "VI-0.2.0-windows-x64.zip", "size": 1234, "digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
           "browser_download_url": "https://github.com/o/r/releases/download/v0.2.0/VI-0.2.0-windows-x64.zip"},
          {"name": "source.tar.gz", "browser_download_url": "https://example.invalid/x"}
        ]}"#;

    #[test]
    fn github_release_json_is_understood() {
        let r = parse_release(GITHUB_JSON).unwrap();
        assert_eq!(r.tag, "v0.2.0");
        assert!(r.notes.starts_with("Faster streets."));
        let asset = r.asset.unwrap();
        assert_eq!(asset.name, "VI-0.2.0-windows-x64.zip");
        assert_eq!(asset.size, 1234);
        assert!(asset.url.ends_with("/VI-0.2.0-windows-x64.zip"));
        assert_eq!(
            hash_from_digest(asset.digest.as_deref().unwrap()),
            Some("a".repeat(64))
        );
        assert!(r.sums_url.unwrap().ends_with("/SHA256SUMS"));
        assert!(parse_release("{}").is_err());
        assert!(parse_release("<html>").is_err());
        let bare = parse_release(r#"{"tag_name":"v9"}"#).unwrap();
        assert!(bare.asset.is_none() && bare.sums_url.is_none());
    }

    fn release(tag: &str, commit: Option<(&str, &str)>) -> Release {
        Release {
            tag: tag.to_owned(),
            notes: String::new(),
            asset: None,
            sums_url: None,
            build_url: None,
            build: commit.map(|(commit, date)| BuildInfo {
                version: "0.1.0".into(),
                commit: commit.into(),
                date: date.into(),
            }),
        }
    }

    #[test]
    fn build_json_is_read() {
        let b = parse_build(r#"{ "version": "0.1.0", "commit": "0123456789abcdef0123456789abcdef01234567", "date": "2026-11-26" }"#).unwrap();
        assert_eq!(
            (b.version.as_str(), b.date.as_str()),
            ("0.1.0", "2026-11-26")
        );
        assert_eq!(
            describe_build(&b.version, &b.commit, &b.date),
            "VI 0.1.0 (0123456, 2026-11-26)"
        );
        assert_eq!(parse_build(r#"{"commit":"unknown"}"#), None);
        assert_eq!(parse_build("not json"), None);
        // (PowerShell likes to put a byte-order mark first.)
        assert!(parse_build("\u{feff}{\"commit\":\"abcdef0\"}").is_some());
    }

    #[test]
    fn stable_offers_later_versions_only() {
        let own = Own {
            version: "0.1.0",
            commit: "aaaaaaa1",
            date: "2026-11-20",
            from: Channel::Stable,
        };
        assert_eq!(
            decide(Channel::Stable, &release("v0.2.0", None), own),
            Offer::Newer
        );
        assert!(matches!(
            decide(Channel::Stable, &release("v0.1.0", None), own),
            Offer::UpToDate(_)
        ));
        assert!(matches!(
            decide(
                Channel::Stable,
                &release("v0.0.9", Some(("bbbbbbb2", "2026-11-01"))),
                own
            ),
            Offer::UpToDate(_)
        ));
    }

    #[test]
    fn latest_build_compares_commits() {
        let own = Own {
            version: "0.1.0",
            commit: "aaaaaaa1aaaaaaa1",
            date: "2026-11-20",
            from: Channel::Stable,
        };
        let nightly = |commit, date| release("nightly", Some((commit, date)));
        // Another commit, the same day or later: newer. The same version number does not matter.
        assert_eq!(
            decide(Channel::Nightly, &nightly("bbbbbbb2", "2026-11-21"), own),
            Offer::Newer
        );
        assert_eq!(
            decide(Channel::Nightly, &nightly("bbbbbbb2", "2026-11-20"), own),
            Offer::Newer
        );
        // The same commit (given short or long): nothing to do.
        assert!(matches!(
            decide(Channel::Nightly, &nightly("aaaaaaa1", "2026-11-20"), own),
            Offer::UpToDate(_)
        ));
        assert!(matches!(
            decide(
                Channel::Nightly,
                &nightly("AAAAAAA1aaaaaaa1ffff", "2026-11-20"),
                own
            ),
            Offer::UpToDate(_)
        ));
        // An older published build is not a downgrade to offer.
        assert!(matches!(
            decide(Channel::Nightly, &nightly("bbbbbbb2", "2026-11-01"), own),
            Offer::UpToDate(_)
        ));
        // Without build.json there is nothing to compare.
        assert!(matches!(
            decide(Channel::Nightly, &release("nightly", None), own),
            Offer::UpToDate(_)
        ));
        // A build that does not know its own commit takes what is published.
        let unknown = Own {
            commit: "unknown",
            date: "unknown",
            ..own
        };
        assert_eq!(
            decide(
                Channel::Nightly,
                &nightly("bbbbbbb2", "2026-11-01"),
                unknown
            ),
            Offer::Newer
        );
    }

    #[test]
    fn switching_channels_works_both_ways() {
        // On a latest build, Stable offers the release even though its version is not later...
        let own = Own {
            version: "0.1.0",
            commit: "ccccccc3",
            date: "2026-11-26",
            from: Channel::Nightly,
        };
        assert_eq!(
            decide(
                Channel::Stable,
                &release("v0.1.0", Some(("aaaaaaa1", "2026-11-20"))),
                own
            ),
            Offer::BackToStable
        );
        assert_eq!(
            decide(Channel::Stable, &release("v0.1.0", None), own),
            Offer::BackToStable
        );
        // ...unless it is the very same build, and a later version is simply newer.
        assert!(matches!(
            decide(
                Channel::Stable,
                &release("v0.1.0", Some(("ccccccc3", "2026-11-26"))),
                own
            ),
            Offer::UpToDate(_)
        ));
        assert_eq!(
            decide(Channel::Stable, &release("v0.2.0", None), own),
            Offer::Newer
        );
        // And from a stable install, Latest build offers the newer commit.
        let own = Own {
            from: Channel::Stable,
            ..own
        };
        assert_eq!(
            decide(
                Channel::Nightly,
                &release("nightly", Some(("ddddddd4", "2026-11-27"))),
                own
            ),
            Offer::Newer
        );
    }

    #[test]
    fn sha256_of_known_data() {
        assert_eq!(
            sha256_bytes(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f");
        std::fs::write(&file, b"abc").unwrap();
        assert_eq!(sha256_file(&file).unwrap(), sha256_bytes(b"abc"));
    }

    #[test]
    fn sums_files_are_read_by_name() {
        let abc = sha256_bytes(b"abc");
        let sums = format!(
            "{}  other.zip\n{}  *VI-0.2.0-windows-x64.zip\nnot a line\n",
            "0".repeat(64),
            abc.to_uppercase()
        );
        assert_eq!(hash_from_sums(&sums, "VI-0.2.0-windows-x64.zip"), Some(abc));
        assert_eq!(hash_from_sums(&sums, "missing.zip"), None);
        assert_eq!(hash_from_sums("abc  VI.zip", "VI.zip"), None);
        assert_eq!(hash_from_digest("sha512:00"), None);
    }

    #[test]
    fn verification_refuses_a_mismatch_and_a_missing_hash() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("VI-0.2.0-windows-x64.zip");
        std::fs::write(&file, b"abc").unwrap();
        assert_eq!(verify(&file, Some(&sha256_bytes(b"abc"))), Ok(()));
        assert_eq!(
            verify(&file, Some(&sha256_bytes(b"abc").to_uppercase())),
            Ok(())
        );
        let wrong = verify(&file, Some(&sha256_bytes(b"abd"))).unwrap_err();
        assert!(wrong.contains("not the one that was published"), "{wrong}");
        let none = verify(&file, None).unwrap_err();
        assert!(none.contains("publishes no SHA-256"), "{none}");
    }

    pub(crate) fn make_zip(path: &Path, files: &[(&str, &[u8])]) {
        let mut zip = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        for (name, data) in files {
            zip.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap();
    }

    /// A folder feed with one release in it; returns the release's tag.
    pub(crate) fn make_feed(dir: &Path, tag: &str, files: &[(&str, &[u8])], sums: Option<&str>) {
        let name = format!("VI-{}-windows-x64.zip", tag.trim_start_matches('v'));
        make_zip(&dir.join(&name), files);
        let hash = sha256_file(&dir.join(&name)).unwrap();
        let sums = sums.map_or_else(|| format!("{hash}  {name}\n"), str::to_owned);
        std::fs::write(dir.join("SHA256SUMS"), sums).unwrap();
        let json = serde_json::json!({
            "tag_name": tag, "body": "What is new.",
            "assets": [
                {"name": name, "browser_download_url": name, "size": 1},
                {"name": "SHA256SUMS", "browser_download_url": "SHA256SUMS"}
            ]
        });
        std::fs::write(dir.join("latest.json"), json.to_string()).unwrap();
    }

    #[test]
    fn the_whole_flow_from_a_folder_feed() {
        let dir = tempfile::tempdir().unwrap();
        let (feed_dir, updates, program) = (
            dir.path().join("feed"),
            dir.path().join("updates"),
            dir.path().join("program"),
        );
        for d in [&feed_dir, &program] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(program.join("victory-launcher.exe"), b"old launcher").unwrap();
        std::fs::write(program.join("victory.exe"), b"old victory").unwrap();
        std::fs::write(program.join("VI.ico"), b"icon").unwrap();
        make_feed(
            &feed_dir,
            "v0.2.0",
            &[
                ("victory-launcher.exe", b"new launcher"),
                ("victory.exe", b"new victory"),
                ("LICENSE", b"text"),
            ],
            None,
        );
        let feed = Feed::Folder(feed_dir);
        assert_eq!(
            fetch_latest(&Feed::Folder(dir.path().join("empty")), Channel::Stable),
            Ok(Found::Nothing)
        );
        let Found::Release(release) = fetch_latest(&feed, Channel::Stable).unwrap() else {
            panic!()
        };
        assert!(is_newer(&release.tag, "0.1.0"));
        assert_eq!(pending(&updates), None);
        let transfer = Transfer::default();
        download_and_stage(&feed, &release, Channel::Stable, &updates, &transfer).unwrap();
        assert!(transfer.received.load(Ordering::Relaxed) > 0);
        assert_eq!(
            pending(&updates),
            Some(("v0.2.0".to_owned(), Channel::Stable))
        );
        // Nothing in the program folder changed yet.
        assert_eq!(
            std::fs::read(program.join("victory.exe")).unwrap(),
            b"old victory"
        );
        let (tag, _, files) = apply_pending(&updates, &program).unwrap().unwrap();
        assert_eq!(tag, "v0.2.0");
        assert_eq!(files.len(), 3);
        assert_eq!(files.last().unwrap(), Path::new("victory-launcher.exe"));
        assert_eq!(
            std::fs::read(program.join("victory-launcher.exe")).unwrap(),
            b"new launcher"
        );
        assert_eq!(
            std::fs::read(program.join("victory.exe")).unwrap(),
            b"new victory"
        );
        assert_eq!(
            std::fs::read(program.join("victory.exe.old")).unwrap(),
            b"old victory"
        );
        assert_eq!(std::fs::read(program.join("VI.ico")).unwrap(), b"icon");
        assert_eq!(pending(&updates), None);
        assert_eq!(apply_pending(&updates, &program), Ok(None));
        remove_old_files(&program);
        assert!(
            !program.join("victory.exe.old").exists()
                && !program.join("victory-launcher.exe.old").exists()
        );
        assert!(program.join("LICENSE").is_file());
    }

    #[test]
    fn a_tampered_download_is_refused_and_nothing_is_staged() {
        let dir = tempfile::tempdir().unwrap();
        let (feed_dir, updates) = (dir.path().join("feed"), dir.path().join("updates"));
        std::fs::create_dir_all(&feed_dir).unwrap();
        let wrong = format!("{}  VI-0.2.0-windows-x64.zip\n", "1".repeat(64));
        make_feed(
            &feed_dir,
            "v0.2.0",
            &[("victory-launcher.exe", b"evil")],
            Some(&wrong),
        );
        let feed = Feed::Folder(feed_dir.clone());
        let Found::Release(release) = fetch_latest(&feed, Channel::Stable).unwrap() else {
            panic!()
        };
        let e = download_and_stage(
            &feed,
            &release,
            Channel::Stable,
            &updates,
            &Transfer::default(),
        )
        .unwrap_err();
        assert!(e.contains("not the one that was published"), "{e}");
        assert_eq!(pending(&updates), None);
        assert!(!updates.join("staged").exists());
        assert!(!updates.join("VI-0.2.0-windows-x64.zip").exists());
        // And with no hash published at all: not even downloaded.
        make_feed(
            &feed_dir,
            "v0.2.0",
            &[("victory-launcher.exe", b"evil")],
            Some("nothing useful\n"),
        );
        let e = download_and_stage(
            &feed,
            &release,
            Channel::Stable,
            &updates,
            &Transfer::default(),
        )
        .unwrap_err();
        assert!(e.contains("publishes no SHA-256"), "{e}");
        assert!(!updates.join("VI-0.2.0-windows-x64.zip").exists());
    }

    /// The rolling pre-release, as a folder feed: `nightly.json`, the zip
    /// with the launcher named `VI-Setup.exe`, `build.json`.
    #[test]
    fn the_latest_build_channel_from_a_folder_feed() {
        let dir = tempfile::tempdir().unwrap();
        let (feed_dir, updates, program) = (
            dir.path().join("feed"),
            dir.path().join("updates"),
            dir.path().join("program"),
        );
        for d in [&feed_dir, &program] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(program.join("victory-launcher.exe"), b"old launcher").unwrap();
        let name = "VI-0.1.0-windows-x64.zip";
        make_zip(
            &feed_dir.join(name),
            &[
                ("VI-Setup.exe", b"nightly launcher"),
                ("victory.exe", b"nightly victory"),
                ("README.txt", b"r"),
            ],
        );
        let hash = sha256_file(&feed_dir.join(name)).unwrap();
        std::fs::write(feed_dir.join("SHA256SUMS"), format!("{hash}  {name}\n")).unwrap();
        std::fs::write(
            feed_dir.join("build.json"),
            r#"{"version":"0.1.0","commit":"feedfacefeedface","date":"2099-01-01"}"#,
        )
        .unwrap();
        let json = serde_json::json!({
            "tag_name": "nightly", "prerelease": true, "body": "Built from feedface.",
            "assets": [
                {"name": name, "browser_download_url": name, "size": 1},
                {"name": "SHA256SUMS", "browser_download_url": "SHA256SUMS"},
                {"name": "build.json", "browser_download_url": "build.json"}
            ]
        });
        std::fs::write(feed_dir.join("nightly.json"), json.to_string()).unwrap();
        let feed = Feed::Folder(feed_dir);
        // Nothing is published on the stable channel of this feed.
        assert_eq!(fetch_latest(&feed, Channel::Stable), Ok(Found::Nothing));
        let Found::Release(release) = fetch_latest(&feed, Channel::Nightly).unwrap() else {
            panic!()
        };
        assert_eq!(release.build.as_ref().unwrap().commit, "feedfacefeedface");
        assert_eq!(
            decide(Channel::Nightly, &release, Own::current(Channel::Stable)),
            Offer::Newer
        );
        assert_eq!(release_name(&release), "latest build feedfac of 2099-01-01");
        download_and_stage(
            &feed,
            &release,
            Channel::Nightly,
            &updates,
            &Transfer::default(),
        )
        .unwrap();
        assert_eq!(
            pending(&updates),
            Some((
                "latest build feedfac of 2099-01-01".to_owned(),
                Channel::Nightly
            ))
        );
        let (_, channel, mut files) = apply_pending(&updates, &program).unwrap().unwrap();
        files.sort();
        assert_eq!(channel, Channel::Nightly);
        assert_eq!(
            files,
            [
                PathBuf::from("README.txt"),
                PathBuf::from("victory-launcher.exe"),
                PathBuf::from("victory.exe")
            ]
        );
        // The setup program of the download is the installed launcher.
        assert_eq!(
            std::fs::read(program.join("victory-launcher.exe")).unwrap(),
            b"nightly launcher"
        );
        assert!(!program.join("VI-Setup.exe").exists());
    }

    #[test]
    fn unpacking_takes_a_wrapping_folder_off_and_wants_the_launcher() {
        let dir = tempfile::tempdir().unwrap();
        let zip = dir.path().join("a.zip");
        make_zip(
            &zip,
            &[
                ("VI-0.2.0/victory-launcher.exe", b"l"),
                ("VI-0.2.0/victory.exe", b"v"),
                ("VI-0.2.0/docs/README.txt", b"r"),
            ],
        );
        let mut files = unpack(&zip, &dir.path().join("out")).unwrap();
        files.sort();
        assert_eq!(
            files,
            [
                PathBuf::from("docs").join("README.txt"),
                PathBuf::from("victory-launcher.exe"),
                PathBuf::from("victory.exe")
            ]
        );
        make_zip(&zip, &[("victory.exe", b"v")]);
        let e = unpack(&zip, &dir.path().join("out2")).unwrap_err();
        assert!(e.contains("does not contain victory-launcher.exe"), "{e}");
        std::fs::write(&zip, b"this is not a zip").unwrap();
        assert!(unpack(&zip, &dir.path().join("out3")).is_err());
    }

    /// A one-request-at-a-time HTTP server on a local port, as the fake of
    /// GitHub: `/latest` and the two assets.
    #[test]
    fn the_whole_flow_over_http() {
        use std::io::{BufRead, BufReader};
        use std::net::TcpListener;
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("VI-0.3.0-windows-x64.zip");
        make_zip(
            &zip_path,
            &[
                ("victory-launcher.exe", b"newer launcher"),
                ("victory.exe", b"newer victory"),
            ],
        );
        let zip_bytes = std::fs::read(&zip_path).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let json = serde_json::json!({
            "tag_name": "v0.3.0", "body": "Notes.",
            "assets": [{"name": "VI-0.3.0-windows-x64.zip", "size": zip_bytes.len(),
                        "digest": format!("sha256:{}", sha256_bytes(&zip_bytes)),
                        "browser_download_url": format!("{base}/download/VI-0.3.0-windows-x64.zip")}]
        })
        .to_string();
        let agents = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = agents.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request = String::new();
                reader.read_line(&mut request).unwrap();
                loop {
                    let mut header = String::new();
                    if reader.read_line(&mut header).unwrap() == 0 || header.trim().is_empty() {
                        break;
                    }
                    seen.lock().unwrap().push(header.trim().to_owned());
                }
                let path = request.split(' ').nth(1).unwrap_or_default();
                let (status, body): (&str, Vec<u8>) = match path {
                    "/latest" => ("200 OK", json.clone().into_bytes()),
                    "/download/VI-0.3.0-windows-x64.zip" => ("200 OK", zip_bytes.clone()),
                    _ => ("404 Not Found", Vec::new()),
                };
                let head = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&body);
            }
        });
        assert_eq!(
            fetch_latest(&Feed::Url(format!("{base}/nothing-here")), Channel::Stable),
            Ok(Found::Nothing)
        );
        let feed = Feed::Url(format!("{base}/latest"));
        let Found::Release(release) = fetch_latest(&feed, Channel::Stable).unwrap() else {
            panic!()
        };
        assert_eq!(release.tag, "v0.3.0");
        let updates = dir.path().join("updates");
        let transfer = Transfer::default();
        download_and_stage(&feed, &release, Channel::Stable, &updates, &transfer).unwrap();
        assert_eq!(
            transfer.total.load(Ordering::Relaxed),
            transfer.received.load(Ordering::Relaxed)
        );
        assert_eq!(pending(&updates).map(|p| p.0).as_deref(), Some("v0.3.0"));
        assert_eq!(
            std::fs::read(updates.join("staged").join("victory.exe")).unwrap(),
            b"newer victory"
        );
        // What was sent: a generic client name, no credentials, no cookies.
        let headers = agents.lock().unwrap().join("\n").to_ascii_lowercase();
        assert!(
            headers.contains("user-agent: victory-launcher"),
            "{headers}"
        );
        assert!(
            !headers.contains("authorization") && !headers.contains("cookie"),
            "{headers}"
        );
    }
}
