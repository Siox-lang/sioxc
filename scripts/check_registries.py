#!/usr/bin/env python3
"""Ratchet string-keyed state tables after resolve; not a Rust type checker."""

import argparse
from dataclasses import dataclass, field
from pathlib import Path
import re
import sys

RAW = re.compile(r'(?:br|cr|r)(#*)"')
CHARACTER = re.compile(r"'(?:\\u\{[0-9a-fA-F_]+\}|\\x[0-9a-fA-F]{2}|\\.|[^'\\\n])'")
TOKEN = re.compile(r"[A-Za-z_][A-Za-z_0-9]*|::|->|.")


@dataclass(frozen=True)
class Token:
    text: str
    line: int
    aliases: dict | None = field(default=None, compare=False, repr=False)


def tokens(source):
    """Ignore comments/literals so embedded Siox/Rust fixtures cannot add fields."""
    result = []
    at, line = 0, 1
    while at < len(source):
        start = at
        if source.startswith("//", at):
            end = source.find("\n", at)
            at = len(source) if end < 0 else end
        elif source.startswith("/*", at):
            at, depth = at + 2, 1
            while at < len(source) and depth:
                if source.startswith("/*", at):
                    at, depth = at + 2, depth + 1
                elif source.startswith("*/", at):
                    at, depth = at + 2, depth - 1
                else:
                    at += 1
        else:
            raw = RAW.match(source, at)
            character = CHARACTER.match(source, at)
            if raw:
                close = '"' + raw[1]
                end = source.find(close, raw.end())
                at = len(source) if end < 0 else end + len(close)
            elif source[at] == '"':
                at += 1
                while at < len(source):
                    if source[at] == "\\":
                        at += 2
                    elif source[at] == '"':
                        at += 1
                        break
                    else:
                        at += 1
            elif character:
                at = character.end()
            elif source[at].isspace():
                at += 1
            else:
                value = TOKEN.match(source, at)[0]
                result.append(Token(value, line))
                at += len(value)
        line += source[start:at].count("\n")
    return result


PAIRS = {"<": ">", "(": ")", "[": "]", "{": "}"}


def end_group(values, start):
    stack = [PAIRS[values[start].text]]
    for at in range(start + 1, len(values)):
        token = values[at].text
        if token in {"(", "[", "{"} or token == "<" and stack[-1] == ">":
            stack.append(PAIRS[token])
        elif token == stack[-1]:
            stack.pop()
            if not stack:
                return at
    raise ValueError(f"unclosed group at line {values[start].line}")


def split(values):
    start, at = 0, 0
    while at < len(values):
        if values[at].text in PAIRS:
            at = end_group(values, at)
        elif values[at].text == ",":
            yield values[start:at]
            start = at + 1
        at += 1
    if start < len(values):
        yield values[start:]


@dataclass(frozen=True)
class Alias:
    parameters: tuple
    body: list
    environment: dict = field(default_factory=dict, compare=False, repr=False)


def applied_alias(values, at, aliases):
    # shortcut: Rust imports and opaque types are not resolved; use compiler-backed analysis when keys require them.
    alias = aliases[values[at].text]
    arguments = []
    if at + 1 < len(values) and values[at + 1].text == "<":
        arguments = list(split(values[at + 2:end_group(values, at + 1)]))
    bindings = {
        name: [Token(token.text, token.line, aliases if token.aliases is None else token.aliases)
               for token in argument]
        for name, argument in zip(alias.parameters, arguments)
    }
    return [part for token in alias.body for part in bindings.get(
        token.text, [Token(token.text, token.line, alias.environment)])]


def key_names(values, aliases, active=frozenset()):
    result = set()
    for at, token in enumerate(values):
        context = aliases if token.aliases is None else token.aliases
        alias = context.get(token.text)
        if alias is not None and id(alias) not in active:
            result.update(key_names(applied_alias(values, at, context), context, active | {id(alias)}))
        else:
            result.add(token.text)
    return result


def has_name_key(values, aliases, active=frozenset()):
    at = 0
    while at < len(values):
        token = values[at]
        name = token.text
        context = aliases if token.aliases is None else token.aliases
        if name in {"HashMap", "HashSet", "BTreeMap", "BTreeSet"} and at + 1 < len(values) and values[at + 1].text == "<":
            end = end_group(values, at + 1)
            arguments = list(split(values[at + 2:end]))
            key = key_names(arguments[0], aliases) if arguments else set()
            if {"String", "str"} & key and "DefId" not in key:
                return True
            # Owner-qualified member maps are not global declaration registries.
            if "DefId" in key:
                at = end + 1
                continue
        alias = context.get(name)
        if alias is not None and id(alias) not in active:
            if has_name_key(applied_alias(values, at, context), context, active | {id(alias)}):
                return True
        at += 1
    return False


@dataclass(frozen=True)
class Finding:
    path: str
    owner: str
    field: str
    line: int

    @property
    def key(self):
        return f"{self.path}:{self.owner}.{self.field}"


def visible_aliases(namespace, aliases):
    visible = {}
    for depth in range(len(namespace) + 1):
        visible.update({name: alias for (scope, name), alias in aliases.items() if scope == namespace[:depth]})
    return visible


