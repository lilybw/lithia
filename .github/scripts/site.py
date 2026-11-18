# SPDX-FileCopyrightText: 2026 lithia Authors
# SPDX-License-Identifier: GPL-2.0-or-later

"""Builds lithia's docs site: one version's snapshot (API and spec), or the whole site from releases.

`site.py snapshot --version 0.3.1.3 --out DIR` writes DIR/api and DIR/spec for one release.
`site.py assemble --out DIR` gathers every release's snapshot under DIR/versions and the newest at DIR/api, DIR/spec.
"""

from __future__ import annotations

import argparse
import functools
import html
import json
import os
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import tomllib
from dataclasses import dataclass
from pathlib import Path
from typing import cast

ROOT: Path = Path(__file__).resolve().parents[2]
SITE: Path = ROOT / "docs" / "site"
SPEC_DIR: Path = ROOT / "docs" / "spec"
TYPESCRIPT: str = "typescript@5.9.3"
PLACEHOLDER: str = '"__LITHIA_VERSION__"'
TAG: re.Pattern[str] = re.compile(r"^v(\d+(?:\.\d+)+)$")
IMAGE: re.Pattern[str] = re.compile(r"!\[[^\]]*\]\(([^)\s]+)\)")


@dataclass(frozen=True)
class Release:
    """A published release: its tag, version, publication date (YYYY-MM-DD, or empty) and assets by name."""

    tag: str
    version: str
    date: str
    assets: tuple[tuple[str, int], ...] = ()


def run(args: list[str], env: dict[str, str] | None = None) -> str:
    """Runs a command in the repository root and returns its standard output."""
    done: subprocess.CompletedProcess[str] = subprocess.run(
        args, cwd=ROOT, env=env, check=True, text=True, stdout=subprocess.PIPE
    )
    return done.stdout


def version_key(version: str) -> tuple[int, ...]:
    """Orders versions numerically: 0.3.1.10 after 0.3.1.9."""
    return tuple(int(part) for part in version.split("."))


@functools.cache
def compiled_script() -> str:
    """docs/site/ts/version.ts, compiled once per run."""
    run(["npx", "--yes", "-p", TYPESCRIPT, "tsc", "-p", str(SITE / "tsconfig.json")])
    compiled: str = (ROOT / "target" / "site-js" / "version.js").read_text()
    if PLACEHOLDER not in compiled:
        sys.exit("version.js lost its version placeholder")
    return compiled.strip()


def version_script(version: str) -> str:
    """The forwarding script for pages of `version`."""
    return compiled_script().replace(PLACEHOLDER, json.dumps(version))


def page(title: str, body: str, stylesheet: str, script: str = "", meta: str = "") -> str:
    """A plain site page in the spec's style."""
    return (
        '<!DOCTYPE html>\n<html lang="en">\n<head>\n<meta charset="utf-8">\n'
        '<meta name="viewport" content="width=device-width, initial-scale=1">\n'
        f"{meta}<title>{html.escape(title)}</title>\n"
        f'<link rel="stylesheet" href="{html.escape(stylesheet)}">\n'
        f"{f'<script>{script}</script>' if script else ''}\n</head>\n<body>\n{body}\n</body>\n</html>\n"
    )


def stub(target: str, title: str, script: str) -> str:
    """A page that forwards to `target`, after the script has had its say about `?v=`."""
    href: str = html.escape(target)
    return (
        '<!DOCTYPE html>\n<html lang="en">\n<head>\n<meta charset="utf-8">\n'
        f'<meta name="lithia-site" data-root-path="./" data-forward="{href}">\n'
        f"<title>{html.escape(title)}</title>\n<script>{script}</script>\n"
        f'<noscript><meta http-equiv="refresh" content="0; url={href}"></noscript>\n'
        f'</head>\n<body><p><a href="{href}">{html.escape(title)}</a></p></body>\n</html>\n'
    )


def target_dir() -> Path:
    """Cargo's target directory for this workspace."""
    metadata: dict[str, object] = json.loads(run(["cargo", "metadata", "--format-version", "1", "--no-deps"]))
    return Path(str(metadata["target_directory"]))


def newest_spec() -> Path:
    """The spec's newest Markdown version in docs/spec."""
    specs: list[Path] = sorted(
        SPEC_DIR.glob("LITHIA_design_spec_v*.md"), key=lambda path: version_key(path.stem.rsplit("_v", 1)[1])
    )
    if not specs:
        sys.exit("no spec in docs/spec")
    return specs[-1]


