#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 lithia Authors
# SPDX-License-Identifier: MPL-2.0

"""Outlines of the repository's source folders: signatures, doc comments and line numbers, no bodies.

Each folder's outline is cached under target/outline/ with a hash of its sources, so a stale outline
is found and rebuilt without reading the code. Paths are relative to the repository root.

  outline.py                       index of every source folder: files, lines, what it is for
  outline.py show PATH... [--pub]  outline of folders or files; --pub keeps `pub` items, the API
  outline.py status                folders whose sources changed since their outline was built
  outline.py update                rebuild stale outlines
  outline.py hash PATH...          each folder's current hash, to compare with one noted earlier

Covers Rust, Python and TLA+. Numbers are 1-based source lines, usable as Read offsets.
"""

from __future__ import annotations

import ast
import hashlib
import json
import re
import subprocess
import sys
from dataclasses import dataclass, field
from pathlib import Path

EXTENSIONS: frozenset[str] = frozenset({".rs", ".py", ".tla"})
ROOT: Path = Path(__file__).resolve().parents[2]
CACHE: Path = ROOT / "target" / "outline"
INDEX: Path = CACHE / "index.json"
VERSION: str = hashlib.sha256(Path(__file__).read_bytes()).hexdigest()[:8]  # a new tool rebuilds every outline
Index = dict[str, dict[str, str | int]]

# Attributes an outline keeps; the rest (inline, allow, expect, ...) are noise.
KEPT_ATTRIBUTES: tuple[str, ...] = (
    "cfg", "derive", "test", "must_use", "non_exhaustive", "doc(hidden)", "repr", "proc_macro",
    "track_caller", "deprecated", "should_panic", "ignore", "lithia::", "tokio::test",
)

# ---------------------------------------------------------------------------------------------- Rust


@dataclass
class Token:
    kind: str  # ident, punct, lit, life, doc, idoc, comment
    text: str
    line: int
    end_line: int


def lex_rust(src: str) -> list[Token]:
    """Splits Rust source into tokens; a string, char, lifetime or comment is one token."""
    tokens: list[Token] = []
    i: int = 0
    line: int = 1
    n: int = len(src)

    def push(kind: str, end: int) -> int:
        text: str = src[i:end]
        tokens.append(Token(kind, text, line, line + text.count("\n")))
        return end

    while i < n:
        c: str = src[i]
        j: int
        if c == "\n":
            line += 1
            i += 1
            continue
        if c.isspace():
            i += 1
            continue
        if src.startswith("//", i):
            j = src.find("\n", i)
            j = n if j < 0 else j
            text: str = src[i:j]
            doc: bool = text.startswith("///") and not text.startswith("////")
            j = push("doc" if doc else "idoc" if text.startswith("//!") else "comment", j)
        elif src.startswith("/*", i):
            depth: int = 0
            j = i
            while j < n:
                if src.startswith("/*", j):
                    depth, j = depth + 1, j + 2
                elif src.startswith("*/", j):
                    depth, j = depth - 1, j + 2
                    if depth == 0:
                        break
                else:
                    j += 1
            text = src[i:j]
            doc = text.startswith("/**") and not text.startswith("/***") and text != "/**/"
            j = push("doc" if doc else "idoc" if text.startswith("/*!") else "comment", j)
        elif c == '"':
            j = push("lit", string_end(src, i + 1))
        elif c == "'":
            if src.startswith("\\", i + 1):
                j = push("lit", char_end(src, i))
            elif i + 2 < n and src[i + 2] == "'":
                j = push("lit", i + 3)
            else:
                j = i + 1
                while j < n and (src[j].isalnum() or src[j] == "_"):
                    j += 1
                j = push("life", j)
        elif c.isalnum() or c == "_":
            j = i
            while j < n and (src[j].isalnum() or src[j] == "_"):
                j += 1
            word: str = src[i:j]
            hashes: int = j
            while hashes < n and src[hashes] == "#":
                hashes += 1
            if word in ("r", "br", "cr") and hashes < n and src[hashes] == '"':
                close: str = '"' + "#" * (hashes - j)
                e: int = src.find(close, hashes + 1)
                j = push("lit", n if e < 0 else e + len(close))
            elif word == "r" and hashes == j + 1:  # a raw identifier, r#name
                j = hashes
                while j < n and (src[j].isalnum() or src[j] == "_"):
                    j += 1
                j = push("ident", j)
            elif word in ("b", "c") and j < n and src[j] == '"':
                j = push("lit", string_end(src, j + 1))
            elif word == "b" and j < n and src[j] == "'":
                j = push("lit", char_end(src, j) if src.startswith("\\", j + 1) else j + 3)
            else:
                j = push("ident", j)
        else:
            j = push("punct", i + 1)
        line += src.count("\n", i, j)
        i = j
    return tokens


