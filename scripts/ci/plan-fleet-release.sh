#!/usr/bin/env bash
# Plan and validate a fork-only Kache fleet release.
#
# Official mode is fail-closed: the upstream v<manifest-version> tag must exist
# and resolve to the exact upstream base included by the fleet source commit.
# Snapshot mode is explicit and produces a SHA-bearing prerelease tag. It is
# rejected when the official upstream tag already matches the included base.
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: plan-fleet-release.sh --mode official|snapshot [options]

Options:
  --revision N          Positive fleet release revision (default: 1)
  --source-ref REF      Fleet source commit/ref (default: HEAD)
  --upstream-url URL    Upstream git URL (default: kunobi-ninja/kache)
  --upstream-base REF   Exact included upstream commit. When omitted, derive
                        it as merge-base(source, refs/remotes/upstream/main).
  --output PATH         Write key=value plan fields to PATH (default: stdout)
  -h, --help            Show this help
EOF
}

die() {
  printf 'fleet release guard: %s\n' "$*" >&2
  exit 1
}

mode=""
revision="1"
source_ref="HEAD"
upstream_url="https://github.com/kunobi-ninja/kache.git"
upstream_base_ref=""
output="-"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --mode)
      [[ $# -ge 2 ]] || die "--mode requires a value"
      mode="$2"
      shift 2
      ;;
    --revision)
      [[ $# -ge 2 ]] || die "--revision requires a value"
      revision="$2"
      shift 2
      ;;
    --source-ref)
      [[ $# -ge 2 ]] || die "--source-ref requires a value"
      source_ref="$2"
      shift 2
      ;;
    --upstream-url)
      [[ $# -ge 2 ]] || die "--upstream-url requires a value"
      upstream_url="$2"
      shift 2
      ;;
    --upstream-base)
      [[ $# -ge 2 ]] || die "--upstream-base requires a value"
      upstream_base_ref="$2"
      shift 2
      ;;
    --output)
      [[ $# -ge 2 ]] || die "--output requires a value"
      output="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      die "unknown argument: $1"
      ;;
  esac
done

[[ "$mode" == "official" || "$mode" == "snapshot" ]] ||
  die "--mode must be official or snapshot"
[[ "$revision" =~ ^[1-9][0-9]*$ ]] ||
  die "--revision must be a positive integer"

repo_root="$(git rev-parse --show-toplevel 2>/dev/null)" ||
  die "run this command inside a git repository"
cd "$repo_root"

source_sha="$(git rev-parse "${source_ref}^{commit}" 2>/dev/null)" ||
  die "source ref does not resolve to a commit: $source_ref"
source_short="${source_sha:0:7}"

manifest="$(git show "${source_sha}:Cargo.toml" 2>/dev/null)" ||
  die "Cargo.toml is missing from source commit $source_sha"
version="$(awk '
  /^\[package\]$/ { in_package = 1; next }
  in_package && /^\[/ { exit }
  in_package && $1 == "version" {
    value = $3
    gsub(/"/, "", value)
    print value
    exit
  }
' <<<"$manifest")"
[[ -n "$version" ]] ||
  die "could not read [package] version from source commit $source_sha"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+([-.][0-9A-Za-z.-]+)?$ ]] ||
  die "manifest version is not valid semver-like text: $version"

if [[ -n "$upstream_base_ref" ]]; then
  upstream_base="$(git rev-parse "${upstream_base_ref}^{commit}" 2>/dev/null)" ||
    die "upstream base does not resolve to a local commit: $upstream_base_ref"
else
  git rev-parse --verify refs/remotes/upstream/main >/dev/null 2>&1 ||
    die "--upstream-base is required unless refs/remotes/upstream/main exists"
  upstream_base="$(git merge-base "$source_sha" refs/remotes/upstream/main)" ||
    die "could not derive upstream base from source and upstream/main"
fi

git merge-base --is-ancestor "$upstream_base" "$source_sha" ||
  die "upstream base $upstream_base is not an ancestor of source $source_sha"

upstream_tag="v${version}"
if ! tag_output="$(git ls-remote --tags "$upstream_url" \
  "refs/tags/${upstream_tag}" "refs/tags/${upstream_tag}^{}" 2>&1)"; then
  die "failed to query upstream tag $upstream_tag at $upstream_url: $tag_output"
fi

tag_direct=""
tag_peeled=""
while read -r sha ref; do
  [[ -n "${sha:-}" && -n "${ref:-}" ]] || continue
  case "$ref" in
    "refs/tags/${upstream_tag}") tag_direct="$sha" ;;
    "refs/tags/${upstream_tag}^{}") tag_peeled="$sha" ;;
  esac
done <<<"$tag_output"
upstream_tag_commit="${tag_peeled:-$tag_direct}"

case "$mode" in
  official)
    [[ -n "$upstream_tag_commit" ]] ||
      die "official mode requires upstream tag $upstream_tag at $upstream_url"
    [[ "$upstream_tag_commit" == "$upstream_base" ]] ||
      die "upstream tag $upstream_tag resolves to $upstream_tag_commit, not included base $upstream_base"
    upstream_tag_status="matches"
    fleet_tag="fleet-v${version}.${revision}"
    release_title="Kache ${version} fleet release ${revision}"
    prerelease="false"
    ;;
  snapshot)
    if [[ -z "$upstream_tag_commit" ]]; then
      upstream_tag_status="missing"
    elif [[ "$upstream_tag_commit" == "$upstream_base" ]]; then
      die "upstream tag $upstream_tag now matches the included base; use --mode official"
    else
      upstream_tag_status="differs"
    fi
    fleet_tag="fleet-snapshot-v${version}-${source_short}.${revision}"
    release_title="Kache fleet snapshot ${version} (${source_short})"
    prerelease="true"
    ;;
esac

if [[ "$output" == "-" ]]; then
  output_path="/dev/stdout"
else
  output_path="$output"
  : >"$output_path"
fi

emit() {
  printf '%s=%s\n' "$1" "$2" >>"$output_path"
}

emit mode "$mode"
emit version "$version"
emit revision "$revision"
emit source_sha "$source_sha"
emit source_short "$source_short"
emit upstream_url "$upstream_url"
emit upstream_base "$upstream_base"
emit upstream_tag "$upstream_tag"
emit upstream_tag_commit "$upstream_tag_commit"
emit upstream_tag_status "$upstream_tag_status"
emit fleet_tag "$fleet_tag"
emit release_title "$release_title"
emit prerelease "$prerelease"

printf 'fleet release guard: %s plan validated for %s\n' "$mode" "$fleet_tag" >&2
