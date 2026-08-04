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
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-m",
            message,
        ],
    );
}

fn tag_annotated(dir: &Path, target: &str, message: &str) {
    run_git(
        dir,
        &[
            "-c",
            "user.name=Fleet Test",
            "-c",
            "user.email=fleet@example.invalid",
            "-c",
            "tag.gpgSign=false",
            "tag",
            "-a",
            "v0.13.0",
            "-m",
            message,
            target,
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
        tag_annotated(&upstream_work, &first_base, "older official tag");
        fs::write(
            upstream_work.join("src/main.rs"),
            "fn main() { println!(\"newer\"); }\n",
        )
        .unwrap();
        commit_all(&upstream_work, "upstream base two");
    }

    let upstream_base = git_stdout(&upstream_work, &["rev-parse", "HEAD"]);
    if matches!(tag, UpstreamTag::Matching) {
        tag_annotated(&upstream_work, &upstream_base, "official tag");
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
    run_git(
        &source,
        &["remote", "add", "upstream", upstream_bare.to_str().unwrap()],
    );
    run_git(
        &source,
        &["fetch", "upstream", "main:refs/remotes/upstream/main"],
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

fn run_guard_with(
    fixture: &Fixture,
    mode: &str,
    revision: &str,
    include_base: bool,
    upstream_url: &Path,
) -> Output {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/ci/plan-fleet-release.sh");
    let mut command = Command::new("bash");
    command.current_dir(&fixture.source).arg(script).args([
        "--mode",
        mode,
        "--revision",
        revision,
        "--source-ref",
        "HEAD",
        "--upstream-url",
        upstream_url.to_str().unwrap(),
    ]);
    if include_base {
        command.args(["--upstream-base", &fixture.upstream_base]);
    }
    command.output().expect("fleet release guard should start")
}

fn run_guard(fixture: &Fixture, mode: &str, revision: &str) -> Output {
    run_guard_with(fixture, mode, revision, true, &fixture.upstream_bare)
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
fn planner_derives_the_upstream_base_from_the_tracking_ref() {
    let fixture = fixture(UpstreamTag::Missing);
    let fields = plan(&run_guard_with(
        &fixture,
        "snapshot",
        "1",
        false,
        &fixture.upstream_bare,
    ));
    assert_eq!(fields["upstream_base"], fixture.upstream_base);
}

#[test]
fn planner_reads_the_manifest_from_the_selected_source_commit() {
    let fixture = fixture(UpstreamTag::Missing);
    fs::write(
        fixture.source.join("Cargo.toml"),
        "[package]\nname = \"kache\"\nversion = \"9.9.9\"\nedition = \"2024\"\n",
    )
    .unwrap();
    let fields = plan(&run_guard(&fixture, "snapshot", "1"));
    assert_eq!(fields["version"], "0.13.0");
}

#[test]
fn planner_fails_closed_when_upstream_tag_lookup_fails() {
    let fixture = fixture(UpstreamTag::Missing);
    let missing = fixture._temp.path().join("missing-upstream.git");
    let output = run_guard_with(&fixture, "snapshot", "1", true, &missing);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("failed to query upstream tag"));
}

#[test]
fn fleet_releases_cannot_trigger_public_package_publishers() {
    let packages = include_str!("../.github/workflows/package-publish.yml");
    let crates = include_str!("../.github/workflows/publish-crates.yaml");
    let fleet = include_str!("../.github/workflows/fleet-release.yml");

    let guard = "!startsWith(github.event.release.tag_name, 'fleet-')";
    let package_resolve = packages
        .split("\n  resolve:\n")
        .nth(1)
        .and_then(|rest| rest.split("\n  gate:\n").next())
        .expect("package resolve job");
    let crates_publish = crates
        .split("\n  publish:\n")
        .nth(1)
        .expect("crates publish job");
    assert!(
        package_resolve.contains(guard),
        "OS package resolve job must ignore fleet-only releases"
    );
    assert!(
        crates_publish.contains(guard),
        "crates.io publish job must ignore fleet-only releases"
    );

    let publish_input = fleet
        .split("\n      publish:\n")
        .nth(1)
        .and_then(|rest| rest.split("\n\npermissions:").next())
        .expect("publish workflow input");
    assert!(publish_input.contains("type: boolean"));
    assert!(publish_input.contains("default: false"));

    let validate_job = fleet
        .split("\n  validate:\n")
        .nth(1)
        .and_then(|rest| rest.split("\n  publish:\n").next())
        .expect("validate job");
    assert!(validate_job.contains("permissions:\n      contents: read"));
    assert!(validate_job.contains("ref: main"));
    assert!(!fleet.contains("source_ref:"));

    let publish_job = fleet.split("\n  publish:\n").nth(1).expect("publish job");
    assert!(publish_job.contains("permissions:\n      contents: write"));
    assert!(publish_job.contains("actions/download-artifact@"));
    assert!(publish_job.contains("git push origin \":refs/tags/$FLEET_TAG\""));
    assert!(publish_job.contains("gh release delete \"$FLEET_TAG\""));
}
