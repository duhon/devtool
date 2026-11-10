import argparse
import base64
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


ROOT = Path(__file__).resolve().parents[3]
VERSION = re.compile(r"(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)")
PREFIX = "devtool-v"
ARCHIVE = "services-macos-arm64.tar.gz"
ASSETS = (ARCHIVE, "checksums.txt", "install.sh", "version.json")
# Modules are released apart from the application: the branch MODULES_BRANCH holds one
# commit with the newest package of each module in MODULES_DIR, which Settings → Modules
# reads through the Standard source.
MODULES_BRANCH = "modules"
MODULES_DIR = "modules"
MODULE_SUFFIX = ".module.tgz"


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
    return newest_notes((root / "RELEASE_NOTES.md").read_text())


def newest_notes(text):
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


def should_publish(version, releases, prefix=PREFIX):
    current = version_tuple(version)
    published = []
    for release in releases:
        if release["draft"] or not release["tag_name"].startswith(prefix):
            continue
        candidate = release["tag_name"][len(prefix):]
        if not VERSION.fullmatch(candidate):
            continue
        candidate = version_tuple(candidate)
        if candidate == current:
            return False
        published.append(candidate)
    if published and current <= max(published):
        raise ValueError(f"The {prefix}{version} release is older than a published release")
    return True


def package_name(name):
    """`(id, version)` of a package file named `<id>-<version>[-<platform>].module.tgz`, else
    None; an id has no dots, so the first part with one is the version."""
    if not name.endswith(MODULE_SUFFIX):
        return None
    parts = name[:-len(MODULE_SUFFIX)].split("-")
    at = next((index for index, part in enumerate(parts) if "." in part), None)
    if not at or not VERSION.fullmatch(parts[at]):
        return None
    return "-".join(parts[:at]), parts[at]


def git_blob_sha(data):
    return hashlib.sha1(b"blob %d\0" % len(data) + data).hexdigest()


def published_modules():
    """`{file name: blob sha}` of the packages in the modules branch; empty before its first
    publication."""
    tree = api("GET", f"git/trees/{MODULES_BRANCH}:{MODULES_DIR}", missing_ok=True)
    if tree is None:
        return {}
    if tree.get("truncated"):
        raise ValueError(f"The {MODULES_BRANCH} branch lists too many files")
    return {entry["path"]: entry["sha"] for entry in tree["tree"] if entry["type"] == "blob"}


def new_modules(versions, published):
    """The `(id, version)` of `versions` (`{id: version}`) that `published` (the file names in
    the modules branch) lacks; a version older than the published one is an error."""
    newest = {}
    for name in published:
        parsed = package_name(name)
        if parsed:
            module, version = parsed
            if module not in newest or version_tuple(version) > version_tuple(newest[module]):
                newest[module] = version
    found = []
    for module, version in versions.items():
        if module in newest:
            if version_tuple(version) == version_tuple(newest[module]):
                continue
            if version_tuple(version) < version_tuple(newest[module]):
                raise ValueError(f"Module {module} {version} is older than the published {newest[module]}")
        found.append((module, version))
    return found


def modules(root=ROOT):
    """`{id: version}` of the module crates in modules/services and modules/panels (the version
    in each Cargo.toml)."""
    found = {}
    cargos = [*(root / "modules/services").glob("*/Cargo.toml"), *(root / "modules/panels").glob("*/Cargo.toml")]
    for cargo in sorted(cargos):
        crate = tomllib.loads(cargo.read_text())
        manifest = cargo.parent / "manifest.json"
        if "cdylib" not in crate.get("lib", {}).get("crate-type", []) or not manifest.is_file():
            continue
        module = json.loads(manifest.read_text())["id"]
        version = crate["package"]["version"]
        if not isinstance(version, str):
            raise ValueError(f"Module {module} needs its own version in {cargo}")
        version_tuple(version)
        found[module] = version
    return found


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
    application = should_publish(version, releases)
    changed = new_modules(modules(), published_modules())
    publish = application or bool(changed)
    outputs = {"version": version, "tag": PREFIX + version, "publish": str(publish).lower()}
    with open(os.environ["GITHUB_OUTPUT"], "a") as output:
        for key, value in outputs.items():
            output.write(f"{key}={value}\n")
    print(f"{'Build' if application else 'Already published; skip'}: {PREFIX}{version}")
    for module, module_version in changed:
        print(f"Build: module {module} {module_version}")


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
    manifest_path = root / "app/extension/src/manifest.json"
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
    if f"REPOSITORY='{repository()}'\nVERSION='{version}'\n" not in (output / "install.sh").read_text():
        raise ValueError("install.sh does not download this release")
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


def upload_assets(release_id, output, names=ASSETS):
    endpoint = f"releases/{release_id}/assets?per_page=100"
    for asset in api("GET", endpoint):
        if asset["name"] in names:
            subprocess.run(
                ["gh", "api", "--method", "DELETE",
                 f"repos/{repository()}/releases/assets/{asset['id']}", "--silent"],
                check=True,
            )
    for name in names:
        subprocess.run(
            ["gh", "api", "--method", "POST",
             f"https://uploads.github.com/repos/{repository()}/releases/{release_id}/assets?name={name}",
             "--header", "Content-Type: application/octet-stream",
             "--input", str(output / name), "--silent"],
            check=True,
        )
    uploaded = api("GET", endpoint)
    if len(uploaded) != len(names) or {asset["name"] for asset in uploaded} != set(names):
        raise ValueError("Draft release is missing the complete expected asset set")
    for asset in uploaded:
        path = output / asset["name"]
        if (
            asset["state"] != "uploaded"
            or asset["size"] != path.stat().st_size
            or asset.get("digest") not in (None, "sha256:" + sha256(path))
        ):
            raise ValueError(f"Draft release asset does not match the verified file: {asset['name']}")