def string_end(src: str, i: int) -> int:
    """The offset past the quote that closes a string whose body starts at `i`."""
    while i < len(src):
        if src[i] == "\\":
            i += 2
        elif src[i] == '"':
            return i + 1
        else:
            i += 1
    return i


def char_end(src: str, i: int) -> int:
    """The offset past the quote that closes an escaped char literal whose opening quote is at `i`."""
    j: int = i + 3  # past the quote, the backslash and the escaped character
    while j < len(src) and src[j] != "'":
        j += 1
    return j + 1


QUALIFIERS: frozenset[str] = frozenset({"pub", "async", "unsafe", "extern", "default", "auto", "safe", "const"})
KEYWORDS: frozenset[str] = frozenset({"fn", "trait", "impl", "mod", "struct", "enum", "union", "type", "const", "static", "use"})
OPEN: dict[str, str] = {"(": ")", "[": "]", "{": "}"}
JOINED: frozenset[str] = frozenset({"::", "->", "=>", "&&", "||", ".."})
NO_SPACE_AFTER: frozenset[str] = frozenset({"(", "[", "&", "#", "!", ".", "::", "<", "..", "$"})
NO_SPACE_BEFORE: frozenset[str] = frozenset({")", "]", ",", ";", ":", ".", "::", ">", "?"})


