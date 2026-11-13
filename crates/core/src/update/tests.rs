use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;

use super::*;

/// Serves `files` (path to body) over loopback HTTP; any other path is 404,
/// and a body of `None` is a 500. Returns the base URL.
fn serve(files: HashMap<String, Option<Vec<u8>>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut request = String::new();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            if reader.read_line(&mut request).is_err() {
                continue;
            }
            // Read the headers to the blank line.
            let mut line = String::new();
            while reader.read_line(&mut line).is_ok_and(|n| n > 2) {
                line.clear();
            }
            let path = request.split_whitespace().nth(1).unwrap_or("").to_owned();
            let (status, body) = match files.get(&path) {
                Some(Some(body)) => ("200 OK", body.clone()),
                Some(None) => ("500 Internal Server Error", Vec::new()),
                None => ("404 Not Found", Vec::new()),
            };
            let _ = write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(&body);
        }
    });
    base
}

fn manifest(version: &str, build: u64) -> Option<Vec<u8>> {
    Some(
        serde_json::json!({
            "version": version, "build": build, "commit": "abc1234",
            "notes": "https://example.invalid/notes",
            "macos-arm64": {"url": "https://example.invalid/Eddie.zip",
                "sha256": "00", "size": 1},
        })
        .to_string()
        .into_bytes(),
    )
}

fn running(version: Version, build: Option<u64>) -> Build {
    Build { version, build }
}

#[test]
fn versions_compare_as_numbers() {
    assert_eq!(Version::parse("v0.10.0"), Some(Version(0, 10, 0)));
    assert!(Version::parse("0.10.0") > Version::parse("0.9.9"));
    for bad in ["0.2", "0.2.0.1", "0.2.x", "", "v"] {
        assert_eq!(Version::parse(bad), None, "{bad:?}");
    }
    assert_eq!(this_build().version.to_string(), env!("CARGO_PKG_VERSION"));
}

#[test]
fn a_later_commit_is_newer_only_to_a_build_that_knows_its_own() {
    let release = |version: &str, build| Release {
        version: version.to_owned(),
        build,
        commit: String::new(),
        notes: String::new(),
        macos_arm64: None,
    };
    let v020 = Version(0, 2, 0);
    // A preview of the same version, 12 commits on.
    assert!(release("0.2.0", 140).is_newer_than(&running(v020, Some(128))));
    assert!(!release("0.2.0", 128).is_newer_than(&running(v020, Some(128))));
    // Built by hand: no commit count, so only a new version counts.
    assert!(!release("0.2.0", 140).is_newer_than(&running(v020, None)));
    assert!(release("0.3.0", 1).is_newer_than(&running(v020, None)));
    // The version outranks the commit count.
    assert!(!release("0.1.9", 900).is_newer_than(&running(v020, Some(128))));
    assert!(release("0.3.0", 100).is_newer_than(&running(v020, Some(128))));
    assert!(!release("nonsense", 900).is_newer_than(&running(v020, Some(1))));
}

#[test]
fn each_channel_reads_its_own_releases() {
    let base = serve(HashMap::from([
        (
            "/latest/download/update.json".to_owned(),
            manifest("0.3.0", 150),
        ),
        (
            "/download/preview/update.json".to_owned(),
            manifest("0.3.0", 162),
        ),
    ]));
    let here = running(Version(0, 2, 0), Some(128));
    let found = |channel| check(&base, channel, &here).unwrap().map(|r| r.build);
    assert_eq!(
        found(Channel::Releases),
        Some(150),
        "previews are not offered"
    );
    assert_eq!(found(Channel::Previews), Some(162), "the newer of the two");
    assert_eq!(found(Channel::Off), None);

    // Up to date with the release, a preview user still sees the preview.
    let current = running(Version(0, 3, 0), Some(150));
    assert!(check(&base, Channel::Releases, &current).unwrap().is_none());
    assert_eq!(
        check(&base, Channel::Previews, &current)
            .unwrap()
            .map(|r| r.build),
        Some(162)
    );
}

#[test]
fn nothing_published_is_no_update_and_a_broken_server_is_an_error() {
    let here = running(Version(0, 2, 0), Some(128));
    let empty = serve(HashMap::new());
    assert_eq!(check(&empty, Channel::Previews, &here), Ok(None));

    let broken = serve(HashMap::from([(
        "/latest/download/update.json".to_owned(),
        None,
    )]));
    assert!(check(&broken, Channel::Releases, &here).is_err());
    // One answer is enough for Previews: the preview is still found.
    let half = serve(HashMap::from([
        ("/latest/download/update.json".to_owned(), None),
        (
            "/download/preview/update.json".to_owned(),
            manifest("0.2.0", 130),
        ),
    ]));
    assert_eq!(
        check(&half, Channel::Previews, &here)
            .unwrap()
            .map(|r| r.build),
        Some(130)
    );
}

#[test]
fn a_download_is_kept_only_when_it_is_the_published_file() {
    let body = b"the app, zipped".to_vec();
    // `printf 'the app, zipped' | sha256sum`
    let digest = "9403bc072dba28834e91f3cc9f00b6d58161393ba61881e10e25fc0e94bde217";
    let base = serve(HashMap::from([(
        "/Eddie.zip".to_owned(),
        Some(body.clone()),
    )]));
    let dir = std::env::temp_dir().join(format!("eddie-update-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let to = dir.join("Eddie.zip");
    let file = |sha256: &str, size: usize| Download {
        url: format!("{base}/Eddie.zip"),
        sha256: sha256.to_owned(),
        size: size as u64,
    };

    download(&file(&digest.to_uppercase(), body.len()), &to).unwrap();
    assert_eq!(std::fs::read(&to).unwrap(), body);

    for (wrong, why) in [
        (file(&"0".repeat(64), body.len()), "another checksum"),
        (file(digest, body.len() - 1), "longer than published"),
        (file(digest, body.len() + 1), "shorter than published"),
    ] {
        std::fs::write(&to, "stale").unwrap();
        assert!(download(&wrong, &to).is_err(), "{why}");
        assert!(!to.exists(), "{why}: nothing is left behind");
    }
    std::fs::remove_dir_all(dir).unwrap();
}
