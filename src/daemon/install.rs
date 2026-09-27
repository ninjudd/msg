//! Installing the daemon: a copied bundle, a launchd agent, and the one step
//! that cannot be automated.
//!
//! There is no API for granting Full Disk Access — `tccutil` only removes — so
//! the install ends by telling the user what to switch on, and the daemon's
//! first failed read is what puts it in the list to be switched
//! (docs/projects/daemon-and-permissions/readme.md §9).

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::daemon::protocol::{LABEL, socket_path, state_directory};
use crate::{Error, Result};

/// The installed bundle.
///
/// A bundle rather than a bare executable, because TCC keys a grant by bundle
/// identifier when it can resolve one and by executable path when it cannot —
/// and a path-keyed grant cannot be switched off. System Settings only ever
/// creates and deletes those rows; the toggle authenticates and then does
/// nothing (§13). It is not in /Applications because nobody launches it.
pub fn bundle_path() -> PathBuf {
    crate::home().join(".local/libexec/msgd.app")
}

/// What launchd runs. TCC resolves it back to the bundle above.
pub fn binary_path() -> PathBuf {
    bundle_path().join("Contents/MacOS/msgd")
}

/// Where the daemon lived before it was bundled, removed on install.
fn legacy_binary_path() -> PathBuf {
    crate::home().join(".local/libexec/msgd")
}

pub fn plist_path() -> PathBuf {
    crate::home().join(format!("Library/LaunchAgents/{LABEL}.plist"))
}

pub fn log_path() -> PathBuf {
    state_directory().join("msgd.log")
}

/// Where the build leaves the signed bundle: beside the `msg` binary itself.
///
/// The TypeScript build resolved this from the source file's own URL, which only
/// ever made sense inside a checkout. A shipped `msg` has no checkout, so the
/// bundle travels next to it and `--from` names it explicitly otherwise.
///
/// The symlink is resolved first, and that is not a detail: the README tells
/// people to install by symlinking `build/msg` onto their PATH, and
/// `current_exe` on macOS reports the path used to launch rather than its
/// target. Without this, `msg daemon install` looked next to the *symlink* —
/// `~/.local/bin/msgd.app` — and reported a bundle that was never going to be
/// there.
pub fn built_bundle() -> PathBuf {
    let Ok(exe) = std::env::current_exe() else {
        return PathBuf::from("msgd.app");
    };
    let resolved = std::fs::canonicalize(&exe).unwrap_or(exe);
    resolved
        .parent()
        .map_or_else(|| PathBuf::from("msgd.app"), |dir| dir.join("msgd.app"))
}

fn domain() -> String {
    format!("gui/{}", unsafe { libc::getuid() })
}

/// Settings the daemon reads for itself.
///
/// A launchd job inherits nothing from the shell that installed it, so anything
/// set here has to be written into the plist or it silently does not apply. The
/// failure is confusing rather than loud: installing with `MSG_SOCKET` set gives
/// a CLI looking at one path and a daemon listening on another.
const DAEMON_ENVIRONMENT: &[&str] = &[
    "MSG_SOCKET",
    "MSG_STATE_DIR",
    "MSG_CONFIG",
    "MSG_CONTACTS_SOURCE",
];

/// `MSG_DB` is deliberately absent.
///
/// It is documented as `--db` by another name, and the CLI answers it locally
/// rather than asking the daemon, so carrying it here could never help the
/// documented path — it could only outlive the shell that set it. Installing
/// while pointed at a fixture and later unsetting the variable would leave a
/// daemon still pinned to that fixture, answering a CLI that has no idea, which
/// is the worst shape a wrong answer can take. Run `msgd` directly with `MSG_DB`
/// set to serve a fixture.
pub fn daemon_environment(read: impl Fn(&str) -> Option<String>) -> BTreeMap<String, String> {
    let mut carried = BTreeMap::new();
    for name in DAEMON_ENVIRONMENT {
        if let Some(value) = read(name).filter(|value| !value.is_empty()) {
            carried.insert((*name).to_string(), value);
        }
    }
    carried
}