@dataclass
class Rust:
    src_lines: list[str]
    toks: list[Token]
    public: bool
    out: list[str] = field(default_factory=list)

    def group_end(self, i: int) -> int:
        """Index past the group whose opening bracket is at `i`."""
        stack: list[str] = []
        while i < len(self.toks):
            t: Token = self.toks[i]
            if t.kind == "punct":
                if t.text in OPEN:
                    stack.append(OPEN[t.text])
                elif stack and t.text == stack[-1]:
                    stack.pop()
                    if not stack:
                        return i + 1
            i += 1
        return i

    def scan(self, i: int, end: int, stops: str, angles: bool, braces: bool) -> int:
        """Index of the first token in `stops` outside brackets (and generics when `angles`).

        A `{` outside brackets is a stop unless `braces`, which makes it a group to step over."""
        depth: int = 0
        angle: int = 0
        while i < end:
            t: Token = self.toks[i]
            p: str = t.text if t.kind == "punct" else ""
            if p in ("(", "[") or (p == "{" and (depth or angle or braces)):
                depth += 1
            elif p in (")", "]") or (p == "}" and depth):
                depth -= 1
            elif angles and p == "<":
                angle += 1
            elif angles and p == ">" and angle and self.toks[i - 1].text not in ("-", "="):
                angle -= 1
            elif p and p in stops and not depth and not angle:
                return i
            i += 1
        return end

    def flat(self, i: int, end: int) -> str:
        """Tokens `i..end` on one line, comments dropped, spaced as rustfmt would."""
        words: list[str] = []
        k: int = i
        while k < end:
            t: Token = self.toks[k]
            k += 1
            if t.kind in ("comment", "doc", "idoc"):
                continue
            pair: str = t.text + self.toks[k].text if k < end else ""
            if t.kind == "punct" and pair in JOINED and self.toks[k].kind == "punct" and self.toks[k].line == t.line:
                words.append(pair)
                k += 1
            else:
                words.append(t.text)
        text: str = ""
        prev: str = ""
        for w in words:
            tight: bool = (
                not prev
                or prev in NO_SPACE_AFTER
                or (w in NO_SPACE_BEFORE and not (w == ">" and prev == "->"))
                or (w in ("(", "<") and (prev[-1].isalnum() or prev[-1] in "_>!"))
                or (w == "[" and prev in ("#", "!"))
                or (prev == ">" and w in ("(", "::"))
            )
            if prev == ":" or w in ("->", "=>", "==", "=", "+") or prev in ("->", "=>", "==", "=", "+", "{", ","):
                tight = not prev
            if w == "!" and prev and (prev[-1].isalnum() or prev[-1] == "_"):
                tight = True
            text += w if tight else " " + w
            prev = w
        text = re.sub(r",\s*(?=[)\]}>])", "", text).rstrip(",")
        if re.match(r"(pub(\([^)]*\))? )?use ", text):
            text = text.replace("{ ", "{").replace(" }", "}")
        return text

    def emit(self, depth: int, line: int, text: str) -> None:
        self.out.append(f"{line:>5} {'  ' * depth}{text}" if line else f"{'':>5} {'  ' * depth}{text}")

    def items(self, i: int, end: int, depth: int, shown: bool) -> None:
        """Outlines the items in tokens `i..end`; `shown` shows all, as inside a public trait."""
        notes: list[Token] = []
        attrs: list[tuple[int, str]] = []
        last: int = 0  # the line the previous item ended on
        while i < end:
            t: Token = self.toks[i]
            if t.kind in ("doc", "comment"):
                if t.kind == "comment" and t.line == last:
                    i += 1  # trailing a previous item
                    continue
                if notes and t.line > notes[-1].end_line + 1:
                    notes = [n for n in notes if n.kind == "doc"]
                notes.append(t)
                i += 1
                continue
            if t.kind == "idoc":
                for k, piece in enumerate(t.text.splitlines()):
                    self.emit(depth, t.line + k, piece.strip())
                i += 1
                continue
            if t.text == "#" and i + 1 < end:
                inner: bool = self.toks[i + 1].text == "!"
                close: int = self.group_end(i + 2 if inner else i + 1)
                text: str = self.flat(i, close)
                body: str = text[3:-1] if inner else text[2:-1]
                if inner:
                    self.emit(depth, t.line, text)
                elif body.startswith(KEPT_ATTRIBUTES):
                    attrs.append((t.line, text))
                i = close
                continue
            if notes and notes[-1].end_line < t.line - 1:
                notes = [n for n in notes if n.kind == "doc"]
            i = self.item(i, end, depth, shown, notes, attrs)
            last = self.toks[i - 1].end_line if i > 0 else 0
            notes, attrs = [], []

    def item(self, i: int, end: int, depth: int, shown: bool, notes: list[Token], attrs: list[tuple[int, str]]) -> int:
        start: int = i
        line: int = self.toks[i].line
        restricted: bool = i + 1 < end and self.toks[i + 1].text == "("
        public: bool = shown or (self.toks[i].text == "pub" and not restricted)
        j: int = i
        while j < end and self.toks[j].text in QUALIFIERS:
            word: str = self.toks[j].text
            if word == "const" and j + 1 < end and self.toks[j + 1].text not in ("fn", "unsafe", "async", "extern"):
                break
            j += 1
            if word == "pub" and j < end and self.toks[j].text == "(":
                j = self.group_end(j)
            elif word == "extern" and j < end and self.toks[j].kind == "lit":
                j += 1
        kw: str = self.toks[j].text if j < end else ""
        visible: bool = not self.public or public
        if kw in KEYWORDS:
            h: int
            if kw in ("const", "static"):
                h = self.scan(j, end, "=;", angles=True, braces=False)
            elif kw in ("use", "type"):
                h = self.scan(j, end, ";", angles=kw == "type", braces=True)
            else:
                h = self.scan(j, end, "{;", angles=True, braces=False)
            head: str = self.flat(start, h)
            after: int = h + 1
            opens: bool = h < end and self.toks[h].text == "{"
            if opens:
                after = self.group_end(h)
            elif h < end and self.toks[h].text == "=":
                after = self.scan(h, end, ";", angles=False, braces=True) + 1
            if kw == "use" and not public:
                return after
            if kw == "impl":
                visible = True
            if not visible:
                return after
            mark: int = len(self.out)
            self.lead(depth, notes, attrs)
            if kw in ("trait", "impl", "mod") and opens:
                self.emit(depth, line, head)
                before: int = len(self.out)
                self.items(h + 1, after - 1, depth + 1, kw == "trait" and public)
                if self.public and kw != "trait" and len(self.out) == before and " for " not in head:
                    del self.out[mark:]
            elif kw in ("struct", "enum", "union") and opens:
                first: int = self.toks[h].line
                last: int = self.toks[after - 1].line
                if first == last:
                    self.emit(depth, line, f"{head} {self.flat(h, after)}")
                else:
                    self.emit(depth, line, head + " {")
                    for n in range(first + 1, last):
                        member: str = self.src_lines[n - 1].strip()
                        if member:
                            self.emit(depth + 1, n, member)
                    self.emit(depth, last, "}")
            else:
                self.emit(depth, line, head if len(head) <= 200 else head[:197] + "...")
            return after
        if kw == "macro_rules" and j + 3 < end:
            k: int = self.group_end(j + 3)
            if k < end and self.toks[k].text == ";":
                k += 1
            if not self.public or notes:
                self.lead(depth, notes, attrs)
                self.emit(depth, line, f"macro_rules! {self.toks[j + 2].text}")
            return k
        k = j
        while k + 1 < end and (self.toks[k].kind == "ident" or self.toks[k].text == ":"):
            k += 1
        if k > j and self.toks[k].text == "!" and k + 1 < end and self.toks[k + 1].text in OPEN:
            close: int = self.group_end(k + 1)
            call: str = self.flat(start, close)
            if len(call) > 100:
                call = self.flat(start, k + 2) + " … " + OPEN[self.toks[k + 1].text]
            if not self.public:
                self.lead(depth, notes, attrs)
                self.emit(depth, line, call)
            return close + 1 if close < end and self.toks[close].text == ";" else close
        return i + 1

    def lead(self, depth: int, notes: list[Token], attrs: list[tuple[int, str]]) -> None:
        for note in notes:
            for k, text in enumerate(note.text.splitlines()):
                self.emit(depth, note.line + k, text.strip())
        if attrs:
            self.emit(depth, attrs[0][0], " ".join(a for _, a in attrs))


