from contextlib import contextmanager
import hashlib
import io
import json
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest
from unittest.mock import patch
from urllib.error import HTTPError

import release


class ReleaseTests(unittest.TestCase):
    def notes(self, text):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "RELEASE_NOTES.md").write_text(text)
            return release.release_notes(root)

    def test_selects_latest_notes_without_older_contents(self):
        self.assertEqual(
            self.notes("# Releases\n\n## 2.5.22\n\n- New feature\n\n## 2.5.21\n\n- Old\n"),
            ("2.5.22", "- New feature"),
        )
        self.assertEqual(self.notes("## 3.0.0\n\n- Major release\n")[0], "3.0.0")

    def test_rejects_missing_empty_duplicate_unordered_and_invalid_versions(self):
        for text in (
            "# Releases\n", "## 2.5.22\n",
            "## 2.5.22\nOne\n## 2.5.22\nTwo",
            "## 2.5.21\nOld\n## 2.5.22\nNew",
            "## 2.05.22\nInvalid", "## 2.5.65536\nInvalid",
            "## 0.0.0\nInvalid", "## 2.5.22-beta\nInvalid",
        ):
            with self.subTest(text=text), self.assertRaises(ValueError):
                self.notes(text)

    def test_published_versions_are_skipped_drafts_and_other_products_are_not(self):
        self.assertTrue(release.should_publish("2.5.22", []))
        releases = [
            {"tag_name": "other-v99.0.0", "draft": False},
            {"tag_name": "devtool-v2.5.21", "draft": False},
            {"tag_name": "devtool-v2.5.22", "draft": True},
        ]
        self.assertTrue(release.should_publish("2.5.22", releases))
        releases[-1]["draft"] = False
        self.assertFalse(release.should_publish("2.5.22", releases))
        with self.assertRaises(ValueError):
            release.should_publish("2.5.20", releases)

    def test_notes_prepare_both_versions_without_changing_dependencies_or_extension_key(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "RELEASE_NOTES.md").write_text("## 2.5.22\n\n- New feature\n")
            cargo = '[workspace.package]\nversion = "2.5.21"\nedition = "2024"\n\n[workspace.dependencies]\nserde = "1"\n'
            (root / "Cargo.toml").write_text(cargo)
            manifest = root / "adapters/extension/src/manifest.json"
            manifest.parent.mkdir(parents=True)
            manifest.write_text(json.dumps({"version": "2.5.21", "key": "public-key", "permissions": ["nativeMessaging"]}))
            release.prepare(root)
            self.assertEqual((root / "Cargo.toml").read_text(), cargo.replace("2.5.21", "2.5.22"))
            self.assertEqual(json.loads(manifest.read_text()), {
                "version": "2.5.22", "key": "public-key", "permissions": ["nativeMessaging"],
            })
            before = manifest.read_bytes()
            release.prepare(root)
            self.assertEqual(manifest.read_bytes(), before)

    @contextmanager
    def package(self, branch="main"):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "RELEASE_NOTES.md").write_text("## 3.0.0\n\n- Major release\n")
            output = root / "dist/distribution"
            output.mkdir(parents=True)
            archive = output / release.ARCHIVE
            with tarfile.open(archive, "w:gz") as package:
                for name, value in {
                    "ServicesDevTools/distribution.json": {
                        "version": "3.0.0", "repository": "owner/repo", "branch": branch,
                    },
                    "ServicesDevTools/extension/manifest.json": {"version": "3.0.0"},
                }.items():
                    data = json.dumps(value).encode()
                    entry = tarfile.TarInfo(name)
                    entry.size = len(data)
                    package.addfile(entry, io.BytesIO(data))
            digest = hashlib.sha256(archive.read_bytes()).hexdigest()
            (output / "checksums.txt").write_text(f"{digest}  {release.ARCHIVE}\n")
            (output / "install.sh").write_text(
                "#!/bin/bash\nREPOSITORY='owner/repo'\nVERSION='3.0.0'\n"
            )
            with patch.dict(os.environ, {
                "GITHUB_REPOSITORY": "owner/repo", "GITHUB_SHA": "a" * 40,
                "GITHUB_REF_NAME": branch,
            }):
                yield root, output

    def test_metadata_uses_exact_major_version_and_verified_archive(self):
        with self.package() as (root, output):
            release.metadata(root)
            metadata = json.loads((output / "version.json").read_text())
            self.assertEqual(metadata["version"], "3.0.0")
            self.assertEqual(metadata["tag"], "devtool-v3.0.0")
            self.assertEqual(metadata["branch"], "main")
            self.assertEqual(metadata["commit"], "a" * 40)
            self.assertEqual(metadata["sha256"], release.sha256(output / release.ARCHIVE))
            self.assertEqual((output / "release-notes.txt").read_text(), "- Major release\n")
            (output / "install.sh").write_text("#!/bin/bash\nREPOSITORY='owner/repo'\nVERSION='2.6.1'\n")
            with self.assertRaisesRegex(ValueError, "install.sh"):
                release.metadata(root)
            (output / "checksums.txt").write_text("invalid\n")
            with self.assertRaises(ValueError):
                release.metadata(root)

    def test_metadata_preserves_legacy_channel_and_rejects_mismatched_or_unknown_branches(self):
        with self.package("devtool") as (root, output):
            release.metadata(root)
            self.assertEqual(json.loads((output / "version.json").read_text())["branch"], "devtool")
            with patch.dict(os.environ, {"GITHUB_REF_NAME": "main"}):
                with self.assertRaisesRegex(ValueError, "update source"):
                    release.metadata(root)
            with patch.dict(os.environ, {"GITHUB_REF_NAME": "feature"}):
                with self.assertRaisesRegex(ValueError, "main or devtool"):
                    release.metadata(root)

    def test_only_not_found_can_be_treated_as_missing(self):
        with patch.dict(os.environ, {"GITHUB_REPOSITORY": "owner/repo", "GH_TOKEN": "fixture"}):
            for code in (401, 403, 429, 500):
                error = HTTPError("https://api.github.com", code, "fixture", {}, None)
                with patch.object(release, "urlopen", side_effect=error), self.assertRaises(HTTPError):
                    release.api("GET", "releases/tags/devtool-v3.0.0", missing_ok=True)
            error = HTTPError("https://api.github.com", 404, "fixture", {}, None)
            with patch.object(release, "urlopen", side_effect=error):
                self.assertIsNone(release.api("GET", "releases/tags/devtool-v3.0.0", missing_ok=True))

    def test_published_release_never_uploads_or_changes_assets(self):
        with self.package() as (root, _):
            release.metadata(root)
            with patch.object(release, "api", return_value={"draft": False}) as api:
                with patch.object(release.subprocess, "run") as upload:
                    release.publish(root)
                    upload.assert_not_called()
                    self.assertEqual(api.call_count, 1)

    def test_upload_failure_never_publishes_draft(self):
        with self.package() as (root, _):
            release.metadata(root)
            with patch.object(release, "api", side_effect=[None, None, {"id": 42, "draft": True}, []]) as api:
                with patch.object(release.subprocess, "run", side_effect=subprocess.CalledProcessError(1, "gh")):
                    with self.assertRaises(subprocess.CalledProcessError):
                        release.publish(root)
                self.assertEqual([call.args[0] for call in api.call_args_list], ["GET", "GET", "POST", "GET"])

    def assets(self, output):
        return [
            {"name": name, "size": (output / name).stat().st_size,
             "state": "uploaded", "digest": "sha256:" + release.sha256(output / name)}
            for name in release.ASSETS
        ]

    def test_complete_upload_is_followed_by_publication(self):
        with self.package() as (root, output):
            release.metadata(root)
            with patch.object(release, "api", side_effect=[
                None, None, {"id": 42, "draft": True}, [], self.assets(output),
                {"html_url": "https://github.com/owner/repo/releases/tag/devtool-v3.0.0"},
            ]) as api:
                with patch.object(release.subprocess, "run") as upload:
                    release.publish(root)
                self.assertEqual(api.call_args_list[-1].args[0:2], ("PATCH", "releases/42"))
                self.assertFalse(api.call_args_list[-1].args[2]["draft"])
                self.assertEqual(upload.call_count, len(release.ASSETS))
                for call, name in zip(upload.call_args_list, release.ASSETS):
                    command = call.args[0]
                    self.assertEqual(
                        command[4],
                        f"https://uploads.github.com/repos/owner/repo/releases/42/assets?name={name}",
                    )
                    self.assertEqual(command[command.index("--input") + 1], str(output / name))

    def test_incomplete_or_corrupt_upload_never_publishes_draft(self):
        with self.package() as (root, output):
            release.metadata(root)
            valid = self.assets(output)
            for uploaded in (
                [],
                valid[:-1],
                [dict(asset, size=0) for asset in valid],
                [dict(asset, state="new") for asset in valid],
                [dict(asset, digest="sha256:" + "0" * 64) for asset in valid],
            ):
                with self.subTest(uploaded=uploaded):
                    with patch.object(release, "api", side_effect=[
                        None, None, {"id": 42, "draft": True}, [], uploaded,
                    ]) as api:
                        with patch.object(release.subprocess, "run"):
                            with self.assertRaises(ValueError):
                                release.publish(root)
                        self.assertNotIn("PATCH", [call.args[0] for call in api.call_args_list])

    def test_retry_replaces_assets_by_id_in_the_selected_draft(self):
        with self.package() as (root, output):
            release.metadata(root)
            with patch.object(release, "api", side_effect=[
                [{"id": 7, "name": "checksums.txt"}], self.assets(output),
            ]):
                with patch.object(release.subprocess, "run") as upload:
                    release.upload_assets(42, output)
            self.assertEqual(upload.call_count, 1 + len(release.ASSETS))
            self.assertEqual(upload.call_args_list[0].args[0], [
                "gh", "api", "--method", "DELETE",
                "repos/owner/repo/releases/assets/7", "--silent",
            ])


if __name__ == "__main__":
    unittest.main()
