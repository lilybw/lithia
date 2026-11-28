#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 lithia Authors
# SPDX-License-Identifier: MPL-2.0

"""The steps of publish.yml that need more than a line of shell; each exits 1 when it refuses.

  release.py check --version V [--dry-run]   is HEAD a release candidate for V? Writes tag, prerelease
                                             and previous to $GITHUB_OUTPUT when it is set
  release.py token                           refuses a token that is not short-lived and publish-only
  release.py publish --version V             publishes lithia-macro, then lithate, skipping either on crates.io
  release.py revoke                          revokes the token in use, so it cannot publish again
  release.py notes --version V               prints V's section of CHANGELOG.md

A dry run turns the checks a finished release needs, but a trial does not, into warnings.
"""

from __future__ import annotations

import datetime
import io
import json
import os
import re
import subprocess
import sys
import tomllib
import urllib.error
import urllib.request
from dataclasses import dataclass, field
from pathlib import Path

ROOT: Path = Path(__file__).resolve().parents[2]
CRATES: list[str] = ["lithia-macro", "lithate"]  # in the order they are published; `lithate` is the library `lithia`
API: str = "https://crates.io/api/v1"
AGENT: str = "lithia-release (https://github.com/lilybw/lithia)"
VERSION: re.Pattern[str] = re.compile(r"(\d+)\.(\d+)\.(\d+)(-[0-9A-Za-z]+(\.[0-9A-Za-z]+)*)?")
MUTANT_SHARDS: int = 16
# The longest a token stored in the `crates-io` environment may still have to live.
TOKEN_DAYS: int = 7


@dataclass
class Findings:
    """What a check found: refusals stop the release, warnings are reported."""

    refusals: list[str] = field(default_factory=list)
    warnings: list[str] = field(default_factory=list)

    def strict(self, ok: bool, message: str, dry_run: bool) -> None:
        """A rule a release must meet and a dry run only reports."""
        if not ok:
            (self.warnings if dry_run else self.refusals).append(message)

    def hard(self, ok: bool, message: str) -> None:
        """A rule even a dry run must meet."""
        if not ok:
            self.refusals.append(message)


def request(method: str, url: str, token: str | None = None) -> tuple[int, str]:
    """The status and body of an HTTP request; a status of 0 means no answer."""
    headers: dict[str, str] = {"User-Agent": AGENT, "Accept": "application/json"}
    if token is not None:
        headers["Authorization"] = token
    req: urllib.request.Request = urllib.request.Request(url, method=method, headers=headers)
    try:
        with urllib.request.urlopen(req, timeout=30) as response:
            return int(response.status), response.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as error:
        return int(error.code), error.read().decode("utf-8", "replace")
    except urllib.error.URLError as error:
        return 0, str(error.reason)


def published(crate: str, version: str) -> bool:
    """Whether crates.io has `version` of `crate`."""
    status: int
    body: str
    status, body = request("GET", f"{API}/crates/{crate}/{version}")
    if status not in (200, 404):
        raise SystemExit(f"crates.io answered {status} for {crate} {version}: {body[:200]}")
    return status == 200


def on_crates_io(crate: str) -> tuple[str, str]:
    """The repository crates.io names for `crate`, and its newest version not yanked; ('', '') if it has none."""
    status: int
    body: str
    status, body = request("GET", f"{API}/crates/{crate}")
    if status == 404:
        return "", ""
    if status != 200:
        raise SystemExit(f"crates.io answered {status} for {crate}: {body[:200]}")
    answer: object = json.loads(body)
    info: dict[str, object] = table(answer, "crate")
    newest: str = ""
    versions: object = answer.get("versions", []) if isinstance(answer, dict) else []
    if isinstance(versions, list):
        for entry in versions:
            if isinstance(entry, dict) and not entry.get("yanked") and isinstance(entry.get("num"), str):
                newest = str(entry["num"])
                break
    return str(info.get("repository") or ""), newest


def git(*args: str) -> str:
    return subprocess.run(["git", *args], cwd=ROOT, capture_output=True, encoding="utf-8", check=True).stdout.strip()


def gh_json(path: str) -> dict[str, object]:
    out: str = subprocess.run(
        ["gh", "api", path], cwd=ROOT, capture_output=True, encoding="utf-8", check=True,
    ).stdout
    value: object = json.loads(out)
    return value if isinstance(value, dict) else {}


def runs_on(sha: str) -> list[dict[str, object]]:
    """The workflow runs GitHub has for `sha`."""
    repo: str = os.environ.get("GITHUB_REPOSITORY", "lilybw/lithia")
    runs: object = gh_json(f"repos/{repo}/actions/runs?head_sha={sha}&per_page=100").get("workflow_runs", [])
    return [r for r in runs if isinstance(r, dict)] if isinstance(runs, list) else []