def outline_rust(src: str, public: bool) -> list[str]:
    rust: Rust = Rust(src.splitlines(), lex_rust(src), public)
    rust.items(0, len(rust.toks), 0, False)
    return rust.out


# -------------------------------------------------------------------------------------------- Python


def outline_python(src: str, public: bool) -> list[str]:
    out: list[str] = []
    try:
        tree: ast.Module = ast.parse(src)
    except SyntaxError as error:
        return [f"    ? not parsed: {error.msg} at line {error.lineno}"]
    doc: str | None = ast.get_docstring(tree)
    if doc:
        out.append(f"{1:>5} \"\"\"{doc.splitlines()[0]}\"\"\"")

    def walk(body: list[ast.stmt], depth: int) -> None:
        pad: str = "  " * depth
        for node in body:
            if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
                if public and node.name.startswith("_") and not node.name.endswith("__"):
                    continue
                for dec in node.decorator_list:
                    out.append(f"{dec.lineno:>5} {pad}@{ast.unparse(dec)}")
                if isinstance(node, ast.ClassDef):
                    bases: str = ", ".join(ast.unparse(b) for b in node.bases)
                    out.append(f"{node.lineno:>5} {pad}class {node.name}" + (f"({bases})" if bases else ""))
                else:
                    prefix: str = "async def" if isinstance(node, ast.AsyncFunctionDef) else "def"
                    ret: str = f" -> {ast.unparse(node.returns)}" if node.returns else ""
                    out.append(f"{node.lineno:>5} {pad}{prefix} {node.name}({ast.unparse(node.args)}){ret}")
                inner: str | None = ast.get_docstring(node)
                if inner:
                    out.append(f"{'':>5} {pad}  \"\"\"{inner.splitlines()[0]}\"\"\"")
                if isinstance(node, ast.ClassDef):
                    walk(node.body, depth + 1)
            elif isinstance(node, ast.AnnAssign) and isinstance(node.target, ast.Name):
                if not (public and node.target.id.startswith("_")):
                    out.append(f"{node.lineno:>5} {pad}{node.target.id}: {ast.unparse(node.annotation)}")

    walk(tree.body, 0)
    return out


# --------------------------------------------------------------------------------------------- TLA+

