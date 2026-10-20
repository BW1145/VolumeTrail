"""Package the verified 0.2.2 Actions artifact; optionally publish and verify it."""
import argparse
import hashlib
import json
from pathlib import Path
import struct
import subprocess
import tempfile
import tomllib
import zipfile

REPO = "BW1145/VolumeTrail"
VERSION = "0.2.2"
TAG = "v" + VERSION
SOURCE = "9bbe8567bc73593bed72ffdbf7dedf93c9ee3da9"
RUN = 39060304453
ARTIFACT = 12110654934
ARCHIVE_HASH = "8b917f4403115a73bee59f513e0e34c1efd3f0a5c24ca1813b3f03b9de7642bf"
EXE_HASHES = {
    "VolumeTrail-cli.exe": "b4cf97965610a082d114027c8e691c8af07a79caeef81c54ab047453d450f856",
    "VolumeTrail.exe": "5b1d27d4771802dabb9c74a12996e6eb152b6223b64bfdbce5217a2ae7241ce8",
}
ROOT = Path(__file__).resolve().parents[1]


def check(condition, message):
    if not condition:
        raise RuntimeError(message)


def sha(data):
    return hashlib.sha256(data).hexdigest()


def gh(*args):
    return subprocess.check_output(["gh", *map(str, args)])


def api(path, *args):
    return json.loads(gh("api", f"repos/{REPO}/{path}", *args))


def resolve_tag(ref):
    obj = ref["object"]
    while obj["type"] == "tag":
        obj = api("git/tags/" + obj["sha"])["object"]
    check(obj["type"] == "commit", "Tag does not resolve to a commit")
    return obj["sha"]


def package(artifact, output):
    raw = artifact.read_bytes()
    check(sha(raw) == ARCHIVE_HASH, "Original artifact SHA-256 mismatch")
    with zipfile.ZipFile(artifact) as source:
        check(source.testzip() is None, "Original artifact CRC error")
        names = source.namelist()
        expected = {*EXE_HASHES, "COMMIT.txt", "SHA256SUMS.txt"}
        check(len(names) == len(expected) and set(names) == expected, "Unexpected artifact contents")
        files = {name: source.read(name) for name in names}
    check(files["COMMIT.txt"].decode("utf-8-sig").strip() == SOURCE, "Source commit mismatch")
    recorded = {}
    for line in files["SHA256SUMS.txt"].decode("utf-8-sig").splitlines():
        digest, name = line.split(None, 1)
        recorded[name.strip()] = digest.lower()
    check(recorded == EXE_HASHES, "Embedded checksums mismatch")
    for name, digest in EXE_HASHES.items():
        binary = files[name]
        check(sha(binary) == digest, f"{name}: SHA-256 mismatch")
        check(binary[:2] == b"MZ", f"{name}: DOS header missing")
        offset = struct.unpack_from("<I", binary, 0x3C)[0]
        check(binary[offset:offset + 4] == b"PE\0\0", f"{name}: PE header missing")
        check(struct.unpack_from("<H", binary, offset + 4)[0] == 0x8664, f"{name}: not x64")
        subsystem = struct.unpack_from("<H", binary, offset + 24 + 68)[0]
        check(subsystem == (2 if name == "VolumeTrail.exe" else 3), f"{name}: wrong subsystem")
    files["README.zh-CN.md"] = (ROOT / f"release/{VERSION}.md").read_bytes()
    output.mkdir(parents=True, exist_ok=True)
    target = output / f"VolumeTrail-{VERSION}-windows-x64-portable.zip"
    with zipfile.ZipFile(target, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=9) as dest:
        for name in sorted(files):
            info = zipfile.ZipInfo("VolumeTrail/" + name, (2026, 10, 17, 19, 0, 18))
            info.compress_type = zipfile.ZIP_DEFLATED
            info.external_attr = 0o100644 << 16
            dest.writestr(info, files[name], compresslevel=9)
    with zipfile.ZipFile(target) as dest:
        check(dest.testzip() is None, "Portable ZIP CRC error")
        check(len(dest.namelist()) == 5, "Portable ZIP file count mismatch")
        for name, data in files.items():
            check(dest.read("VolumeTrail/" + name) == data, "Portable ZIP content mismatch")
    checksum = target.with_suffix(target.suffix + ".sha256")
    checksum.write_text(f"{sha(target.read_bytes())}  {target.name}\n", encoding="ascii")
    print(json.dumps({"zip": str(target), "bytes": target.stat().st_size,
                      "sha256": sha(target.read_bytes()), "executables": EXE_HASHES}, indent=2))
    return target, checksum


