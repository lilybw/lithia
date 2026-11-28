# SPDX-FileCopyrightText: 2026 lithia Authors
# SPDX-License-Identifier: MPL-2.0

"""Charts the scaling benchmark's and lock_bench.py's Markdown tables as one HTML page.

Each row becomes a chart per measure against the thread count, on log scales: the line is the median
thread, the band the fastest to the slowest. lock_bench.py's `--stats` figures get charts of their own.
Usage: `bench_plot.py RESULTS.txt... [--out plot.html]`; `-` reads standard input.
"""

from __future__ import annotations

import argparse
import html
import math
import re
import sys
from dataclasses import dataclass, field
from pathlib import Path

ROOT: Path = Path(__file__).resolve().parents[2]
BENCH: Path = ROOT / "crates" / "bench" / "benches" / "scaling.rs"

UNITS: dict[str, float] = {"ns": 1.0, "µs": 1e3, "us": 1e3, "ms": 1e6, "s": 1e9}
VALUE: str = r"(\d+(?:\.\d+)?)\s*(ns|µs|us|ms|s)(?![a-z])"
RANGE: re.Pattern[str] = re.compile(r"\(\s*" + VALUE + r"\s*[–-]\s*" + VALUE + r"\s*\)")
TAKES: re.Pattern[str] = re.compile(r"(\d+)\s*takes")
HELD: re.Pattern[str] = re.compile(r"held\s*" + VALUE + r"\s*/\s*" + VALUE + r"\s*max")
WAITED: re.Pattern[str] = re.compile(r"waited\s*" + VALUE)
HOST: re.Pattern[str] = re.compile(r"^Host: (.*?)\. Threads per cell")


@dataclass
class Point:
    """One measure in one cell: the median thread and, with several threads, the fastest and slowest."""

    median: float
    low: float | None = None
    high: float | None = None


@dataclass
class Stats:
    """The sweep lock's figures for one cell, under `--cfg lithia_lock_stats`."""

    takes: int
    held: float
    held_max: float
    waited: float


@dataclass
class Cell:
    """One table cell: a point per measure, and the lock's figures if printed."""

    points: list[Point]
    stats: Stats | None
    text: str


@dataclass
class Row:
    """One benchmark row: cells by thread count, for one series (a lock, or the build measured)."""

    name: str
    series: str
    cells: dict[int, Cell] = field(default_factory=dict)


@dataclass
class Section:
    """One run's tables: the host line and its rows, in the order printed."""

    host: str
    rows: list[Row] = field(default_factory=list)


def nanos(number: str, unit: str) -> float:
    """A duration in nanoseconds."""
    return float(number) * UNITS[unit]


def split_top(text: str) -> list[str]:
    """`text` split at each `/` outside parentheses."""
    parts: list[str] = [""]
    depth: int = 0
    for char in text:
        depth += char == "("
        depth -= char == ")"
        if char == "/" and depth == 0:
            parts.append("")
        else:
            parts[-1] += char
    return [part.strip() for part in parts]


def parse_cell(text: str) -> Cell | None:
    """A cell such as `701 ns / 13.60 ms (13.03 ms–13.45 ms) [2 takes, held 1 ms / 2 ms max, waited 0 ns]`."""
    bracket: int = text.find("[")
    main: str = text if bracket < 0 else text[:bracket]
    stats: Stats | None = None
    if bracket >= 0:
        inside: str = text[bracket:]
        takes: re.Match[str] | None = TAKES.search(inside)
        held: re.Match[str] | None = HELD.search(inside)
        waited: re.Match[str] | None = WAITED.search(inside)
        if takes and held and waited:
            stats = Stats(int(takes[1]), nanos(held[1], held[2]), nanos(held[3], held[4]),
                          nanos(waited[1], waited[2]))
    points: list[Point] = []
    for part in split_top(main):
        median: re.Match[str] | None = re.match(VALUE, part)
        if median is None:
            return None
        spread: re.Match[str] | None = RANGE.search(part)
        point: Point = Point(nanos(median[1], median[2]))
        if spread:
            point.low, point.high = nanos(spread[1], spread[2]), nanos(spread[3], spread[4])
        points.append(point)
    return Cell(points, stats, text.strip())


def cells_of(line: str) -> list[str]:
    """The cells of a Markdown table line."""
    return [cell.strip() for cell in line.strip().strip("|").split("|")]