def full_extended(run: dict[str, object]) -> bool:
    """Whether an extended run ran, and passed, every job, cargo-mutants' shards included."""
    repo: str = os.environ.get("GITHUB_REPOSITORY", "lilybw/lithia")
    jobs: object = gh_json(f"repos/{repo}/actions/runs/{run['id']}/jobs?per_page=100").get("jobs", [])
    listed: list[dict[str, object]] = [j for j in jobs if isinstance(j, dict)] if isinstance(jobs, list) else []
    shards: int = sum(1 for j in listed if str(j.get("name", "")).startswith("cargo-mutants (shard "))
    return shards == MUTANT_SHARDS and all(j.get("conclusion") in ("success", "skipped") for j in listed)


def manifest() -> dict[str, object]:
    with open(ROOT / "Cargo.toml", "rb") as f:
        return tomllib.load(f)


def table(value: object, key: str) -> dict[str, object]:
    found: object = value.get(key, {}) if isinstance(value, dict) else {}
    return found if isinstance(found, dict) else {}


def metadata() -> dict[str, dict[str, object]]:
    """`cargo metadata`'s entry for each published crate."""
    out: str = subprocess.run(
        ["cargo", "metadata", "--no-deps", "--format-version", "1", "--locked"],
        cwd=ROOT, capture_output=True, encoding="utf-8", check=True,
    ).stdout
    packages: object = json.loads(out).get("packages", [])
    listed: list[object] = packages if isinstance(packages, list) else []
    return {str(p["name"]): p for p in listed if isinstance(p, dict) and p.get("name") in CRATES}


def changelog_section(base: str) -> tuple[str, str]:
    """The heading of `base`'s section in CHANGELOG.md, and its text."""
    text: str = (ROOT / "CHANGELOG.md").read_text(encoding="utf-8")
    found: re.Match[str] | None = re.search(rf"^## {re.escape(base)}\b.*$", text, re.M)
    if found is None:
        return "", ""
    rest: str = text[found.end():]
    following: re.Match[str] | None = re.search(r"^## ", rest, re.M)
    return found.group(0), rest[: following.start() if following else len(rest)].strip()


def check(version: str, dry_run: bool) -> int:
    findings: Findings = Findings()
    parsed: re.Match[str] | None = VERSION.fullmatch(version)
    if parsed is None:
        print(f"refused: `{version}` is not a version such as 1.0.0 or 1.0.0-rc.1")
        return 1
    base: str = ".".join(parsed.group(i) for i in (1, 2, 3))
    prerelease: bool = parsed.group(4) is not None
    ref: str = os.environ.get("GITHUB_REF", "refs/heads/" + git("rev-parse", "--abbrev-ref", "HEAD"))
    sha: str = os.environ.get("GITHUB_SHA") or git("rev-parse", "HEAD")
    findings.hard(ref == "refs/heads/nightly", f"run from `nightly`, not `{ref}`: a release candidate is a nightly commit")

    packages: dict[str, dict[str, object]] = metadata()
    for crate in CRATES:
        found: object = packages.get(crate, {}).get("version")
        findings.hard(found == version, f"Cargo.toml says {crate} {found}, not {version}: commit the version first")
        findings.strict(packages.get(crate, {}).get("publish") is None,
                        f"Cargo.toml keeps {crate} from being published (`publish`)", dry_run)
    deps: dict[str, object] = table(table(manifest(), "workspace"), "dependencies")
    pin: object = table(deps, "lithia-macro").get("version")
    findings.hard(pin == f"={version}", f"lithia-macro is pinned to {pin!r}, not \"={version}\"")
    findings.hard(table(deps, "lithia").get("version") == version, f"the workspace's lithia dependency is not {version}")

    repository: str = str(table(table(manifest(), "workspace"), "package").get("repository", ""))
    previous: str = ""
    for crate in CRATES:
        theirs: str
        newest: str
        theirs, newest = on_crates_io(crate)
        findings.hard(not newest or theirs.rstrip("/") == repository.rstrip("/"),
                      f"crates.io's `{crate}` is another project ({theirs or 'no repository'}); "
                      "publish under another package name, or have its owners add you")
        if crate == "lithate":
            previous = newest
    tag: str = f"v{version}"
    findings.hard(not git("ls-remote", "--tags", "origin", f"refs/tags/{tag}"), f"the tag {tag} exists already")
    on_crates: list[bool] = [published(crate, version) for crate in CRATES]
    findings.hard(not all(on_crates), f"crates.io has {version} of both crates already")
    if on_crates[0] and not on_crates[1]:
        findings.warnings.append(f"crates.io has lithia-macro {version} but not lithia: publishing resumes with lithia")

    heading: str
    heading, _ = changelog_section(base)
    findings.strict(bool(heading), f"CHANGELOG.md has no `## {base}` section", dry_run)
    findings.strict(prerelease or "unreleased" not in heading,
                    f"CHANGELOG.md still calls {base} unreleased: give its heading the date", dry_run)
    spec: Path = ROOT / f"docs/spec/LITHIA_design_spec_v{base}.md"
    findings.strict(spec.is_file() and spec.with_suffix(".pdf").is_file(),
                    f"no {spec.relative_to(ROOT).as_posix()} with its PDF", dry_run)
    readme: str = (ROOT / "README.md").read_text(encoding="utf-8")
    findings.strict("not on crates.io" not in readme, "README.md still says lithia is not on crates.io", dry_run)

    runs: list[dict[str, object]] = runs_on(sha)
    nightly_ok: bool = any(r.get("name") == "nightly" and r.get("event") == "push" and r.get("conclusion") == "success"
                           for r in runs)
    findings.hard(nightly_ok, f"no passing `nightly` run on {sha[:12]}: it is not a release candidate")
    extended: list[dict[str, object]] = [
        r for r in runs if r.get("name") == "extended" and r.get("event") in ("workflow_dispatch", "schedule")
        and r.get("conclusion") == "success"
    ]
    findings.strict(any(full_extended(r) for r in extended),
                    f"no full `extended` run, cargo-mutants included, passed on {sha[:12]}: "
                    "`gh workflow run extended.yml --ref nightly`, then run this again", dry_run)

    for warning in findings.warnings:
        print(f"::warning::{warning}")
    for refusal in findings.refusals:
        print(f"refused: {refusal}")
    output: str | None = os.environ.get("GITHUB_OUTPUT")
    if output:
        with open(output, "a", encoding="utf-8") as f:
            f.write(f"tag={tag}\nprerelease={'true' if prerelease else 'false'}\nprevious={previous}\n")
    if findings.refusals:
        return 1
    kind: str = "release candidate" if prerelease else "release"
    print(f"{sha[:12]} is a {kind} for {version}" + (f", after {previous}" if previous else ", the first"))
    return 0


