#!/usr/bin/env python3
"""Aggregate a browser DevTools profile (captured on a real phone) into
per-phase self-time for the generator.

Why this exists: the exported profiles do NOT carry wasm symbolication —
Firefox leaves the JIT'd wasm frames as raw addresses (0x..) with the real
function names sitting on PARENT frames (from the wasm name section); Chrome
folds unsymbolized JIT/wasm into one "(program)" bucket. So a naive leaf count
finds ~nothing. The trick (Firefox) is to attribute each sample to its DEEPEST
*named* wasm ancestor — that recovers self-time per generator function.

The wasm must be built with the `profiling` feature (web/serve-profile.sh) so
the per-phase functions aren't inlined and the name section is present.

Usage:
    python generator-lab/analyze_profile.py firefox-profile.json.gz [chrome-profile.json.gz ...]
    python generator-lab/analyze_profile.py            # defaults to ./firefox-profile.json.gz, ./chrome-profile.json.gz

Firefox profiles parse fully. Chrome's CPU-profile export is usually
unsymbolized (everything in "(program)"); read it in the DevTools UI instead.
"""

import gzip
import json
import re
import sys
from collections import Counter

# readable-name -> phase bucket. Order matters (first hit wins in `short`).
NAME_RE = re.compile(
    r"(solve_first|any_alt_solves|apply_clear|apply_place|clear_naked|"
    r"random_full_grid|naked_subset|hidden_subset|for_each_combination|"
    r"requirement_met|drain_naked_singles|hidden_single|step_harder|baseline|"
    r"from_board|min_target_uses|verify|run_attempts|attempt|fill|"
    r"malloc|free|from_iter)"
)

PHASE = {
    "solve_first": "prober(DFS)",
    "any_alt_solves": "prober(DFS)",
    "baseline": "baseline",
    "hidden_single": "baseline",
    "drain_naked_singles": "baseline",
    "step_harder": "baseline",
    "naked_subset": "baseline",
    "hidden_subset": "baseline",
    "for_each_combination": "baseline",
    "requirement_met": "baseline",
    "random_full_grid": "grid",
    "fill": "grid",
    "clear_naked": "clear_naked",
    "apply_clear": "bb-maint",
    "apply_place": "bb-maint",
    "Board::place": "bb-maint",
    "from_board": "bb-build",
    "verify": "verify",
    "min_target_uses": "verify",
    "malloc": "alloc",
    "free": "alloc",
    "from_iter": "alloc",
    "attempt": "loop",
    "run_attempts": "loop",
}


def short(raw):
    """Readable function name from a (possibly Rust-mangled) wasm symbol."""
    m = NAME_RE.search(raw)
    if m:
        return m.group(1)
    if "Board" in raw and "place" in raw:
        return "Board::place"
    if ".wasm" in raw:
        return "wasm:other"
    return None  # host / js / browser / idle


def phase_of(name):
    return PHASE.get(name, "wasm:other" if name else "host")


def report(title, self_counts, unit="samples"):
    """self_counts: Counter mapping readable-name -> self time (count or us)."""
    wasm = {k: v for k, v in self_counts.items() if k is not None}
    total = sum(wasm.values())
    if total == 0:
        print(f"\n===== {title} =====\n  no symbolized wasm frames found "
              f"(read this one in the DevTools UI).")
        return
    phases = Counter()
    for name, c in wasm.items():
        phases[phase_of(name)] += c
    print(f"\n===== {title} =====  wasm self-time total = {total} {unit}")
    print("  -- by phase --")
    for ph, c in phases.most_common():
        print(f"    {ph:14} {100 * c / total:5.1f}%   {c}")
    print("  -- by function --")
    for name, c in Counter(wasm).most_common(16):
        print(f"    {name:24} {100 * c / total:5.1f}%   {c}")


# ---- Firefox (Gecko profile) ------------------------------------------------

def parse_firefox(path):
    d = json.load(gzip.open(path))
    sh = d["shared"]
    frame_func = sh["frameTable"]["func"]
    func_name = sh["funcTable"]["name"]
    strings = sh["stringArray"]
    stk_frame = sh["stackTable"]["frame"]
    stk_prefix = sh["stackTable"]["prefix"]

    def frame_name(stack_idx):
        return strings[func_name[frame_func[stk_frame[stack_idx]]]]

    # Pick the content-tab GeckoMain thread that actually ran wasm (most wasm
    # samples), not the mostly-idle one.
    best = None
    for t in d["threads"]:
        if t.get("name") != "GeckoMain" or t.get("processType") != "tab":
            continue
        counts = Counter()
        wasm_samples = 0
        for si in t["samples"]["stack"]:
            if si is None:
                continue
            # Walk leaf -> root; take the deepest frame whose name is wasm.
            cur, found = si, None
            while cur is not None:
                nm = frame_name(cur)
                if ".wasm" in nm:
                    found = short(nm) or "wasm:other"
                    break
                cur = stk_prefix[cur]
            if found:
                counts[found] += 1
                wasm_samples += 1
        if best is None or wasm_samples > best[0]:
            best = (wasm_samples, counts)
    report(f"FIREFOX/SpiderMonkey  ({path})", best[1] if best else Counter())


# ---- Chrome (trace-event export) -------------------------------------------

def parse_chrome(path):
    d = json.load(gzip.open(path))
    events = d["traceEvents"] if isinstance(d, dict) else d
    # Reassemble per-tid CPU profiles from Profile/ProfileChunk events.
    from collections import defaultdict
    id2name = defaultdict(dict)
    samples = defaultdict(list)
    deltas = defaultdict(list)
    for e in events:
        if e.get("name") not in ("Profile", "ProfileChunk"):
            continue
        tid = e.get("tid")
        data = e.get("args", {}).get("data", {})
        cp = data.get("cpuProfile", data)
        for nd in cp.get("nodes", []):
            id2name[tid][nd["id"]] = nd.get("callFrame", {}).get("functionName") or "(anon)"
        samples[tid] += cp.get("samples", [])
        deltas[tid] += data.get("timeDeltas", [])
    if not samples:
        report(f"CHROME/V8  ({path})", Counter(), unit="us")
        return
    tid = max(samples, key=lambda k: len(samples[k]))
    self_us = Counter()
    for i, s in enumerate(samples[tid]):
        raw = id2name[tid].get(s, "?")
        # "(program)"/"(idle)" => host bucket (None); else map.
        name = short(raw) if raw not in ("(program)", "(idle)", "(garbage collector)") else None
        self_us[name] += max(deltas[tid][i] if i < len(deltas[tid]) else 0, 0)
    report(f"CHROME/V8  ({path})", self_us, unit="us")


def main(argv):
    paths = argv[1:] or ["firefox-profile.json.gz", "chrome-profile.json.gz"]
    for p in paths:
        try:
            head = gzip.open(p).read(64)
        except FileNotFoundError:
            print(f"(skip, not found: {p})")
            continue
        if b'"shared"' in gzip.open(p).read(4096) or b'"threads"' in head or b'"meta"' in head:
            parse_firefox(p)
        else:
            parse_chrome(p)


if __name__ == "__main__":
    main(sys.argv)