def build_api(script: str, out: Path) -> None:
    """Documents lithia and lithia-macro with the site's font and script, into `out`."""
    doc: Path = target_dir() / "doc"
    with tempfile.TemporaryDirectory() as scratch:
        header: Path = Path(scratch) / "header.html"
        header.write_text(f"<script>{script}</script>\n")
        flags: list[str] = ["--extend-css", str(SITE / "rustdoc.css"), "--html-in-header", str(header)]
        env: dict[str, str] = dict(os.environ)
        env.pop("RUSTDOCFLAGS", None)
        env["CARGO_ENCODED_RUSTDOCFLAGS"] = "\x1f".join(flags)
        shutil.rmtree(doc, ignore_errors=True)
        run(["cargo", "doc", "--no-deps", "-p", "lithia", "-p", "lithia-macro", "--locked"], env=env)
    shutil.copytree(doc, out)
    shutil.copytree(SITE / "fonts", out / "fonts")
    (out / "index.html").write_text(stub("lithia/index.html", "lithia API", script))


def build_spec(version: str, script: str, out: Path) -> None:
    """Renders the newest spec to `out`/index.html, with its PDF and images beside it."""
    source: Path = newest_spec()
    text: str = source.read_text()
    title: str = text.splitlines()[0].lstrip("# ").strip()
    pdf: Path = source.with_suffix(".pdf")
    out.mkdir(parents=True)
    with tempfile.TemporaryDirectory() as scratch:
        header: Path = Path(scratch) / "header.html"
        header.write_text(f"<script>{script}</script>\n")
        args: list[str] = [
            "pandoc", str(source), "--from", "gfm", "--to", "html5", "--standalone",
            "--toc", "--toc-depth", "2", "--template", str(SITE / "spec-template.html"),
            "--include-in-header", str(header), "--metadata", f"pagetitle={title}",
            "--variable", f"version={version}", "--output", str(out / "index.html"),
        ]
        if pdf.exists():
            args += ["--variable", f"pdf={pdf.name}"]
            shutil.copy2(pdf, out / pdf.name)
        run(args)
    shutil.copy2(SITE / "spec.css", out / "spec.css")
    shutil.copytree(SITE / "fonts", out / "fonts")
    for image in IMAGE.findall(text):
        found: Path = SPEC_DIR / image
        if found.is_file():
            (out / image).parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(found, out / image)


def snapshot(version: str, out: Path) -> None:
    """Writes one version's API docs and spec to `out`/api and `out`/spec."""
    script: str = version_script(version)
    shutil.rmtree(out, ignore_errors=True)
    out.mkdir(parents=True)
    build_api(script, out / "api")
    build_spec(version, script, out / "spec")


def github_releases(repo: str) -> list[Release]:
    """The repository's published releases with a version tag."""
    listing: str = run(
        ["gh", "api", f"repos/{repo}/releases", "--paginate", "--jq",
         '.[] | select(.draft | not) | {tag: .tag_name, date: (.published_at // "")[0:10], '
         'assets: [.assets[] | [.name, .id]]} | @json']
    )
    found: list[Release] = []
    for line in listing.splitlines():
        entry: dict[str, object] = json.loads(line)
        tag: str = str(entry["tag"])
        match: re.Match[str] | None = TAG.match(tag)
        listed: list[list[str | int]] = cast(list[list[str | int]], entry["assets"])
        if match:
            assets: tuple[tuple[str, int], ...] = tuple((str(name), int(asset)) for name, asset in listed)
            found.append(Release(tag, match.group(1), str(entry["date"]), assets))
    return found


def local_releases(source: Path) -> list[Release]:
    """Releases laid out as `source`/<tag>/<assets>, for building the site without GitHub."""
    found: list[Release] = []
    for entry in sorted(source.iterdir()):
        match: re.Match[str] | None = TAG.match(entry.name)
        if entry.is_dir() and match:
            found.append(Release(entry.name, match.group(1), ""))
    return found


