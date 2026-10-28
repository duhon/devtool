import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import tarfile
import tomllib
from urllib.error import HTTPError
from urllib.request import Request, urlopen


ROOT = Path(__file__).resolve().parents[2]
VERSION = re.compile(r"(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)")
PREFIX = "devtool-v"
ARCHIVE = "services-macos-arm64.tar.gz"
ASSETS = (ARCHIVE, "checksums.txt", "install.sh", "version.json")


def sha256(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def version_tuple(version):
    match = VERSION.fullmatch(version)
    if not match:
        raise ValueError(f"Expected a stable three-part version, got {version!r}")
    parts = tuple(map(int, match.groups()))
    if not any(parts) or any(part > 65535 for part in parts):
        raise ValueError(f"Version is outside Chrome's supported range: {version}")
    return parts


def release_notes(root=ROOT):
    text = (root / "RELEASE_NOTES.md").read_text()
    headings = list(re.finditer(r"^## (.+)$", text, re.MULTILINE))
    if not headings:
        raise ValueError("RELEASE_NOTES.md has no '## X.Y.Z' release")
    previous = None
    for index, heading in enumerate(headings):
        version = heading[1]
        parts = version_tuple(version)
        if previous is not None and parts >= previous:
            raise ValueError("Release versions must be unique and newest first")
        previous = parts
        end = headings[index + 1].start() if index + 1 < len(headings) else len(text)
        notes = text[heading.end():end].strip()
        if not notes:
            raise ValueError(f"Release {version} has no notes")
        if index == 0:
            latest = (version, notes)
    return latest


def repository():
    name = os.environ["GITHUB_REPOSITORY"]
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", name):
        raise ValueError("Invalid GITHUB_REPOSITORY")
    return name


def release_branch():
    branch = os.environ["GITHUB_REF_NAME"]
    if branch not in ("main", "devtool"):
        raise ValueError("Releases must use the main or devtool branch")
    return branch


def api(method, path, body=None, missing_ok=False):
    data = None if body is None else json.dumps(body).encode()
    request = Request(
        f"https://api.github.com/repos/{repository()}/{path}",
        data=data,
        method=method,
        headers={
            "Authorization": f"Bearer {os.environ['GH_TOKEN']}",
            "Accept": "application/vnd.github+json",
            "Content-Type": "application/json",
            "X-GitHub-Api-Version": "2022-11-28",
            "User-Agent": "ServicesDevTools-release",
        },
    )
    try:
        with urlopen(request, timeout=60) as response:
            return json.load(response)
    except HTTPError as error:
        if missing_ok and error.code == 404:
            return None
        raise


def should_publish(version, releases):
    current = version_tuple(version)
    published = []
    for release in releases:
        if release["draft"] or not release["tag_name"].startswith(PREFIX):
            continue
        candidate = version_tuple(release["tag_name"][len(PREFIX):])
        if candidate == current:
            return False
        published.append(candidate)
    if published and current <= max(published):
        raise ValueError("The release-notes version is older than a published release")
    return True


def plan():
    version, _ = release_notes()
    releases = []
    page = 1
    while True:
        batch = api("GET", f"releases?per_page=100&page={page}")
        releases.extend(batch)
        if len(batch) < 100:
            break
        page += 1
    publish = should_publish(version, releases)
    outputs = {"version": version, "tag": PREFIX + version, "publish": str(publish).lower()}
    with open(os.environ["GITHUB_OUTPUT"], "a") as output:
        for key, value in outputs.items():
            output.write(f"{key}={value}\n")
    print(f"{'Build' if publish else 'Already published; skip'}: {PREFIX}{version}")


def prepare(root=ROOT):
    version, _ = release_notes(root)
    cargo = root / "Cargo.toml"
    text = cargo.read_text()
    current = tomllib.loads(text)["workspace"]["package"]["version"]
    section = re.compile(r'(\[workspace\.package\][^\[]*?^version\s*=\s*)"[^"]+"', re.MULTILINE)
    updated, count = section.subn(lambda match: f'{match[1]}"{version}"', text)
    if count != 1:
        raise ValueError("Could not locate workspace.package.version")
    if current != version:
        cargo.write_text(updated)
    manifest_path = root / "adapters/extension/src/manifest.json"
    manifest = json.loads(manifest_path.read_text())
    if manifest["version"] != version:
        manifest["version"] = version
        manifest_path.write_text(json.dumps(manifest, indent=2) + "\n")
    print(f"Prepared application and extension version {version}")


def metadata(root=ROOT):
    version, notes = release_notes(root)
    branch = release_branch()
    output = root / "dist/distribution"
    archive = output / ARCHIVE
    digest = sha256(archive)
    if (output / "checksums.txt").read_text() != f"{digest}  {ARCHIVE}\n":
        raise ValueError("Archive checksum does not match checksums.txt")
    with tarfile.open(archive) as package:
        distribution = json.load(package.extractfile("ServicesDevTools/distribution.json"))
        manifest = json.load(package.extractfile("ServicesDevTools/extension/manifest.json"))
    if distribution["version"] != version or manifest["version"] != version:
        raise ValueError("Packaged application/extension version differs from release notes")
    if distribution["repository"] != repository() or distribution["branch"] != branch:
        raise ValueError("Packaged update source differs from the release repository")
    commit = os.environ["GITHUB_SHA"]
    if not re.fullmatch(r"[0-9a-f]{40}", commit):
        raise ValueError("Invalid GITHUB_SHA")
    base = f"https://github.com/{repository()}/releases/download/{PREFIX}{version}"
    document = {
        "schemaVersion": 1,
        "application": "ServicesDevTools",
        "version": version,
        "tag": PREFIX + version,
        "commit": commit,
        "repository": repository(),
        "branch": branch,
        "platform": "darwin",
        "arch": "arm64",
        "minimumMacOS": "13.5",
        "archive": ARCHIVE,
        "sha256": digest,
        "archiveUrl": f"{base}/{ARCHIVE}",
        "checksumsUrl": f"{base}/checksums.txt",
        "installerUrl": f"{base}/install.sh",
    }
    (output / "version.json").write_text(json.dumps(document, indent=2) + "\n")
    (output / "release-notes.txt").write_text(notes + "\n")


def upload_assets(release_id, output):
    endpoint = f"releases/{release_id}/assets?per_page=100"
    for asset in api("GET", endpoint):
        if asset["name"] in ASSETS:
            subprocess.run(
                ["gh", "api", "--method", "DELETE",
                 f"repos/{repository()}/releases/assets/{asset['id']}", "--silent"],
                check=True,
            )
    for name in ASSETS:
        subprocess.run(
            ["gh", "api", "--method", "POST",
             f"https://uploads.github.com/repos/{repository()}/releases/{release_id}/assets?name={name}",
             "--header", "Content-Type: application/octet-stream",
             "--input", str(output / name), "--silent"],
            check=True,
        )
    uploaded = api("GET", endpoint)
    if len(uploaded) != len(ASSETS) or {asset["name"] for asset in uploaded} != set(ASSETS):
        raise ValueError("Draft release is missing the complete expected asset set")
    for asset in uploaded:
        path = output / asset["name"]
        if (
            asset["state"] != "uploaded"
            or asset["size"] != path.stat().st_size
            or asset.get("digest") not in (None, "sha256:" + sha256(path))
        ):
            raise ValueError(f"Draft release asset does not match the verified file: {asset['name']}")


def publish(root=ROOT):
    version, notes = release_notes(root)
    output = root / "dist/distribution"
    document = json.loads((output / "version.json").read_text())
    commit = os.environ["GITHUB_SHA"]
    tag = PREFIX + version
    if (document["version"], document["commit"], document["repository"]) != (
        version, commit, repository()
    ):
        raise ValueError("Release artifact does not belong to this version, commit and repository")
    digest = sha256(output / ARCHIVE)
    if document["sha256"] != digest or (output / "checksums.txt").read_text() != f"{digest}  {ARCHIVE}\n":
        raise ValueError("Release artifact checksum mismatch")
    release = api("GET", f"releases/tags/{tag}", missing_ok=True)
    if release is not None and not release["draft"]:
        print(f"Already published; leave immutable release {tag} unchanged")
        return
    reference = api("GET", f"git/ref/tags/{tag}", missing_ok=True)
    if reference is not None:
        tagged = api("GET", f"commits/{tag}")
        if tagged["sha"] != commit:
            raise ValueError(f"Tag {tag} already points to another commit")
    if release is None:
        release = api("POST", "releases", {
            "tag_name": tag, "target_commitish": commit,
            "name": f"Services DevTools {version}", "body": notes, "draft": True,
        })
    elif release["target_commitish"] != commit:
        raise ValueError("An unfinished release belongs to another commit; use a new version")
    upload_assets(release["id"], output)
    published = api("PATCH", f"releases/{release['id']}", {
        "body": notes, "draft": False, "prerelease": False, "make_latest": "true",
    })
    print(f"Published {published['html_url']}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("command", choices=("plan", "prepare", "metadata", "publish"))
    arguments = parser.parse_args()
    {"plan": plan, "prepare": prepare, "metadata": metadata, "publish": publish}[arguments.command]()