def parse(text: str) -> list[Section]:
    """Every run in `text`: scaling tables (`| Benchmark |`) and lock_bench tables (`### row`, `| Lock |`)."""
    sections: list[Section] = []
    heading: str | None = None
    counts: list[int] = []
    kind: str = ""
    for line in text.splitlines():
        host: re.Match[str] | None = HOST.match(line.strip())
        if host:
            sections.append(Section(host[1]))
            continue
        if line.startswith("### "):
            heading = line[4:].strip()
            continue
        if not line.startswith("|") or not sections:
            continue
        cells: list[str] = cells_of(line)
        if cells[0] in ("Benchmark", "Lock"):
            kind = cells[0]
            counts = [int(c) for c in cells[1:] if c.isdigit()]
            continue
        if set(cells[0]) <= set("-: ") or not counts:
            continue
        name: str = cells[0] if kind == "Benchmark" else heading or "?"
        series: str = "" if kind == "Benchmark" else cells[0]
        row: Row = Row(name, series)
        for count, cell in zip(counts, cells[1:]):
            parsed: Cell | None = parse_cell(cell)
            if parsed is not None:
                row.cells[count] = parsed
        sections[-1].rows.append(row)
    return sections


def measures(name: str, width: int) -> list[str]:
    """What each value of `name`'s cells is, from the bench's own description where it can be found."""
    reads: str = ""
    if BENCH.is_file():
        source: str = BENCH.read_text(encoding="utf-8")
        found: re.Match[str] | None = re.search(r'name: "' + re.escape(name) + r'",\s*reads: "([^"]*)"', source)
        reads = found[1] if found else ""
    # What a cell is comes after the last colon: "...: per creation / per drop, median".
    names: list[str] = re.findall(r"per ([a-z][a-z -]*?)(?= /|[,;:.]|$)", reads.rsplit(":", 1)[-1])
    if len(names) >= width:
        return [f"per {n}" for n in names[:width]]
    return [f"value {i + 1}" for i in range(width)] if width > 1 else ["per operation"]


# Categorical slots 1–5 (light, dark): blue, orange, aqua, yellow, magenta.
SLOTS: list[tuple[str, str]] = [("#257de0", "#338ced"), ("#f47230", "#e76624"), ("#00b894", "#00a888"),
                                ("#f2a600", "#d28e05"), ("#ee7fac", "#e24d8b")]
WIDE: int = 360
TALL: int = 210
LEFT: int = 58
RIGHT: int = 14
TOP: int = 14
BOTTOM: int = 34


def label(ns: float) -> str:
    """A duration as the benchmark prints it, to three significant figures."""
    for unit, scale in (("s", 1e9), ("ms", 1e6), ("µs", 1e3)):
        if ns >= scale:
            return f"{ns / scale:.3g} {unit}"
    return f"{ns:.3g} ns"


def ticks(low: float, high: float) -> list[float]:
    """1, 2 and 5 times powers of ten between `low` and `high`, thinned to at most seven."""
    found: list[float] = []
    for exponent in range(math.floor(math.log10(low)) - 1, math.ceil(math.log10(high)) + 1):
        for step in (1.0, 2.0, 5.0):
            value: float = step * 10.0**exponent
            if low <= value <= high:
                found.append(value)
    while len(found) > 7:
        found = found[::2]
    return found


@dataclass
class Series:
    """One line of a chart: its name, slot and points by thread count."""

    name: str
    slot: int
    points: dict[int, Point]
    notes: dict[int, str]


def chart(title: str, counts: list[int], series: list[Series]) -> str:
    """An SVG chart: log-scaled time against thread count, a band for each series' spread."""
    values: list[float] = [v for s in series for p in s.points.values()
                           for v in (p.median, p.low, p.high) if v is not None and v > 0]
    if not values or not counts:
        return ""
    low: float = min(values) / 1.25
    high: float = max(values) * 1.25
    if high / low < 10:
        middle: float = math.sqrt(low * high)
        low, high = middle / math.sqrt(10), middle * math.sqrt(10)

    def y(v: float) -> float:
        return TOP + (TALL - TOP - BOTTOM) * (1 - (math.log(v) - math.log(low)) / (math.log(high) - math.log(low)))

    def x(count: int) -> float:
        span: float = WIDE - LEFT - RIGHT
        return LEFT + (span * counts.index(count) / (len(counts) - 1) if len(counts) > 1 else span / 2)

    out: list[str] = [f'<svg viewBox="0 0 {WIDE} {TALL}" role="img" aria-label="{html.escape(title)}">']
    for tick in ticks(low, high):
        out.append(f'<line class="grid" x1="{LEFT}" x2="{WIDE - RIGHT}" y1="{y(tick):.1f}" y2="{y(tick):.1f}"/>')
        out.append(f'<text class="axis" x="{LEFT - 6}" y="{y(tick) + 4:.1f}" text-anchor="end">{label(tick)}</text>')
    for count in counts:
        out.append(f'<text class="axis" x="{x(count):.1f}" y="{TALL - BOTTOM + 16}" text-anchor="middle">{count}</text>')
    out.append(f'<text class="axis" x="{(LEFT + WIDE - RIGHT) / 2:.1f}" y="{TALL - 4}" text-anchor="middle">threads</text>')
    for s in series:
        known: list[int] = [c for c in counts if c in s.points and s.points[c].median > 0]
        spread: list[int] = [c for c in known if s.points[c].low is not None and s.points[c].high is not None]
        if len(spread) >= 2:
            upper: list[str] = [f"{x(c):.1f},{y(s.points[c].high or 0):.1f}" for c in spread]
            lower: list[str] = [f"{x(c):.1f},{y(s.points[c].low or 0):.1f}" for c in reversed(spread)]
            out.append(f'<polygon class="band s{s.slot}" points="{" ".join(upper + lower)}"/>')
        if len(known) >= 2:
            line: str = " ".join(f"{x(c):.1f},{y(s.points[c].median):.1f}" for c in known)
            out.append(f'<polyline class="line s{s.slot}" points="{line}"/>')
        for c in known:
            p: Point = s.points[c]
            tip: str = f"{s.name + ', ' if s.name else ''}{c} threads: {label(p.median)}"
            if p.low is not None and p.high is not None:
                tip += f" (fastest {label(p.low)}, slowest {label(p.high)})"
            if c in s.notes:
                tip += f"; {s.notes[c]}"
            out.append(f'<g class="dot s{s.slot}"><title>{html.escape(tip)}</title>'
                       f'<circle class="hit" cx="{x(c):.1f}" cy="{y(p.median):.1f}" r="11"/>'
                       f'<circle cx="{x(c):.1f}" cy="{y(p.median):.1f}" r="4"/></g>')
    out.append("</svg>")
    return f'<figure><figcaption>{html.escape(title)}</figcaption>{"".join(out)}</figure>'