def findings(source, path):
    values, aliases, bodies = tokens(source), {}, []
    modules = []
    at = 0
    while at < len(values):
        while modules and at >= modules[-1][1]:
            modules.pop()
        namespace = tuple(name for name, _ in modules)
        word = values[at].text
        if word == "mod" and at + 2 < len(values) and values[at + 1].text == "tests" and values[at + 2].text == "{":
            at = end_group(values, at + 2) + 1
            continue
        if word == "mod" and at + 2 < len(values) and values[at + 2].text == "{":
            modules.append((values[at + 1].text, end_group(values, at + 2)))
        if word == "type" and at + 2 < len(values):
            end = at + 2
            parameters = ()
            if values[end].text == "<":
                close = end_group(values, end)
                parameters = tuple(part[1].text if part[0].text in {"'", "const"} else part[0].text for part in split(values[end + 1:close]))
            while end < len(values) and values[end].text not in {"=", ";", "{"}:
                if values[end].text in PAIRS:
                    end = end_group(values, end)
                end += 1
            if end < len(values) and values[end].text == "=":
                start = end + 1
                end = start
                while end < len(values) and values[end].text != ";":
                    if values[end].text in PAIRS:
                        end = end_group(values, end)
                    end += 1
                aliases[namespace, values[at + 1].text] = Alias(parameters, values[start:end])
        if word in {"struct", "enum"} and at + 2 < len(values):
            name, end = values[at + 1].text, at + 2
            if values[end].text == "<":
                end = end_group(values, end) + 1
            while end < len(values) and values[end].text not in {"{", "(", ";"}:
                end += 1
            if end < len(values) and values[end].text in {"{", "("}:
                close = end_group(values, end)
                bodies.append((namespace, name, word, values[end + 1:close]))
                at = close + 1
                continue
        at += 1
    for (namespace, _), alias in aliases.items():
        alias.environment.update(visible_aliases(namespace, aliases))
    result = []
    for namespace, owner, kind, body in bodies:
        visible = visible_aliases(namespace, aliases)
        owner = "::".join((*namespace, owner))
        if kind == "enum":
            for variant in split(body):
                start = skip_attributes(variant)
                if start >= len(variant):
                    continue
                for index in range(start + 1, len(variant)):
                    token = variant[index]
                    if token.text in {"{", "("}:
                        fields = variant[index + 1:end_group(variant, index)]
                        result.extend(field_findings(fields, path, f"{owner}::{variant[start].text}", visible))
                        break
        else:
            result.extend(field_findings(body, path, owner, visible))
    return result


def skip_attributes(values):
    at = 0
    while at + 1 < len(values) and values[at].text == "#" and values[at + 1].text == "[":
        at = end_group(values, at + 1) + 1
    return at


def field_findings(body, path, owner, aliases):
    for position, field in enumerate(split(body)):
        at = skip_attributes(field)
        if at < len(field) and field[at].text == "pub":
            at += 1
            if at < len(field) and field[at].text == "(":
                at = end_group(field, at) + 1
        if at >= len(field):
            continue
        named = at + 1 < len(field) and field[at + 1].text == ":"
        ty = field[at + 2:] if named else field[at:]
        if has_name_key(ty, aliases):
            yield Finding(path, owner, field[at].text if named else str(position), field[at].line)


def inventory(root):
    paths = [root / "src/elab.rs"]
    for stage in ["types", "elab", "ir"]:
        paths.extend((root / "src" / stage).rglob("*.rs"))
    result = {}
    for path in sorted(paths):
        if "tests" in path.parts or path.name == "tests.rs":
            continue
        relative = path.relative_to(root).as_posix()
        try:
            found = findings(path.read_text(), relative)
        except ValueError as error:
            raise ValueError(f"{relative}: {error}") from error
        for finding in found:
            if finding.key in result:
                raise ValueError(f"ambiguous state-table identity {finding.key}")
            result[finding.key] = finding
    return result


def audit(actual, expected, strict=False):
    errors = [f"unclassified name-keyed state table: {key}" for key in sorted(actual.keys() - expected.keys())]
    errors.extend(f"stale registry inventory entry: {key}" for key in sorted(expected.keys() - actual.keys()))
    if strict:
        errors.extend(f"declaration registry still needs identity migration: {key}" for key, (kind, _) in sorted(expected.items()) if kind == "migration")
    return errors


def read_inventory(path):
    result = {}
    for line in path.read_text().splitlines():
        if not line or line.startswith("#"):
            continue
        key, kind, reason = line.split(" | ", 2)
        if kind not in {"migration", "symbol", "state", "output"} or not reason.strip() or key in result:
            raise ValueError(f"invalid registry inventory entry: {line}")
        result[key] = kind, reason
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--strict", action="store_true", help="also reject existing declaration migration exceptions")
    parser.add_argument("--list", action="store_true", help="print detected state tables without changing the inventory")
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    actual = inventory(root)
    if args.list:
        print("\n".join(sorted(actual)))
        return 0
    expected = read_inventory(root / "scripts/registry_inventory.txt")
    errors = audit(actual, expected, args.strict)
    for error in errors:
        print(error, file=sys.stderr)
    if errors:
        return 1
    pending = sum(kind == "migration" for kind, _ in expected.values())
    print(f"Registry ratchet: {len(actual)} classified state tables; {pending} declaration migrations remain")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
