#!/usr/bin/env python3
"""Fill the README's demo blocks with real command output.

Each block in README.md looks like

    <!-- DEMO:name -->

or an already-filled

    <!-- DEMO:name -->
    ```console
    ...
    ```

and is replaced by the output of the corresponding command below. Run this
after changing anything that affects what the CLI prints, so the README can
never drift from what the program actually does.
"""
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

DEMOS = {
    "opt": ["opt", "a*x^3 + b*x^2 + c*x + d", "--rules", "all", "--stats"],
    "divide": ["opt", "u / w + v / w", "--rules", "all"],
    "why": ["opt", "a*x^3 + b*x^2 + c*x + d", "--rules", "all", "--why"],
    "diff": ["diff", "x", "exp(sin(x * x))"],
    "vm": ["vm", "a*x^3 + b*x^2 + c*x + d", "--rules", "all"],
    "emit": ["emit", "a*x^3 + b*x^2 + c*x + d", "--rules", "all", "--name", "poly"],
    "time": ["time", "a*x^3 + b*x^2 + c*x + d", "--rules", "all"],
    "fuzz": ["fuzz", "--rules", "safe", "--count", "400", "--samples", "200"],
    "rules": ["rules"],
    "bench": ["bench"],
}


def run(args):
    proc = subprocess.run(
        [str(ROOT / "target" / "release" / "saturn"), *args, "--color", "never"],
        capture_output=True,
        text=True,
        cwd=ROOT,
    )
    out = (proc.stdout + proc.stderr).rstrip("\n")
    if proc.returncode != 0 and not out:
        raise SystemExit(f"saturn {' '.join(args)} failed with no output")
    return out


def main():
    subprocess.run(["cargo", "build", "--release"], cwd=ROOT, check=True)
    text = (ROOT / "README.md").read_text()

    for name, args in DEMOS.items():
        marker = f"<!-- DEMO:{name} -->"
        if marker not in text:
            print(f"note: no block for {name}", file=sys.stderr)
            continue
        shown = " ".join(f"'{a}'" if " " in a else a for a in args)
        body = f"{marker}\n\n```console\n$ saturn {shown}\n{run(args)}\n```"
        pattern = re.compile(
            re.escape(marker) + r"(?:\s*\n```console\n.*?\n```)?",
            re.DOTALL,
        )
        text = pattern.sub(lambda _: body, text, count=1)

    (ROOT / "README.md").write_text(text)
    print(f"regenerated {len(DEMOS)} demo blocks")


if __name__ == "__main__":
    main()