def fetch(release: Release, repo: str | None, source: Path | None, into: Path) -> None:
    """Puts the release's docs archive and spec PDF into `into`."""
    into.mkdir(parents=True, exist_ok=True)
    if source is not None:
        for file in (source / release.tag).iterdir():
            shutil.copy2(file, into / file.name)
        return
    for name, asset in release.assets:
        if not (name.startswith("lithia-docs-") or name.startswith("LITHIA_design_spec_")):
            continue
        download: subprocess.CompletedProcess[bytes] = subprocess.run(
            ["gh", "api", "-H", "Accept: application/octet-stream", f"repos/{repo}/releases/assets/{asset}"],
            cwd=ROOT, stdout=subprocess.PIPE,
        )
        if download.returncode != 0:
            print(f"{release.tag}: {name} could not be downloaded", file=sys.stderr)
            continue
        (into / Path(name).name).write_bytes(download.stdout)


def place(release: Release, assets: Path, out: Path, script: str) -> bool:
    """Unpacks a release's docs into `out`; false when it has none. Early releases hold rustdoc alone."""
    archives: list[Path] = sorted(assets.glob("lithia-docs-*.tar.gz"))
    if not archives:
        print(f"{release.tag}: no docs archive; left out", file=sys.stderr)
        return False
    unpacked: Path = assets / "unpacked"
    with tarfile.open(archives[0]) as archive:
        archive.extractall(unpacked, filter="data")
    if (unpacked / "api").is_dir():
        shutil.copytree(unpacked / "api", out / "api")
        shutil.copytree(unpacked / "spec", out / "spec")
        return True
    shutil.copytree(unpacked / "doc", out / "api")
    (out / "api" / "index.html").write_text(stub("lithia/index.html", "lithia API", script))
    (out / "spec").mkdir()
    shutil.copy2(SITE / "spec.css", out / "spec" / "spec.css")
    shutil.copytree(SITE / "fonts", out / "spec" / "fonts")
    pdfs: list[Path] = sorted(assets.glob("LITHIA_design_spec_*.pdf"))
    links: str = "".join(f'<li><a href="{html.escape(p.name)}">{html.escape(p.name)}</a></li>' for p in pdfs)
    for pdf in pdfs:
        shutil.copy2(pdf, out / "spec" / pdf.name)
    body: str = (
        f'<header class="site"><a href="../">lithia {release.version}</a>'
        f'<a href="../api/lithia/index.html">API</a></header>\n<main><h1>Design spec</h1>\n'
        f"<p>This release predates the HTML spec; its spec is a PDF.</p>\n<ul>{links}</ul></main>"
    )
    (out / "spec" / "index.html").write_text(
        page(f"lithia {release.version}: design spec", body, "spec.css", script, '<meta name="lithia-site" data-root-path="./">\n')
    )
    return True


def description() -> str:
    """The lithia crate's one-line description."""
    manifest: dict[str, dict[str, str]] = tomllib.loads((ROOT / "crates" / "lithia" / "Cargo.toml").read_text())
    return manifest["package"]["description"]


def landing(placed: list[Release], keep: int) -> str:
    """The site's front page: the latest docs and every hosted version."""
    latest: Release = placed[0]
    rows: str = "\n".join(
        f"<tr><td>{r.version}</td><td>{r.date}</td>"
        f'<td><a href="versions/{r.version}/api/lithia/index.html">API</a></td>'
        f'<td><a href="versions/{r.version}/spec/index.html">Spec</a></td></tr>'
        for r in placed
    )
    body: str = (
        f'<header class="site"><a href="./">lithia</a><a href="api/lithia/index.html">API</a>'
        f'<a href="spec/index.html">Spec</a></header>\n<main>\n<h1>lithia</h1>\n<p>{html.escape(description())}</p>\n'
        f'<p>Latest: <strong>{latest.version}</strong>. <a href="api/lithia/index.html">API reference</a> · '
        f'<a href="spec/index.html">Design spec</a></p>\n'
        "<p>Any page under <code>api/</code> or <code>spec/</code> takes <code>?v=&lt;version&gt;</code> "
        f"to open that version instead, such as <code>api/lithia/index.html?v={latest.version}</code>.</p>\n"
        f"<h2>Versions</h2>\n<p>The newest {keep} releases are hosted here.</p>\n"
        f"<table><thead><tr><th>Version</th><th>Released</th><th></th><th></th></tr></thead>\n<tbody>\n{rows}\n</tbody></table>\n</main>"
    )
    return page("lithia docs", body, "site.css")