def release_once(tag, name, notes, output, names, latest):
    """Publishes release `tag` of this commit with the files `names` from `output`; a published
    release stays unchanged, an unfinished draft of this commit is completed."""
    commit = os.environ["GITHUB_SHA"]
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
            "name": name, "body": notes, "draft": True,
        })
    elif release["target_commitish"] != commit:
        raise ValueError("An unfinished release belongs to another commit; use a new version")
    upload_assets(release["id"], output, names)
    published = api("PATCH", f"releases/{release['id']}", {
        "body": notes, "draft": False, "prerelease": False,
        "make_latest": "true" if latest else "false",
    })
    print(f"Published {published['html_url']}")


def module_package(archive):
    """`(id, version)` of a module package file, whose notes must start with its version and
    whose name must be the one its package.json gives."""
    with tarfile.open(archive) as package:
        described = json.load(package.extractfile("package.json"))
        try:
            text = package.extractfile("RELEASE_NOTES.md").read().decode()
        except KeyError:
            raise ValueError(f"{archive.name} has no RELEASE_NOTES.md") from None
    module, version = described["id"], described["version"]
    noted, _ = newest_notes(text)
    if noted != version:
        raise ValueError(f"{archive.name}: the newest module notes are for {noted}, not {version}")
    platform = f"-{described['platform']}" if described.get("platform") else ""
    if archive.name != f"{module}-{version}{platform}{MODULE_SUFFIX}":
        raise ValueError(f"{archive.name} is not named by its package")
    return module, version


def publish_modules(root=ROOT):
    """Points the modules branch at a new commit without history holding the newest package of
    each built module: a version already there keeps its published file, a new one is
    uploaded, older versions are dropped. Nothing changes when every version is there."""
    archives = sorted((root / "dist/modules").glob(f"*{MODULE_SUFFIX}"))
    if not archives:
        print(f"The build has no module packages; leave the {MODULES_BRANCH} branch unchanged")
        return
    published = published_modules()
    by_module = {}
    for name, sha in published.items():
        parsed = package_name(name)
        if parsed:
            by_module[parsed[0]] = (parsed[1], name, sha)
    files = {}
    changed = False
    for archive in archives:
        module, version = module_package(archive)
        if module in files:
            raise ValueError(f"Two packages of module {module}")
        known = by_module.get(module)
        if known and version_tuple(version) == version_tuple(known[0]):
            files[module] = (known[1], known[2])
            continue
        if known and version_tuple(version) < version_tuple(known[0]):
            raise ValueError(f"Module {module} {version} is older than the published {known[0]}")
        data = archive.read_bytes()
        blob = api("POST", "git/blobs", {
            "content": base64.b64encode(data).decode(), "encoding": "base64",
        })
        if blob["sha"] != git_blob_sha(data):
            raise ValueError(f"GitHub stored {archive.name} differently")
        files[module] = (archive.name, blob["sha"])
        changed = True
        print(f"Publish module {module} {version}")
    if not changed and set(published) == {name for name, _ in files.values()}:
        print(f"Already published; leave the {MODULES_BRANCH} branch unchanged")
        return
    tree = api("POST", "git/trees", {"tree": [
        {"path": f"{MODULES_DIR}/{name}", "mode": "100644", "type": "blob", "sha": sha}
        for name, sha in sorted(files.values())
    ]})
    commit = api("POST", "git/commits", {
        "message": f"Modules of {os.environ['GITHUB_SHA']}",
        "tree": tree["sha"],
        "parents": [],
    })
    reference = f"heads/{MODULES_BRANCH}"
    if api("GET", f"git/ref/{reference}", missing_ok=True) is None:
        api("POST", "git/refs", {"ref": f"refs/{reference}", "sha": commit["sha"]})
    else:
        api("PATCH", f"git/refs/{reference}", {"sha": commit["sha"], "force": True})
    print(f"Published the {MODULES_BRANCH} branch: {commit['sha']}")


def publish(root=ROOT):
    version, notes = release_notes(root)
    output = root / "dist/distribution"
    document = json.loads((output / "version.json").read_text())
    commit = os.environ["GITHUB_SHA"]
    if (document["version"], document["commit"], document["repository"]) != (
        version, commit, repository()
    ):
        raise ValueError("Release artifact does not belong to this version, commit and repository")
    digest = sha256(output / ARCHIVE)
    if document["sha256"] != digest or (output / "checksums.txt").read_text() != f"{digest}  {ARCHIVE}\n":
        raise ValueError("Release artifact checksum mismatch")
    release_once(PREFIX + version, f"Services DevTools {version}", notes, output, ASSETS, True)
    publish_modules(root)


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("command", choices=("plan", "prepare", "metadata", "publish"))
    arguments = parser.parse_args()
    {"plan": plan, "prepare": prepare, "metadata": metadata, "publish": publish}[arguments.command]()