TLA_DEF: re.Pattern[str] = re.compile(r"^(LOCAL\s+)?[A-Za-z_][A-Za-z0-9_]*\s*(\(.*?\)|\[.*?\])?\s*==")
TLA_DECL: re.Pattern[str] = re.compile(r"^(EXTENDS|CONSTANTS?|VARIABLES?|ASSUME|THEOREM|INSTANCE|-{4,}\s*MODULE)\b")
TLA_BORDER: re.Pattern[str] = re.compile(r"^\(\*{10,}\)$")


def outline_tla(src: str, public: bool) -> list[str]:
    out: list[str] = []
    notes: list[str] = []
    block: bool = False  # inside a (*****) bordered comment block
    listing: bool = False  # inside a CONSTANTS or VARIABLES list
    for n, raw in enumerate(src.splitlines(), 1):
        text: str = raw.rstrip()
        stripped: str = text.strip()
        if TLA_BORDER.match(stripped):
            block = not block
            if block:
                notes = [f"{n:>5} {text}"]
            else:
                out += notes + [f"{n:>5} {text}"]
                notes = []
            continue
        if block:
            notes.append(f"{n:>5} {text}")
            continue
        if listing and stripped and raw[:1].isspace():
            out.append(f"{n:>5} {text}")
            continue
        listing = False
        if stripped.startswith("\\*"):
            if "SPDX-" in stripped or raw[:1].isspace():
                notes = []  # a licence line, or a comment inside a definition
            else:
                notes.append(f"{n:>5} {text}")
        elif not stripped:
            notes = []
        elif TLA_DECL.match(text):
            out += notes + [f"{n:>5} {text}"]
            notes = []
            listing = text.startswith(("CONSTANT", "VARIABLE"))
        elif TLA_DEF.match(text):
            out += notes + [f"{n:>5} {text if len(text) <= 140 else text[:137] + '...'}"]
            notes = []
        else:
            notes = []
    return out


# --------------------------------------------------------------------------------------------- Cache


def folders() -> dict[str, list[Path]]:
    """Each folder holding sources git tracks or would track, with its sources."""
    listed: str = subprocess.run(
        ["git", "ls-files", "--cached", "--others", "--exclude-standard"],
        cwd=ROOT, capture_output=True, text=True, check=True,
    ).stdout
    found: dict[str, list[Path]] = {}
    for name in sorted(set(listed.splitlines())):
        path: Path = Path(name)
        if path.suffix in EXTENSIONS and (ROOT / path).is_file():
            found.setdefault(path.parent.as_posix(), []).append(path)
    return found


def folder_hash(files: list[Path]) -> str:
    digest = hashlib.sha256(VERSION.encode())
    for path in files:
        digest.update(path.name.encode() + b"\0" + hashlib.sha256((ROOT / path).read_bytes()).digest())
    return digest.hexdigest()[:16]


def outline_file(path: Path, public: bool) -> list[str]:
    src: str = (ROOT / path).read_text(encoding="utf-8", errors="replace")
    if path.suffix == ".rs":
        return outline_rust(src, public)
    if path.suffix == ".py":
        return outline_python(src, public)
    return outline_tla(src, public)


# What folders without a module doc of their own are for.
NOTES: dict[str, str] = {
    ".claude/tools": "Harness tools: these outlines and the session-start summary.",
    ".github/scripts": "Scripts for CI and development: checks, TLC, kept and cargo-mutants mutations, stress, site.",
    "crates/validation/tests/pass": "Programs `node!` must accept (trybuild).",
    "crates/validation/tests/ui": "Programs `node!` must reject, with their expected errors (trybuild, Case AH).",
    "docs/tla": "TLA+ specs of the scheduling protocol and of node lifetimes; README.md says what each checks.",
    "docs/tla/models": "TLC models: one configuration each; counts.tsv records their state counts.",
}


def summary(folder: str, files: list[Path]) -> str:
    """The first line of the folder's module doc, if it has one."""
    if folder in NOTES:
        return NOTES[folder]
    candidates: list[Path] = [ROOT / folder / name for name in ("mod.rs", "lib.rs", "main.rs")]
    candidates += [ROOT / (folder + ".rs")] + [ROOT / f for f in files]
    for path in candidates:
        if not path.is_file():
            continue
        text: str = path.read_text(encoding="utf-8", errors="replace")
        found: str | None = None
        if path.suffix == ".rs":
            m: re.Match[str] | None = re.search(r"^//!\s?(.*)$", text, re.M)
            found = m.group(1) if m else None
        elif path.suffix == ".py":
            try:
                doc: str | None = ast.get_docstring(ast.parse(text))
            except SyntaxError:
                doc = None
            found = doc.splitlines()[0] if doc else None
        else:
            m = re.search(r"^\(\*\s+([^*\s].*?)\s*\*\)$", text, re.M)
            found = m.group(1) if m else None
        if found:
            return found
    return ""