def version_landing(release: Release) -> str:
    """A hosted version's own front page."""
    body: str = (
        f'<header class="site"><a href="../../">lithia</a><a href="api/lithia/index.html">API</a>'
        f'<a href="spec/index.html">Spec</a></header>\n<main>\n<h1>lithia {release.version}</h1>\n'
        f'<p><a href="api/lithia/index.html">API reference</a> · <a href="spec/index.html">Design spec</a> · '
        f'<a href="../../">All versions</a></p>\n</main>'
    )
    return page(f"lithia {release.version}", body, "../../site.css")


def not_found(base: str, latest: str) -> str:
    """The 404 page; GitHub serves it at any missing path, so its links are absolute."""
    body: str = (
        f'<header class="site"><a href="{base}">lithia</a><a href="{base}api/lithia/index.html">API</a>'
        f'<a href="{base}spec/index.html">Spec</a></header>\n<main>\n<h1>Page not found</h1>\n'
        f"<p>The page does not exist, or not in the version asked for. Older versions leave the site as new ones "
        f'arrive; the <a href="{base}">front page</a> lists those still here. The latest is {latest}.</p>\n</main>'
    )
    return page("lithia: page not found", body, f"{base}site.css")


def assemble(out: Path, keep: int, base: str, repo: str | None, source: Path | None) -> None:
    """Builds the whole site in `out` from the newest `keep` releases."""
    releases: list[Release] = local_releases(source) if source is not None else github_releases(repo or "")
    releases.sort(key=lambda r: version_key(r.version), reverse=True)
    shutil.rmtree(out, ignore_errors=True)
    out.mkdir(parents=True)
    placed: list[Release] = []
    with tempfile.TemporaryDirectory() as scratch:
        for release in releases:
            if len(placed) == keep:
                break
            assets: Path = Path(scratch) / release.tag
            fetch(release, repo, source, assets)
            if place(release, assets, out / "versions" / release.version, version_script(release.version)):
                placed.append(release)
    if not placed:
        sys.exit("no release has docs to host")
    for release in placed:
        (out / "versions" / release.version / "index.html").write_text(version_landing(release))
    latest: Release = placed[0]
    shutil.copytree(out / "versions" / latest.version / "api", out / "api")
    shutil.copytree(out / "versions" / latest.version / "spec", out / "spec")
    shutil.copy2(SITE / "spec.css", out / "site.css")
    shutil.copytree(SITE / "fonts", out / "fonts")
    (out / "index.html").write_text(landing(placed, keep))
    (out / "404.html").write_text(not_found(base, latest.version))
    versions: dict[str, object] = {
        "latest": latest.version,
        "versions": [{"version": r.version, "tag": r.tag, "date": r.date} for r in placed],
    }
    (out / "versions.json").write_text(json.dumps(versions, indent=2) + "\n")
    print(f"site: {len(placed)} versions, latest {latest.version}")


def main() -> None:
    parser: argparse.ArgumentParser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    commands: argparse._SubParsersAction[argparse.ArgumentParser] = parser.add_subparsers(dest="command", required=True)
    one: argparse.ArgumentParser = commands.add_parser("snapshot", help="one version's API docs and spec")
    one.add_argument("--version", required=True)
    one.add_argument("--out", type=Path, required=True)
    whole: argparse.ArgumentParser = commands.add_parser("assemble", help="the whole site from releases")
    whole.add_argument("--out", type=Path, required=True)
    whole.add_argument("--keep", type=int, default=50, help="how many of the newest versions to host")
    whole.add_argument("--base-path", default="/", help="the site's path on its host, such as /lithia/")
    whole.add_argument("--repo", default=os.environ.get("GITHUB_REPOSITORY"))
    whole.add_argument("--from-dir", type=Path, help="releases as DIR/<tag>/<assets>, instead of GitHub")
    args: argparse.Namespace = parser.parse_args()
    if args.command == "snapshot":
        snapshot(args.version, args.out)
        return
    base: str = "/" + args.base_path.strip("/") + "/" if args.base_path.strip("/") else "/"
    if args.from_dir is None and not args.repo:
        sys.exit("assemble needs --repo or GITHUB_REPOSITORY, or --from-dir")
    assemble(args.out, args.keep, base, args.repo, args.from_dir)


if __name__ == "__main__":
    main()
