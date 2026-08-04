use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Fixture {
    _temp: tempfile::TempDir,
    source: PathBuf,
    upstream_bare: PathBuf,
    upstream_base: String,
    source_sha: String,
}

#[derive(Clone, Copy)]
enum UpstreamTag {
    Missing,
    Matching,
    Mismatched,
}

fn run_git(dir: &Path, args: &[&str]) -> Output {
    let output = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .expect("git command should start");
    assert!(
        output.status.success(),
        "git {:?} failed\nstdout:\n{}\nstderr:\n{}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn git_stdout(dir: &Path, args: &[&str]) -> String {
    String::from_utf8(run_git(dir, args).stdout)
        .expect("git stdout is utf8")
        .trim()
        .to_string()
}

fn commit_all(dir: &Path, message: &str) {
    run_git(dir, &["add", "."]);
    run_git(
        dir,
        &[
            "-c",
            "user.name=Fleet Test",
            "-c",
            "user.email=fleet@example.invalid",
            "commit",
            "-m",
            message,
        ],
    );
}

fn fixture(tag: UpstreamTag) -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let upstream_work = temp.path().join("upstream-work");
    let upstream_bare = temp.path().join("upstream.git");
    let source = temp.path().join("source");

    fs::create_dir_all(upstream_work.join("src")).unwrap();
    run_git(
        temp.path(),
        &["init", "-b", "main", upstream_work.to_str().unwrap()],
    );
    fs::write(
        upstream_work.join("Cargo.toml"),
        "[package]\nname = \"kache\"\nversion = \"0.13.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    fs::write(upstream_work.join("src/main.rs"), "fn main() {}\n").unwrap();
    commit_all(&upstream_work, "upstream base one");
    let first_base = git_stdout(&upstream_work, &["rev-parse", "HEAD"]);

    if matches!(tag, UpstreamTag::Mismatched) {
        run_git(
            &upstream_work,
            &[
                "-c",
                "user.name=Fleet Test",
                "-c",
                "user.email=fleet@example.invalid",
                "tag",
                "-a",
                "v0.13.0",
                "-m",
                "older official tag",
                &first_base,
            ],
        );
        fs::write(
            upstream_work.join("src/main.rs"),
            "fn main() { println!(\"newer\"); }\n",
        )
        .unwrap();
        commit_all(&upstream_work, "upstream base two");
    }

    let upstream_base = git_stdout(&upstream_work, &["rev-parse", "HEAD"]);
    if matches!(tag, UpstreamTag::Matching) {
        run_git(
            &upstream_work,
            &[
                "-c",
                "user.name=Fleet Test",
                "-c",
                "user.email=fleet@example.invalid",
                "tag",
                "-a",
                "v0.13.0",
                "-m",
                "official tag",
                &upstream_base,
            ],
        );
    }

    run_git(
        temp.path(),
        &[
            "clone",
            "--bare",
            upstream_work.to_str().unwrap(),
            upstream_bare.to_str().unwrap(),
        ],
    );
    run_git(
        temp.path(),
        &[
            "clone",
            upstream_work.to_str().unwrap(),
            source.to_str().unwrap(),
        ],
    );
    fs::write(source.join("fleet.txt"), "fork-only fleet controls\n").unwrap();
    commit_all(&source, "fork fleet change");
    let source_sha = git_stdout(&source, &["rev-parse", "HEAD"]);

    Fixture {
        _temp: temp,
        source,
        upstream_bare,
        upstream_base,
        source_sha,
    }
}

fn run_guard(fixture: &Fixture, mode: &str, revision: &str) -> Output {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/ci/plan-fleet-release.sh");
    Command::new("bash")
        .current_dir(&fixture.source)
        .arg(script)
        .args([
            "--mode",
            mode,
            "--revision",
            revision,
            "--source-ref",
            "HEAD",
            "--upstream-url",
            fixture.upstream_bare.to_str().unwrap(),
            "--upstream-base",
            &fixture.upstream_base,
        ])
        .output()
        .expect("fleet release guard should start")
}

fn plan(output: &Output) -> HashMap<String, String> {
    assert!(
        output.status.success(),
        "guard failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout.clone())
        .unwrap()
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

#[test]
fn snapshot_without_upstream_tag_uses_sha_prerelease_name() {
    let fixture = fixture(UpstreamTag::Missing);
    let fields = plan(&run_guard(&fixture, "snapshot", "3"));
    assert_eq!(fields["mode"], "snapshot");
    assert_eq!(fields["upstream_tag_status"], "missing");
    assert_eq!(fields["prerelease"], "true");
    assert_eq!(
        fields["fleet_tag"],
        format!("fleet-snapshot-v0.13.0-{}.3", &fixture.source_sha[..7])
    );
}

#[test]
fn official_mode_requires_the_upstream_version_tag() {
    let fixture = fixture(UpstreamTag::Missing);
    let output = run_guard(&fixture, "official", "1");
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("official mode requires upstream tag v0.13.0")
    );
}

#[test]
fn official_mode_requires_the_tag_to_match_the_included_base() {
    let fixture = fixture(UpstreamTag::Mismatched);
    let output = run_guard(&fixture, "official", "1");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("not included base"));
}

#[test]
fn official_mode_with_matching_tag_uses_stable_fleet_name() {
    let fixture = fixture(UpstreamTag::Matching);
    let fields = plan(&run_guard(&fixture, "official", "2"));
    assert_eq!(fields["mode"], "official");
    assert_eq!(fields["upstream_tag_status"], "matches");
    assert_eq!(fields["upstream_tag_commit"], fixture.upstream_base);
    assert_eq!(fields["fleet_tag"], "fleet-v0.13.0.2");
    assert_eq!(fields["prerelease"], "false");
}

#[test]
fn snapshot_mode_refuses_a_base_that_is_already_official() {
    let fixture = fixture(UpstreamTag::Matching);
    let output = run_guard(&fixture, "snapshot", "1");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("use --mode official"));
}

#[test]
fn fleet_releases_cannot_trigger_public_package_publishers() {
    let packages = include_str!("../.github/workflows/package-publish.yml");
    let crates = include_str!("../.github/workflows/publish-crates.yaml");
    let fleet = include_str!("../.github/workflows/fleet-release.yml");
    let planner = include_str!("../scripts/ci/plan-fleet-release.sh");

    let guard = "!startsWith(github.event.release.tag_name, 'fleet-')";
    assert!(
        packages.contains(guard),
        "OS package publication must ignore fleet-only releases"
    );
    assert!(
        crates.contains(guard),
        "crates.io publication must ignore fleet-only releases"
    );
    assert!(fleet.contains("scripts/ci/plan-fleet-release.sh"));
    assert!(fleet.contains("default: false"));
    assert!(planner.contains("fleet-snapshot-v${version}-${source_short}.${revision}"));
}