def publish(output):
    run = api(f"actions/runs/{RUN}")
    check(run["head_sha"] == SOURCE and run["conclusion"] == "success", "Build run mismatch/failure")
    artifact = api(f"actions/artifacts/{ARTIFACT}")
    check(not artifact["expired"] and artifact["workflow_run"]["id"] == RUN
          and artifact["workflow_run"]["head_sha"] == SOURCE, "Artifact provenance mismatch")
    check(artifact["digest"] == "sha256:" + ARCHIVE_HASH, "Artifact metadata digest mismatch")
    cargo = gh("api", f"repos/{REPO}/contents/Cargo.toml?ref={SOURCE}",
               "-H", "Accept: application/vnd.github.raw+json").decode()
    check(tomllib.loads(cargo)["package"]["version"] == VERSION, "Program version mismatch")
    refs = api(f"git/matching-refs/tags/{TAG}")
    ref = next((r for r in refs if r["ref"] == "refs/tags/" + TAG), None)
    if ref:
        check(resolve_tag(ref) == SOURCE, "Existing version tag points elsewhere")
    releases = api("releases?per_page=100")
    existing = next((r for r in releases if r["tag_name"] == TAG), None)
    check(existing is None or (existing["draft"] and not existing["prerelease"]),
          "A published release already exists; inspect it before retrying")
    if existing:
        check(existing["body"].strip() == (ROOT / f"release/{VERSION}.md").read_text().strip(),
              "Existing draft has different release notes")
    with tempfile.TemporaryDirectory() as temp:
        archive = Path(temp) / "original.zip"
        with archive.open("wb") as stream:
            subprocess.run(["gh", "api", f"repos/{REPO}/actions/artifacts/{ARTIFACT}/zip"],
                           stdout=stream, check=True)
        check(archive.stat().st_size == artifact["size_in_bytes"], "Artifact size mismatch")
        target, checksum = package(archive, output)
    if ref is None:
        api("git/refs", "-X", "POST", "-f", f"ref=refs/tags/{TAG}", "-f", f"sha={SOURCE}")
    if existing is None:
        gh("release", "create", TAG, "--repo", REPO, "--verify-tag", "--draft",
           "--title", f"盘迹 · VolumeTrail {VERSION}（Windows x64 便携版）",
           "--notes-file", ROOT / f"release/{VERSION}.md")
        existing = next(r for r in api("releases?per_page=100") if r["tag_name"] == TAG)
    release_id = existing["id"]
    uploaded = {a["name"] for a in existing["assets"]}
    for local in (target, checksum):
        if local.name not in uploaded:
            gh("release", "upload", TAG, local, "--repo", REPO)
    # Draft releases are addressed by ID; the public tag endpoint returns 404.
    release = api(f"releases/{release_id}")
    check(release["draft"] and not release["prerelease"], "Release is not a stable draft")
    check({a["name"] for a in release["assets"]} == {target.name, checksum.name}, "Release assets mismatch")
    # Verify bytes served by GitHub before making the release public.
    with tempfile.TemporaryDirectory() as temp:
        gh("release", "download", TAG, "--repo", REPO, "--dir", temp)
        for local in (target, checksum):
            remote = Path(temp) / local.name
            check(remote.stat().st_size == local.stat().st_size
                  and sha(remote.read_bytes()) == sha(local.read_bytes()), "Uploaded asset mismatch")
    gh("release", "edit", TAG, "--repo", REPO, "--draft=false", "--latest")
    release = api("releases/tags/" + TAG)
    check(not release["draft"] and not release["prerelease"], "Release publication failed")
    check(resolve_tag(api("git/ref/tags/" + TAG)) == SOURCE, "Published tag mismatch")
    # Check the now-public download URLs, not just the authenticated draft download.
    import urllib.request
    for asset in release["assets"]:
        local = output / asset["name"]
        with urllib.request.urlopen(asset["browser_download_url"], timeout=120) as response:
            remote = response.read()
        check(len(remote) == local.stat().st_size and sha(remote) == sha(local.read_bytes()),
              "Public release download mismatch")
        print(asset["browser_download_url"])
    print(release["html_url"])


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--artifact", type=Path, help="Verify and package a local original artifact without publishing")
    parser.add_argument("--output", type=Path, default=ROOT / "dist/release")
    parser.add_argument("--publish", action="store_true")
    args = parser.parse_args()
    check(bool(args.artifact) != args.publish, "Choose --artifact or --publish")
    if args.publish:
        publish(args.output)
    else:
        package(args.artifact, args.output)
