from pathlib import Path
import tempfile
import unittest
from urllib.parse import unquote, urlsplit

import sync_wiki as wiki


class WikiTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        (self.root / "docs").mkdir()
        self.destination = self.root / "wiki"
        (self.destination / ".git").mkdir(parents=True)
        (self.root / "docs/README.md").write_text("# Reference\n\n[Language](language.md#types)\n", encoding="utf-8")
        (self.root / "docs/language.md").write_text("# Language\n\n[Home](README.md)\n", encoding="utf-8")

    def render(self, text):
        pages = {"docs/README.md": "Home.md", "docs/language.md": "language.md"}
        return wiki.transform_markdown(text, lambda target, image: wiki.rewrite_target(target, image, pages))

    def test_reference_navigation_and_code_labels(self):
        self.assertEqual(self.render("[`spec`](language.md#types) [home](README.md)"), "[`spec`](language#types) [home](Home)")

    def test_repository_and_external_links(self):
        self.assertEqual(self.render("[spec](https://github.com/Siox-lang/sioxc/blob/main/docs/language.md#types)"), "[spec](language#types)")
        text = "[external](https://example.org/language.md) [local](#here)"
        self.assertEqual(self.render(text), text)

    def test_non_mirrored_files_directories_and_images(self):
        self.assertIn("https://github.com/Siox-lang/sioxc/blob/main/src/lib.rs", self.render("[code](../src/lib.rs)"))
        self.assertIn("https://github.com/Siox-lang/sioxc/tree/main/docs/proposals", self.render("[proposals](proposals/)"))
        self.assertEqual(self.render("![picture](figures/a.svg)"), "![picture](https://raw.githubusercontent.com/Siox-lang/sioxc/main/docs/figures/a.svg)")

    def test_examples_and_mermaid_are_not_rewritten(self):
        text = "```siox\nunsigned[4](300); [spec](language.md)\n```\n~~~mermaid\nA --> B[spec](language.md)\n~~~\n    [code](language.md)\n`[inline](language.md)`\n``code\n[more](language.md)``\n"
        self.assertEqual(self.render(text), text)

    def test_long_fences_allow_shorter_nested_fences(self):
        text = "````markdown\n```siox\n[code](language.md)\n```\n````\n[spec](language.md)\n"
        self.assertEqual(self.render(text), text.replace("[spec](language.md)", "[spec](language)"))

    def test_sync_is_idempotent_and_check_does_not_write(self):
        (self.destination / "Home.md").write_text("Welcome to the sioxc wiki!\n", encoding="utf-8")
        self.assertEqual(len(wiki.synchronize(self.root, self.destination)), 3)
        self.assertEqual(wiki.synchronize(self.root, self.destination, check=True), [])
        path = self.destination / "language.md"
        path.write_text(path.read_text(encoding="utf-8") + "manual drift\n", encoding="utf-8")
        self.assertEqual(wiki.synchronize(self.root, self.destination, check=True), ["language.md"])
        self.assertTrue(path.read_text(encoding="utf-8").endswith("manual drift\n"))
        self.assertEqual(wiki.synchronize(self.root, self.destination), ["language.md"])

    def test_unmanaged_collisions_fail_before_any_write(self):
        path = self.destination / "language.md"
        path.write_text("Manually maintained\n", encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "manually maintained"):
            wiki.synchronize(self.root, self.destination)
        self.assertFalse((self.destination / "Home.md").exists())
        self.assertEqual(path.read_text(encoding="utf-8"), "Manually maintained\n")

    def test_only_obsolete_generated_pages_are_removed(self):
        (self.destination / "old.md").write_text(wiki.MARKER + "old.md -->\n", encoding="utf-8")
        (self.destination / "Extra.md").write_text("Keep me\n", encoding="utf-8")
        self.assertIn("old.md", wiki.synchronize(self.root, self.destination))
        self.assertFalse((self.destination / "old.md").exists())
        self.assertEqual((self.destination / "Extra.md").read_text(encoding="utf-8"), "Keep me\n")

    def test_destination_must_be_git_checkout(self):
        with self.assertRaisesRegex(ValueError, "Git checkout"):
            wiki.synchronize(self.root, self.root)

    def test_reference_links_resolve_in_repository(self):
        root = Path(wiki.__file__).resolve().parents[1]
        broken = []
        for source in (root / "docs").glob("*.md"):
            def check(target, _image):
                parsed = urlsplit(target)
                if not parsed.scheme and not parsed.netloc and parsed.path and not parsed.path.startswith("/"):
                    if not (source.parent / unquote(parsed.path)).exists():
                        broken.append(f"{source.name}: {target}")
                return target
            wiki.transform_markdown(source.read_text(encoding="utf-8"), check)
        self.assertEqual(broken, [])


if __name__ == "__main__":
    unittest.main()
