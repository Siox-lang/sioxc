#!/usr/bin/env python3
"""Mirror top-level reference Markdown into a checkout of the GitHub wiki."""

import argparse
from pathlib import Path
import posixpath
import re
import sys
from urllib.parse import unquote, urlsplit, urlunsplit

REPOSITORY = "https://github.com/Siox-lang/sioxc"
MARKER = "<!-- Generated from docs/"
FENCE = re.compile(r"^ {0,3}(`{3,}|~{3,})(.*)$")
CODE = re.compile(r"(?<!`)(`+)(?!`)(.*?)(?<!`)\1(?!`)", re.DOTALL)
LINK = re.compile(r'(?P<prefix>!?\[[^\]]*\]\()(?P<target>[^\s)]+)(?P<tail>(?:\s+"[^"\n]*")?\))')


def transform_markdown(text, rewrite):
    """Change link destinations, leaving fenced/indented/inline code intact."""
    output, prose = [], []
    fence = None

    def flush():
        text = "".join(prose)
        code = [(m.start(), m.end()) for m in CODE.finditer(text)]

        def replace(match):
            if any(start <= match.start() < end for start, end in code):
                return match.group()
            target = rewrite(match["target"], match["prefix"].startswith("!"))
            return match["prefix"] + target + match["tail"]

        output.append(LINK.sub(replace, text))
        prose.clear()

    for line in text.splitlines(keepends=True):
        match = FENCE.match(line)
        if fence:
            output.append(line)
            if match and match[1][0] == fence[0] and len(match[1]) >= fence[1] and not match[2].strip():
                fence = None
        elif match:
            flush()
            fence = (match[1][0], len(match[1]))
            output.append(line)
        elif line.startswith(("    ", "\t")):
            flush()
            output.append(line)
        else:
            prose.append(line)
    flush()
    return "".join(output)


def rewrite_target(target, image, pages):
    parsed = urlsplit(target)
    prefix = REPOSITORY + "/blob/main/"
    if target.startswith(prefix):
        relative = unquote(parsed.path.removeprefix("/Siox-lang/sioxc/blob/main/"))
    elif parsed.scheme or parsed.netloc or not parsed.path or parsed.path.startswith("/"):
        return target
    else:
        relative = posixpath.normpath(posixpath.join("docs", unquote(parsed.path)))
    if relative in pages:
        return urlunsplit(("", "", pages[relative].removesuffix(".md"), parsed.query, parsed.fragment))
    if target.startswith(prefix):
        return target
    if image:
        base = "https://raw.githubusercontent.com/Siox-lang/sioxc/main/"
    else:
        base = REPOSITORY + ("/tree/main/" if parsed.path.endswith("/") else "/blob/main/")
    return urlunsplit((*urlsplit(base + relative)[:3], parsed.query, parsed.fragment))


def render_pages(root):
    sources = sorted((root / "docs").glob("*.md"))
    if not sources:
        raise ValueError("no reference Markdown found in docs/")
    pages = {"docs/" + source.name: "Home.md" if source.name == "README.md" else source.name for source in sources}
    if len({name.casefold() for name in pages.values()}) != len(pages):
        raise ValueError("reference page names collide in the wiki")
    rendered, sidebar = {}, []
    for source in sources:
        text = source.read_text(encoding="utf-8")
        name = pages["docs/" + source.name]
        banner = f"{MARKER}{source.name} by scripts/sync_wiki.py; edit the source Markdown. -->\n\n"
        rendered[name] = banner + transform_markdown(text, lambda target, image: rewrite_target(target, image, pages))
        title = next((line[2:].strip() for line in text.splitlines() if line.startswith("# ")), source.stem)
        sidebar.append(f"- [{title}]({name.removesuffix('.md')})\n")
    rendered["_Sidebar.md"] = MARKER + " by scripts/sync_wiki.py. -->\n\n" + "".join(sidebar)
    return rendered


def synchronize(root, wiki, check=False):
    if not (wiki / ".git").exists():
        raise ValueError("destination must be an existing wiki Git checkout")
    rendered = render_pages(root)
    changed = [name for name, text in rendered.items() if not (wiki / name).exists() or (wiki / name).read_text(encoding="utf-8") != text]
    stale = [path for path in wiki.glob("*.md") if path.name not in rendered and path.read_text(encoding="utf-8").startswith(MARKER)]
    if check:
        return changed + [path.name for path in stale]
    # Validate every collision before modifying any page; unrelated wiki content is never removed.
    for name in changed:
        path = wiki / name
        if path.exists():
            old = path.read_text(encoding="utf-8")
            placeholder = name == "Home.md" and old.strip() == "Welcome to the sioxc wiki!"
            if not old.startswith(MARKER) and not placeholder:
                raise ValueError(f"refusing to overwrite manually maintained wiki page: {name}")
    for name in changed:
        (wiki / name).write_text(rendered[name], encoding="utf-8")
    for path in stale:
        path.unlink()
    return changed + [path.name for path in stale]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="report drift without modifying the checkout")
    parser.add_argument("wiki", type=Path, help="local clone of sioxc.wiki.git")
    args = parser.parse_args()
    try:
        changed = synchronize(Path(__file__).resolve().parents[1], args.wiki, args.check)
    except (OSError, ValueError) as error:
        print(error, file=sys.stderr)
        return 1
    if args.check and changed:
        print("wiki is out of date: " + ", ".join(changed), file=sys.stderr)
        return 1
    print("wiki matches docs/" if args.check else f"updated {len(changed)} wiki page(s)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
