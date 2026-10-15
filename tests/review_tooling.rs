//! Repository-level invariants for QML, packaging and release tooling.
//!
//! These tests only read files from the source tree (and `git ls-files` when
//! Git metadata is available); they never build or run Qt.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn read(relative: &str) -> String {
    fs::read_to_string(root().join(relative))
        .unwrap_or_else(|error| panic!("cannot read {relative}: {error}"))
}

fn qml_files(relative_dir: &str) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(root().join(relative_dir))
        .unwrap_or_else(|error| panic!("cannot list {relative_dir}: {error}"))
        .map(|entry| entry.expect("directory entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "qml"))
        .collect();
    files.sort();
    files
}

fn qml_files_recursive(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("list qml directory") {
        let path = entry.expect("directory entry").path();
        if path.is_dir() {
            qml_files_recursive(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "qml") {
            out.push(path);
        }
    }
}

/// QML that is loaded by the Sailfish (Qt 5.6) application.
fn sailfish_qml() -> Vec<PathBuf> {
    let mut files = vec![root().join("app/qml/harbour-plexfreq.qml")];
    files.extend(qml_files("app/qml/pages"));
    files.extend(qml_files("app/qml/shared"));
    files
}

fn display(path: &Path) -> String {
    path.strip_prefix(root())
        .unwrap_or(path)
        .display()
        .to_string()
}

/// Replaces string literal contents and comments with spaces, keeping
/// newlines, so token scans only see executable QML/JavaScript.
fn code_only(source: &str) -> String {
    let chars: Vec<char> = source.chars().collect();
    let mut out = String::with_capacity(source.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        if c == '/' && next == Some('/') {
            while i < chars.len() && chars[i] != '\n' {
                out.push(' ');
                i += 1;
            }
        } else if c == '/' && next == Some('*') {
            out.push_str("  ");
            i += 2;
            while i < chars.len() && !(chars[i] == '*' && chars.get(i + 1) == Some(&'/')) {
                out.push(if chars[i] == '\n' { '\n' } else { ' ' });
                i += 1;
            }
            out.push_str("  ");
            i += 2;
        } else if c == '"' || c == '\'' {
            out.push(c);
            i += 1;
            while i < chars.len() && chars[i] != c && chars[i] != '\n' {
                if chars[i] == '\\' {
                    out.push(' ');
                    i += 1;
                }
                if i < chars.len() {
                    out.push(' ');
                    i += 1;
                }
            }
            if i < chars.len() {
                out.push(chars[i]);
                i += 1;
            }
        } else {
            out.push(c);
            i += 1;
        }
    }
    out
}

fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '$'
}

/// True when `keyword` occurs as a whole word followed by whitespace.
fn has_keyword_statement(line: &str, keyword: &str) -> bool {
    let mut start = 0;
    while let Some(offset) = line[start..].find(keyword) {
        let at = start + offset;
        let before = line[..at].chars().next_back();
        let after = line[at + keyword.len()..].chars().next();
        if !before.is_some_and(is_word_char) && after.is_some_and(char::is_whitespace) {
            return true;
        }
        start = at + keyword.len();
    }
    false
}

fn post_qt56_constructs(line: &str) -> Vec<&'static str> {
    let mut found = Vec::new();
    // Qt.callLater() was added in Qt 5.8; Sailfish ships qtdeclarative 5.6.3.
    if line.contains("Qt.callLater") {
        found.push("Qt.callLater (Qt 5.8+)");
    }
    if line.contains("=>") {
        found.push("arrow function (ES6)");
    }
    if line.contains('`') {
        found.push("template literal (ES6)");
    }
    if line.contains("??") {
        found.push("nullish coalescing (Qt 5.15+)");
    }
    let bytes = line.as_bytes();
    for window in bytes.windows(3) {
        if window[0] == b'?' && window[1] == b'.' && !window[2].is_ascii_digit() {
            found.push("optional chaining (Qt 5.15+)");
            break;
        }
    }
    if has_keyword_statement(line, "let") {
        found.push("let (ES6)");
    }
    if has_keyword_statement(line, "const") {
        found.push("const (ES6)");
    }
    if line.contains("required property") {
        found.push("required property (Qt 5.15+)");
    }
    let trimmed = line.trim_start();
    if trimmed.starts_with("component ")
        && trimmed["component ".len()..]
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_uppercase())
    {
        found.push("inline component (Qt 5.15+)");
    }
    found
}