pub fn daemon_environment_from_process() -> BTreeMap<String, String> {
    daemon_environment(|name| std::env::var(name).ok())
}

fn xml_text(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The agent is user-owned, which is safe only because the daemon is a single
/// executable application: pointing this plist somewhere else runs a binary that
/// holds no grant (§4).
pub fn plist(binary: &str, log: &str, environment: &BTreeMap<String, String>) -> String {
    let block = if environment.is_empty() {
        String::new()
    } else {
        let entries: Vec<String> = environment
            .iter()
            .map(|(name, value)| {
                format!("    <key>{name}</key><string>{}</string>", xml_text(value))
            })
            .collect();
        format!(
            "  <key>EnvironmentVariables</key>\n  <dict>\n{}\n  </dict>\n",
            entries.join("\n")
        )
    };

    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{}</string>
  </array>
{block}  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>ProcessType</key><string>Background</string>
  <key>StandardErrorPath</key><string>{}</string>
</dict>
</plist>
"#,
        xml_text(binary),
        xml_text(log)
    )
}

fn launchctl(args: &[&str]) -> (bool, String) {
    match Command::new("launchctl").args(args).output() {
        Ok(output) => (
            output.status.success(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ),
        Err(error) => (false, error.to_string()),
    }
}

pub fn is_loaded() -> bool {
    launchctl(&["print", &format!("{}/{LABEL}", domain())]).0
}

pub struct Installed {
    pub bundle: PathBuf,
    pub binary: PathBuf,
    pub plist: PathBuf,
    pub socket: PathBuf,
    pub log: PathBuf,
    /// Whether an unbundled daemon from an older install was removed.
    pub replaced_legacy: bool,
    /// What was carried into the job from the installing shell's environment.
    pub environment: BTreeMap<String, String>,
}

pub fn install(source: &Path) -> Result<Installed> {
    if !source.join("Contents/MacOS/msgd").exists() {
        return Err(Error::other(format!(
            "no daemon bundle at {}\nBuild it first with `./scripts/build.sh`.",
            source.display()
        )));
    }

    let bundle = bundle_path();
    let binary = binary_path();
    if let Some(parent) = bundle.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // launchd holds the running binary open, so replacing it in place fails. A
    // leftover _CodeSignature would also make the new bundle fail to validate.
    std::fs::remove_dir_all(&bundle).ok();
    copy_tree(source, &bundle)?;
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755))?;

    // An install from before the bundle left an executable where the bundle now
    // goes beside it. It holds its own grants and would keep running if anything
    // still pointed at it, so it does not get to linger.
    let legacy = legacy_binary_path();
    let replaced_legacy = legacy.is_file();
    if replaced_legacy {
        std::fs::remove_file(&legacy)?;
    }

    let log = log_path();
    std::fs::create_dir_all(state_directory())?;
    std::fs::set_permissions(state_directory(), std::fs::Permissions::from_mode(0o700))?;

    let environment = daemon_environment_from_process();
    let agent = plist_path();
    if let Some(parent) = agent.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(
        &agent,
        plist(
            &binary.to_string_lossy(),
            &log.to_string_lossy(),
            &environment,
        ),
    )?;

    // Booting out first makes install idempotent, and picks up a changed plist.
    // bootout returns before the job is actually gone, and bootstrapping into a
    // half-torn-down service fails with a bare "Input/output error", so wait for
    // the service to disappear before putting it back.
    launchctl(&["bootout", &format!("{}/{LABEL}", domain())]);
    for _ in 0..50 {
        if !is_loaded() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    let (started, output) = launchctl(&["bootstrap", &domain(), &agent.to_string_lossy()]);
    if !started {
        return Err(Error::other(format!(
            "launchctl could not start {LABEL}: {}",
            output.trim()
        )));
    }

    Ok(Installed {
        bundle,
        binary,
        plist: agent,
        socket: socket_path(),
        log,
        replaced_legacy,
        environment,
    })
}

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        // `symlink_metadata`, so a symlink inside a bundle is not followed into
        // whatever it points at.
        let kind = entry.path().symlink_metadata()?.file_type();
        if kind.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else if kind.is_symlink() {
            let link = std::fs::read_link(entry.path())?;
            std::os::unix::fs::symlink(link, &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// Read `codesign -dv --verbose=2` output.
///
/// A self-signed certificate produces no `Authority` line, so the presence of a
/// signature is what distinguishes it from ad-hoc — and the field is
/// `Signature size=`, not `Signature=`.
pub fn describe_signature(text: &str) -> String {
    if text.lines().any(|line| {
        line.split_whitespace()
            .any(|word| word.starts_with("flags=") && word.contains("adhoc"))
    }) {
        return "ad-hoc".to_string();
    }
    // The first Authority line is the leaf, which is the certificate the grant
    // is anchored to. `(unavailable)` is codesign's placeholder for a chain it
    // could not build — what any machine holding the bundle but not the signing
    // key reports — and repeating it back says less than "signed" does.
    let authority = text
        .lines()
        .find_map(|line| line.strip_prefix("Authority="))
        .map(str::trim)
        .filter(|value| !value.is_empty() && *value != "(unavailable)");
    if let Some(authority) = authority {
        return authority.to_string();
    }
    if text.lines().any(|line| line.starts_with("Signature size=")) {
        "signed".to_string()
    } else {
        "unsigned".to_string()
    }
}

/// How the bundle is signed, which decides whether its grant survives a rebuild.
///
/// `--verbose=2` because plain `-dv` omits the Authority line entirely, so
/// everything came back as a bare "signed" with no way to tell which certificate
/// the grant is anchored to. It reports on stderr.
pub fn signature_of(target: &Path) -> String {
    let Ok(output) = Command::new("codesign")
        .arg("-dv")
        .arg("--verbose=2")
        .arg(target)
        .output()
    else {
        return "unsigned".to_string();
    };
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    describe_signature(&text)
}

/// What the freshly installed daemon said when asked whether it can read.
///
/// The grant is keyed to the bundle identifier and the signing certificate, not
/// to the build, so a reinstall of an already-granted daemon needs nothing
/// switched on (§9). Which is the common case while developing, and the reason
/// this is worth asking rather than assuming.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grant {
    /// It read the database, so Full Disk Access is already held.
    Held { message_count: i64 },
    /// It answered, and was refused.
    Missing,
    /// It never answered, so nothing is known.
    Unknown,
}

impl Grant {
    /// Only a daemon known to be reading is left alone.
    ///
    /// `Unknown` opens the pane. The two mistakes are not symmetrical: opening
    /// it needlessly is a window someone closes, while not opening it when the
    /// grant is missing leaves a daemon that will never work and no sign of why.
    pub fn needs_pane(self) -> bool {
        !matches!(self, Grant::Held { .. })
    }
}

fn open_pane(url: &str) {
    Command::new("open").arg(url).status().ok();
}

/// Put the pane in front of the user. There is no API for granting Full Disk
/// Access — only `tccutil` for removing one — so the last step of a *first*
/// install is always a human at System Settings (§9).
pub fn open_full_disk_access() {
    open_pane("x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles");
}

/// The pane holding the switch that decides whether the daemon may send (§13).
pub fn open_automation() {
    open_pane("x-apple.systempreferences:com.apple.preference.security?Privacy_Automation");
}

pub fn uninstall() -> Vec<PathBuf> {
    let mut removed = Vec::new();
    launchctl(&["bootout", &format!("{}/{LABEL}", domain())]);

    for path in [
        plist_path(),
        bundle_path(),
        legacy_binary_path(),
        socket_path(),
    ] {
        if !path.exists() {
            continue;
        }
        let gone = if path.is_dir() {
            std::fs::remove_dir_all(&path).is_ok()
        } else {
            std::fs::remove_file(&path).is_ok()
        };
        if gone {
            removed.push(path);
        }
    }
    // Deleting the bundle does not withdraw its grants; the entries outlive it (§9).
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        daemon_environment(|name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_string())
        })
    }

    /// Reinstalling over a working daemon is the common case while developing,
    /// and it used to open System Settings every time.
    #[test]
    fn only_a_daemon_that_cannot_read_gets_the_pane_opened() {
        assert!(!Grant::Held { message_count: 1 }.needs_pane());
        assert!(!Grant::Held { message_count: 0 }.needs_pane());
        assert!(Grant::Missing.needs_pane());
        // Not knowing is not the same as knowing it is fine.
        assert!(Grant::Unknown.needs_pane());
    }

    #[test]
    fn the_plist_is_valid_property_list_xml() {
        let directory = crate::db::temporary_directory("msg-plist-").unwrap();
        let path = directory.join("agent.plist");
        std::fs::write(
            &path,
            plist("/usr/local/libexec/msgd", "/tmp/msgd.log", &BTreeMap::new()),
        )
        .unwrap();
        let ok = Command::new("plutil")
            .arg("-lint")
            .arg(&path)
            .status()
            .unwrap()
            .success();
        std::fs::remove_dir_all(&directory).ok();
        assert!(ok, "plutil rejected the plist");
    }

    #[test]
    fn it_runs_the_binary_it_was_given_under_the_label_the_grant_is_keyed_to() {
        let text = plist("/opt/msgd", "/tmp/msgd.log", &BTreeMap::new());
        assert!(
            text.contains(&format!("<string>{LABEL}</string>")),
            "{text}"
        );
        assert!(text.contains("<string>/opt/msgd</string>"), "{text}");
        // Resident rather than socket-activated (daemon-and-permissions.md §3).
        assert!(text.contains("<key>KeepAlive</key><true/>"), "{text}");
    }

    /// TCC resolves an executable back to the bundle above it by walking up from
    /// Contents/MacOS. Anywhere else and it finds nothing, falls back to keying
    /// the grant by path, and the Automation switch stops working (§13).
    #[test]
    fn it_runs_the_executable_from_inside_contents_macos() {
        assert_eq!(binary_path(), bundle_path().join("Contents/MacOS/msgd"));
        assert!(bundle_path().to_string_lossy().ends_with(".app"));
    }

    #[test]
    fn it_installs_from_a_bundle_beside_the_binary() {
        assert!(built_bundle().to_string_lossy().ends_with("msgd.app"));
    }

    #[test]
    fn the_environment_carries_only_what_the_daemon_reads() {
        let carried = env(&[
            ("MSG_SOCKET", "/tmp/msgd.sock"),
            ("MSG_CONFIG", "/tmp/config.toml"),
            ("MSG_SIGN_IDENTITY", "msg dev"),
            ("PATH", "/usr/bin"),
            ("MSG_EMPTY", ""),
        ]);
        assert_eq!(carried.len(), 2);
        assert_eq!(carried["MSG_SOCKET"], "/tmp/msgd.sock");
        assert_eq!(carried["MSG_CONFIG"], "/tmp/config.toml");
    }

    /// The CLI answers MSG_DB locally, so persisting it could only produce a
    /// daemon pinned to a fixture that a later shell knows nothing about.
    #[test]
    fn it_never_carries_msg_db() {
        assert!(env(&[("MSG_DB", "/tmp/fixture.db")]).is_empty());
        let carried = env(&[("MSG_DB", "/tmp/fixture.db"), ("MSG_SOCKET", "/tmp/x.sock")]);
        assert_eq!(carried.len(), 1);
        assert!(carried.contains_key("MSG_SOCKET"));
    }

    #[test]
    fn it_writes_the_environment_into_the_job_since_launchd_inherits_nothing() {
        let text = plist(
            "/opt/msgd",
            "/tmp/msgd.log",
            &env(&[("MSG_SOCKET", "/tmp/msgd.sock")]),
        );
        assert!(text.contains("<key>EnvironmentVariables</key>"), "{text}");
        assert!(
            text.contains("<key>MSG_SOCKET</key><string>/tmp/msgd.sock</string>"),
            "{text}"
        );
    }

    #[test]
    fn it_omits_the_key_entirely_when_there_is_nothing_to_carry() {
        let text = plist("/opt/msgd", "/tmp/msgd.log", &BTreeMap::new());
        assert!(!text.contains("EnvironmentVariables"), "{text}");
    }

    #[test]
    fn it_escapes_values_rather_than_producing_invalid_xml() {
        let directory = crate::db::temporary_directory("msg-plist-").unwrap();
        let path = directory.join("escaped.plist");
        std::fs::write(
            &path,
            plist(
                "/opt/msgd",
                "/tmp/msgd.log",
                &env(&[("MSG_CONFIG", "/tmp/a&b<c>.toml")]),
            ),
        )
        .unwrap();
        let ok = Command::new("plutil")
            .arg("-lint")
            .arg(&path)
            .status()
            .unwrap()
            .success();
        std::fs::remove_dir_all(&directory).ok();
        assert!(ok, "plutil rejected a plist with & < > in a value");
    }

    // A self-signed certificate produces no Authority line, and the field is
    // `Signature size=`, not `Signature=` — reading it wrong reported a properly
    // signed daemon as unsigned.
    const SELF_SIGNED: &str = "Identifier=com.ninjudd.msgd