def table(name: str, rows: list[Row], counts: list[int]) -> str:
    """The numbers behind a row's charts, as printed."""
    head: str = "".join(f"<th>{c}</th>" for c in counts)
    body: str = "".join(
        f"<tr><th>{html.escape(r.series or name)}</th>"
        + "".join(f"<td>{html.escape(r.cells[c].text) if c in r.cells else ''}</td>" for c in counts) + "</tr>"
        for r in rows)
    return (f'<details><summary>Numbers</summary><div class="scroll"><table><tr><th></th>{head}</tr>'
            f"{body}</table></div></details>")


def render(sections: list[Section]) -> str:
    """The page: a part per run, a group of charts per benchmark row."""
    parts: list[str] = []
    for section in sections:
        names: list[str] = list(dict.fromkeys(r.name for r in section.rows))
        locks: list[str] = list(dict.fromkeys(r.series for r in section.rows if r.series))
        slot: dict[str, int] = {lock: i % len(SLOTS) + 1 for i, lock in enumerate(locks)}
        legend: str = "".join(f'<span class="key"><i class="s{slot[k]}"></i>{html.escape(k)}</span>' for k in locks)
        groups: list[str] = []
        for name in names:
            rows: list[Row] = [r for r in section.rows if r.name == name]
            counts: list[int] = sorted({c for r in rows for c in r.cells})
            width: int = max((len(cell.points) for r in rows for cell in r.cells.values()), default=0)
            figures: list[str] = []
            for i, measure in enumerate(measures(name, width)):
                series: list[Series] = [
                    Series(r.series, slot.get(r.series, 1),
                           {c: cell.points[i] for c, cell in r.cells.items() if len(cell.points) > i}, {})
                    for r in rows]
                figures.append(chart(f"{name}, {measure}", counts, series))
            if any(cell.stats for r in rows for cell in r.cells.values()):
                for title, pick in (("sweep lock held, mean per acquisition", "held"),
                                    ("sweep lock waited for, mean per acquisition", "waited")):
                    series = []
                    for r in rows:
                        points: dict[int, Point] = {}
                        notes: dict[int, str] = {}
                        for c, cell in r.cells.items():
                            if cell.stats is None:
                                continue
                            st: Stats = cell.stats
                            points[c] = Point(st.held, None, None) if pick == "held" else Point(st.waited)
                            notes[c] = f"{st.takes} acquisitions, longest hold {label(st.held_max)}"
                        series.append(Series(r.series, slot.get(r.series, 1), points, notes))
                    figures.append(chart(f"{name}, {title}", counts, series))
            groups.append(f'<section><h3>{html.escape(name)}</h3><div class="grid">{"".join(figures)}</div>'
                          f"{table(name, rows, counts)}</section>")
        parts.append(f'<h2>{html.escape(section.host)}</h2>'
                     + (f'<p class="legend">{legend}</p>' if legend else "") + "".join(groups))
    colors: str = "".join(f"--s{i + 1}: {light};" for i, (light, _) in enumerate(SLOTS))
    darks: str = "".join(f"--s{i + 1}: {dark};" for i, (_, dark) in enumerate(SLOTS))
    slots: str = "".join(
        f".s{i}{{--c: var(--s{i})}}" for i in range(1, len(SLOTS) + 1))
    return f"""<!doctype html>
<html lang="en">
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover">
<title>Benchmark charts</title>
<style>
/* One column of benchmark rows; each row a grid of small charts on a shared log scale per chart. */
:root {{ --bg: #f5f6f8; --surface: #ffffff; --fg: #191919; --muted: #62656b; --grid: #e3e5e9; {colors} }}
@media (prefers-color-scheme: dark) {{ :root:not([data-theme="light"]) {{ --bg: #141518; --surface: #1c1d21;
  --fg: #efefef; --muted: #9c9ea4; --grid: #2f3136; {darks} color-scheme: dark; }} }}
:root[data-theme="dark"] {{ --bg: #141518; --surface: #1c1d21; --fg: #efefef; --muted: #9c9ea4; --grid: #2f3136;
  {darks} color-scheme: dark; }}
body {{ background: var(--bg); color: var(--fg); font: 14px/1.5 system-ui, -apple-system, "Segoe UI", sans-serif;
  padding-block: 24px 48px; padding-left: max(16px, env(safe-area-inset-left, 0px));
  padding-right: max(16px, env(safe-area-inset-right, 0px)); max-width: 1200px; margin: 0 auto; }}
h1 {{ font-size: 22px; margin: 0 0 4px; text-wrap: balance; }}
h2 {{ font-size: 17px; margin: 32px 0 8px; }}
h3 {{ font-size: 15px; margin: 24px 0 8px; font-family: ui-monospace, "Cascadia Mono", Menlo, monospace; }}
p {{ max-width: 70ch; color: var(--muted); margin: 0 0 8px; }}
.grid {{ display: grid; grid-template-columns: repeat(auto-fill, minmax(280px, 1fr)); gap: 12px; }}
figure {{ margin: 0; background: var(--surface); border-radius: 6px; padding: 8px 8px 4px; min-width: 0; }}
figcaption {{ font-size: 12.5px; color: var(--muted); padding: 0 4px 2px; }}
svg {{ width: 100%; height: auto; display: block; font-variant-numeric: tabular-nums; }}
.grid line {{ stroke: var(--grid); stroke-width: 1; }}
.axis {{ fill: var(--muted); font-size: 11px; }}
{slots}
.line {{ fill: none; stroke: var(--c); stroke-width: 2; stroke-linejoin: round; }}
.band {{ fill: var(--c); fill-opacity: 0.14; stroke: none; }}
.dot circle {{ fill: var(--c); stroke: var(--surface); stroke-width: 2; }}
.dot circle.hit {{ fill: transparent; stroke: none; }}
.dot:hover circle:not(.hit) {{ stroke: var(--fg); }}
.legend {{ display: flex; flex-wrap: wrap; gap: 6px 16px; color: var(--fg); }}
.key {{ display: inline-flex; align-items: center; gap: 6px; }}
.key i {{ width: 14px; height: 3px; border-radius: 2px; background: var(--c); display: inline-block; }}
details {{ margin-top: 8px; }}
summary {{ cursor: pointer; color: var(--muted); }}
summary:focus-visible {{ outline: 2px solid var(--s1); outline-offset: 2px; }}
.scroll {{ overflow-x: auto; }}
table {{ border-collapse: collapse; font-size: 12px; font-variant-numeric: tabular-nums; margin-top: 6px; }}
th, td {{ text-align: left; padding: 3px 8px; border-bottom: 1px solid var(--grid); vertical-align: top; }}
</style>
<h1>Benchmark charts</h1>
<p>Time against threads, both on log scales. The line is the median thread; the band runs from the fastest
thread to the slowest. Hover a point for its figures; each row's numbers are under it as printed.</p>
{"".join(parts)}
</html>
"""


def main() -> int:
    parser: argparse.ArgumentParser = argparse.ArgumentParser(description=__doc__.splitlines()[0] if __doc__ else "")
    parser.add_argument("inputs", nargs="+", help="benchmark output files, or - for standard input")
    parser.add_argument("--out", default="bench-plot.html", help="the page to write (default: bench-plot.html)")
    args: argparse.Namespace = parser.parse_args()
    text: str = "\n".join(sys.stdin.read() if name == "-" else Path(name).read_text(encoding="utf-8")
                          for name in args.inputs)
    sections: list[Section] = parse(text)
    if not any(s.rows for s in sections):
        print("no benchmark tables found", file=sys.stderr)
        return 1
    Path(args.out).write_text(render(sections), encoding="utf-8", newline="\n")
    print(f"{args.out}: {sum(len(s.rows) for s in sections)} rows")
    return 0


if __name__ == "__main__":
    sys.exit(main())