def token() -> int:
    secret: str = os.environ.get("CARGO_REGISTRY_TOKEN", "")
    if not secret:
        print("refused: no CRATES_IO_PUBLISH secret, in the repository or the `crates-io` environment")
        return 1
    expires: str = os.environ.get("CRATES_IO_TOKEN_EXPIRES", "")
    today: datetime.date = datetime.datetime.now(datetime.UTC).date()
    try:
        until: datetime.date = datetime.date.fromisoformat(expires)
    except ValueError:
        print("refused: set the CRATES_IO_TOKEN_EXPIRES variable (repository or `crates-io` environment) to the token's "
              "expiry date (YYYY-MM-DD) when you store it")
        return 1
    if until < today:
        print(f"refused: the token expired on {until}")
        return 1
    if (until - today).days > TOKEN_DAYS:
        print(f"refused: the token lives until {until}, more than {TOKEN_DAYS} days; make one that expires sooner")
        return 1
    # A token limited to publishing is refused by endpoints outside its scopes; a legacy token is not.
    status: int
    status, _ = request("GET", f"{API}/me", secret)
    if status == 200:
        print("refused: the token can read /api/v1/me, so it is not limited to publishing; "
              "make one with only the publish-new and publish-update scopes, for lithate and lithia-macro")
        return 1
    if status not in (401, 403):
        print(f"refused: crates.io did not say whether the token's scopes are limited ({status})")
        return 1
    print(f"the token expires on {until} and is limited to the endpoints it was made for")
    return 0


def publish(version: str) -> int:
    for crate in CRATES:
        if published(crate, version):
            print(f"{crate} {version} is on crates.io already; skipped")
            continue
        print(f"publishing {crate} {version}", flush=True)
        done: subprocess.CompletedProcess[str] = subprocess.run(
            ["cargo", "publish", "-p", crate, "--locked"], cwd=ROOT, encoding="utf-8",
        )
        if done.returncode != 0:
            return 1
    return 0


def revoke() -> int:
    secret: str = os.environ.get("CARGO_REGISTRY_TOKEN", "")
    status: int
    body: str
    status, body = request("DELETE", f"{API}/tokens/current", secret) if secret else (0, "no token")
    if status in (200, 204):
        print("the token is revoked: store a new one for the next release")
        return 0
    print(f"::warning::the token could not be revoked ({status}: {body[:200]}); revoke it on crates.io")
    return 0


def notes(version: str) -> int:
    parsed: re.Match[str] | None = VERSION.fullmatch(version)
    if parsed is None:
        return 1
    base: str = ".".join(parsed.group(i) for i in (1, 2, 3))
    text: str
    _, text = changelog_section(base)
    sources: str = "https://github.com/lilybw/lithia"
    print(f"lithia {version} on [crates.io](https://crates.io/crates/lithate/{version}), as `lithate`.\n\n{text}\n\n"
          f"[Design spec]({sources}/blob/v{version}/docs/spec/LITHIA_design_spec_v{base}.md)")
    return 0


def main(argv: list[str]) -> int:
    if isinstance(sys.stdout, io.TextIOWrapper):
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")  # Windows consoles default to a code page
    command: str = argv[0] if argv else ""
    version: str = argv[argv.index("--version") + 1] if "--version" in argv[:-1] else ""
    if command == "check" and version:
        return check(version, "--dry-run" in argv)
    if command == "token":
        return token()
    if command == "publish" and version:
        return publish(version)
    if command == "revoke":
        return revoke()
    if command == "notes" and version:
        return notes(version)
    print(__doc__, file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