/// Shared and Silica QML runs on Sailfish's Qt 5.6 V4 engine (ES5 only).
/// CI's Ubuntu Qt 5.15 qmllint accepts newer syntax and cannot detect missing
/// runtime APIs such as Qt.callLater, so this scan is the Qt 5.6 gate.
#[test]
fn sailfish_qml_avoids_apis_missing_from_qt56() {
    let mut violations = Vec::new();
    for path in sailfish_qml() {
        let source = fs::read_to_string(&path).expect("read qml");
        for (index, line) in code_only(&source).lines().enumerate() {
            for construct in post_qt56_constructs(line) {
                violations.push(format!("{}:{}: {construct}", display(&path), index + 1));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "Qt 5.6-incompatible QML/JS in Sailfish-loaded files:\n{}",
        violations.join("\n")
    );
}

/// sailfishapp installs the whole app/qml tree into the RPM, so desktop
/// QtQuick.Controls 2 pages must not live there (or must be excluded via
/// `qml.files`): they are dead weight and fail Harbour import validation.
#[test]
fn sailfish_rpm_does_not_install_desktop_controls_qml() {
    let project = read("app/harbour-plexfreq.pro");
    let explicit_qml_files = project
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("qml.files"));
    if let Some(line) = explicit_qml_files {
        let value = line.split_once('=').map(|(_, v)| v).unwrap_or("");
        let entries: Vec<&str> = value.split_whitespace().collect();
        assert!(
            !entries
                .iter()
                .any(|entry| entry.contains("desktop") || *entry == "qml" || *entry == "qml/"),
            "qml.files still installs desktop QML: {line}"
        );
        return;
    }
    let mut files = Vec::new();
    qml_files_recursive(&root().join("app/qml"), &mut files);
    files.sort();
    let offenders: Vec<String> = files
        .iter()
        .filter(|path| {
            fs::read_to_string(path)
                .expect("read qml")
                .lines()
                .any(|line| {
                    let line = line.trim();
                    line.starts_with("import QtQuick.Controls")
                        || line.starts_with("import QtQuick.Layouts")
                })
        })
        .map(|path| display(path))
        .collect();
    assert!(
        offenders.is_empty(),
        "sailfishapp installs app/qml/** into the Sailfish RPM; these desktop files import \
         QtQuick.Controls/Layouts:\n{}",
        offenders.join("\n")
    );
}

fn cargo_version() -> String {
    read("Cargo.toml")
        .lines()
        .find_map(|line| {
            let rest = line
                .strip_prefix("version")?
                .trim_start()
                .strip_prefix('=')?;
            Some(rest.trim().trim_matches('"').to_string())
        })
        .expect("package version in Cargo.toml")
}

/// The release version is checked by tools/release-version.py in Cargo, .pro,
/// spec, main.cpp and CMake, but not in QML. Embedding it in a qsTr() source
/// string also forces all 39 catalogues to change on every bump. QML should
/// use Qt.application.version instead.
#[test]
fn qml_does_not_hardcode_release_version() {
    let version = cargo_version();
    let mut files = Vec::new();
    qml_files_recursive(&root().join("app/qml"), &mut files);
    files.sort();
    let offenders: Vec<String> = files
        .iter()
        .flat_map(|path| {
            let source = fs::read_to_string(path).expect("read qml");
            source
                .lines()
                .enumerate()
                .filter(|(_, line)| line.contains(&version))
                .map(|(index, _)| format!("{}:{}", display(path), index + 1))
                .collect::<Vec<_>>()
        })
        .collect();
    assert!(
        offenders.is_empty(),
        "QML hard-codes release version {version}; use Qt.application.version:\n{}",
        offenders.join("\n")
    );
}

/// Scratch reproduction programs must not be committed at the repository
/// root: Cargo never builds them, so they silently rot.
#[test]
fn stray_repro_sources_are_not_tracked() {
    let output = Command::new("git")
        .arg("-C")
        .arg(root())
        .args(["ls-files", "--", "*.rs"])
        .output();
    let Ok(output) = output else {
        eprintln!("git unavailable; skipping tracked-file hygiene check");
        return;
    };
    if !output.status.success() {
        eprintln!("no git metadata; skipping tracked-file hygiene check");
        return;
    }
    let listing = String::from_utf8_lossy(&output.stdout);
    let stray: Vec<&str> = listing.lines().filter(|path| !path.contains('/')).collect();
    assert!(
        stray.is_empty(),
        "Rust sources tracked at the repository root (not part of any Cargo target): {stray:?}"
    );
}

/// Session.enqueue() only handles tracks and albums. Desktop context-menu
/// "Play next"/"Add to queue" entries must be hidden for other item types
/// (as the Silica page does) instead of silently doing nothing.
#[test]
fn desktop_item_menu_queue_actions_match_session_enqueue() {
    let source = read("app/qml/desktop/Main.qml");
    let offenders: Vec<String> = source
        .lines()
        .enumerate()
        .filter(|(_, line)| {
            (line.contains("qsTr(\"Play next\")") || line.contains("qsTr(\"Add to queue\")"))
                && !line.contains("type===\"track\"")
        })
        .map(|(index, line)| format!("app/qml/desktop/Main.qml:{}: {}", index + 1, line.trim()))
        .collect();
    assert!(
        offenders.is_empty(),
        "queue actions shown for item types Session.enqueue ignores:\n{}",
        offenders.join("\n")
    );
}

/// Screen-reader names and visible state labels must go through qsTr.
#[test]
fn user_visible_qml_strings_are_translated() {
    let mut files = Vec::new();
    qml_files_recursive(&root().join("app/qml"), &mut files);
    files.sort();
    let mut offenders = Vec::new();
    for path in &files {
        let source = fs::read_to_string(path).expect("read qml");
        for (index, line) in source.lines().enumerate() {
            let literal_accessible_name = line
                .split("Accessible.name:")
                .skip(1)
                .any(|rest| rest.trim_start().starts_with('"'));
            // Raw "off"/"all"/"one" queue repeat mode shown to the user.
            let raw_repeat_mode = line.contains("+ backend.state.queue.repeat");
            if literal_accessible_name || raw_repeat_mode {
                offenders.push(format!("{}:{}: {}", display(path), index + 1, line.trim()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "untranslated user-visible QML text:\n{}",
        offenders.join("\n")
    );
}

/// Third-party actions run with the workflow token (contents: write in the
/// publish job); mutable tags/branches such as @stable or @v2 must be pinned
/// to a full commit SHA.
#[test]
fn third_party_workflow_actions_are_pinned_to_commit_sha() {
    let mut offenders = Vec::new();
    for workflow in [".github/workflows/ci.yml", ".github/workflows/release.yml"] {
        for (index, line) in read(workflow).lines().enumerate() {
            let Some(reference) = line.trim().trim_start_matches("- ").strip_prefix("uses:") else {
                continue;
            };
            let reference = reference.trim().trim_matches(|c| c == '\'' || c == '"');
            if reference.starts_with("./") || reference.starts_with("actions/") {
                continue;
            }
            let pinned = reference.split_once('@').is_some_and(|(_, rev)| {
                rev.len() == 40 && rev.chars().all(|c| c.is_ascii_hexdigit())
            });
            if !pinned {
                offenders.push(format!("{workflow}:{}: {reference}", index + 1));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "third-party actions not pinned to a commit SHA:\n{}",
        offenders.join("\n")
    );
}

/// Release RPMs are built from tools/audio-sdk.Dockerfile. A floating
/// `:latest` base makes releases non-reproducible and can drop the hard-coded
/// SailfishOS-5.2.0.15 target without any source change.
#[test]
fn sailfish_sdk_base_image_is_pinned_by_digest() {
    let dockerfile = read("tools/audio-sdk.Dockerfile");
    let from = dockerfile
        .lines()
        .map(str::trim)
        .find(|line| line.to_ascii_uppercase().starts_with("FROM "))
        .expect("FROM line");
    assert!(
        from.contains("@sha256:"),
        "SDK base image is not pinned by digest: {from}"
    );
}