CodeDirectory v=20400 size=234857 flags=0x0(none) hashes=7334+2 location=embedded
Signature size=1660
Info.plist entries=4";

    #[test]
    fn it_recognises_a_signature_with_no_authority_line() {
        assert_eq!(describe_signature(SELF_SIGNED), "signed");
    }

    #[test]
    fn it_calls_out_ad_hoc_since_its_grant_dies_on_the_next_rebuild() {
        assert_eq!(
            describe_signature(
                "CodeDirectory v=20400 size=1 flags=0x2(adhoc) hashes=1+2\nSignature size=1"
            ),
            "ad-hoc"
        );
    }

    #[test]
    fn it_prefers_the_authority_when_there_is_one() {
        assert_eq!(
            describe_signature("Signature size=9\nAuthority=Apple Development: Someone\n"),
            "Apple Development: Someone"
        );
    }

    #[test]
    fn it_reports_an_unsigned_binary_as_unsigned() {
        assert_eq!(
            describe_signature("Identifier=x\nFormat=Mach-O thin (arm64)\n"),
            "unsigned"
        );
    }

    /// What `--verbose=2` prints on a machine holding the bundle but not the key
    /// it was signed with. Returning the placeholder reports `signed
    /// (unavailable)` where the certificate name belongs, and says less than
    /// "signed" alone.
    #[test]
    fn it_ignores_the_placeholder_codesign_prints_for_a_chain_it_cannot_build() {
        assert_eq!(
            describe_signature(
                "Identifier=com.ninjudd.msgd\nSignature size=1660\nAuthority=(unavailable)\n"
            ),
            "signed"
        );
    }

    #[test]
    fn it_takes_the_leaf_when_a_full_chain_is_printed() {
        assert_eq!(
            describe_signature(
                "Signature size=4813
Authority=Developer ID Application: Someone (ABCDE12345)
Authority=Developer ID Certification Authority
Authority=Apple Root CA"
            ),
            "Developer ID Application: Someone (ABCDE12345)"
        );
    }
}