def render(files: list[Path], public: bool) -> str:
    parts: list[str] = []
    for path in files:
        lines: int = (ROOT / path).read_bytes().count(b"\n")
        parts.append(f"## {path.as_posix()} ({lines} lines)")
        parts += outline_file(path, public)
    return "\n".join(parts) + "\n"


def cached(folder: str) -> Path:
    return CACHE / "tree" / folder / "outline.txt"


def load_index() -> Index:
    try:
        data: Index = json.loads(INDEX.read_text())
    except (OSError, ValueError):
        return {}
    return data


def update(only: set[str] | None = None) -> tuple[Index, list[str]]:
    """Rebuilds the stale outlines (of all folders, or those named); returns the index and what it rebuilt."""
    index: Index = load_index()
    current: dict[str, list[Path]] = folders()
    rebuilt: list[str] = []
    for folder, files in current.items():
        if only is not None and folder not in only:
            continue
        h: str = folder_hash(files)
        if index.get(folder, {}).get("hash") == h and cached(folder).is_file():
            continue
        cached(folder).parent.mkdir(parents=True, exist_ok=True)
        cached(folder).write_text(render(files, False))
        lines: int = sum((ROOT / p).read_bytes().count(b"\n") for p in files)
        index[folder] = {"hash": h, "files": len(files), "lines": lines, "summary": summary(folder, files)}
        rebuilt.append(folder)
    gone: list[str] = [f for f in index if f not in current]
    for folder in gone:
        del index[folder]
    if rebuilt or gone:
        CACHE.mkdir(parents=True, exist_ok=True)
        INDEX.write_text(json.dumps(index, indent=1, sort_keys=True))
    return index, rebuilt


def relative(arg: str) -> str:
    path: Path = Path(arg)
    if path.is_absolute():
        path = path.resolve().relative_to(ROOT)
    text: str = path.as_posix().rstrip("/")
    return "." if text in ("", ".") else text.removeprefix("./")


def main(argv: list[str]) -> int:
    public: bool = "--pub" in argv
    args: list[str] = [a for a in argv if a != "--pub"]
    command: str = args[0] if args else "index"
    paths: list[str] = [relative(a) for a in args[1:]]
    if command == "index":
        index: Index = update()[0]
        for folder in sorted(index):
            entry: dict[str, str | int] = index[folder]
            print(f"{folder:<30} {entry['files']:>3} files {entry['lines']:>6} lines  {entry['summary']}")
        return 0
    if command == "status":
        old: Index = load_index()
        changed: list[str] = [f for f, fs in folders().items() if old.get(f, {}).get("hash") != folder_hash(fs)]
        print("\n".join(changed) if changed else "all outlines current")
        return 1 if changed else 0
    if command == "update":
        rebuilt: list[str] = update()[1]
        print(f"rebuilt {len(rebuilt)} outline(s)" + (": " + ", ".join(rebuilt) if rebuilt else ""))
        return 0
    current: dict[str, list[Path]] = folders()
    if command == "hash":
        for p in paths:
            print(f"{p} {folder_hash(current[p]) if p in current else 'has no sources'}")
        return 0
    if command == "show" and paths:
        for p in paths:
            if p in current:
                if public:
                    sys.stdout.write(render(current[p], True))
                else:
                    update({p})
                    sys.stdout.write(cached(p).read_text())
            elif (ROOT / p).is_file() and Path(p).suffix in EXTENSIONS:
                sys.stdout.write(render([Path(p)], public))
            else:
                below: list[str] = sorted(f for f in current if p == "." or f.startswith(p + "/"))
                if not below:
                    print(f"{p}: no sources here", file=sys.stderr)
                    return 2
                print(f"{p} holds no sources itself; folders below it: {', '.join(below)}")
        return 0
    print(__doc__, file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
