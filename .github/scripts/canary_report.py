#!/usr/bin/env python3
"""Turn the live-canary JUnit report into a job summary and an issue body.

Usage: canary_report.py <junit.xml> [target-label]

Writes the markdown table to $GITHUB_STEP_SUMMARY, the failure details to
canary-issue-body.md, and `failed=<n>` to $GITHUB_OUTPUT. Each failure's first
line is the canary's own message: `<tool>: <what broke> — adapter <file>`.
"""

import os
import sys
import xml.etree.ElementTree as ET
from datetime import datetime, timezone


def main(path: str, target: str) -> int:
    run_url = os.environ.get("RUN_URL", "")
    rows = []
    failures = []
    if os.path.exists(path):
        for case in ET.parse(path).getroot().iter("testcase"):
            name = case.get("name", "?")
            secs = float(case.get("time", "0") or 0)
            # Elements without children are falsy, so compare against None.
            failure = case.find("failure")
            if failure is None:
                failure = case.find("error")
            if failure is not None:
                message = (failure.text or failure.get("message") or "").strip()
                failures.append((name, message))
                rows.append((name, "❌ fail", secs, headline(message)))
            elif case.find("skipped") is not None:
                rows.append((name, "⏭ skipped", secs, ""))
            else:
                rows.append((name, "✅ pass", secs, "slow" if secs > 20 else ""))
    else:
        note = f"{path} was not produced; the build or the harness itself failed"
        failures.append(("(no report)", note))
        rows.append(("(no report)", "❌ fail", 0.0, note))

    rows.sort(key=lambda r: (r[1].startswith("✅"), r[0]))
    stamp = datetime.now(timezone.utc).strftime("%Y-%m-%d %H:%M UTC")
    passed = sum(1 for r in rows if r[1].startswith("✅"))

    table = ["| Canary | Result | Time | What broke |", "|---|---|---:|---|"]
    for name, status, secs, note in rows:
        table.append(f"| `{name}` | {status} | {secs:.1f}s | {escape(note)} |")

    summary = [
        f"## Live canaries ({target}): {passed} passed, {len(failures)} failed, {stamp}",
        "",
        *table,
        "",
    ]
    with open(os.environ.get("GITHUB_STEP_SUMMARY", os.devnull), "a") as fh:
        fh.write("\n".join(summary) + "\n")

    plural = "canary" if len(failures) == 1 else "canaries"
    issue = [
        f"**{len(failures)} live {plural} failed** against the {target} on {stamp}.",
        "",
        "Each entry is one tool. The first line names what broke and the adapter file behind it.",
        "A canary fails when an upstream changed shape or went stale (fix the adapter), or when",
        "the upstream is down (re-run tomorrow before touching the parser).",
        "",
    ]
    if run_url:
        issue += [f"Run: {run_url}", ""]
    for name, message in failures:
        issue += [f"### `{name}`", "", "```", message[:1500], "```", ""]
    issue += ["<details><summary>Full table</summary>", "", *table, "", "</details>"]
    with open("canary-issue-body.md", "w") as fh:
        fh.write("\n".join(issue) + "\n")

    with open(os.environ.get("GITHUB_OUTPUT", os.devnull), "a") as fh:
        fh.write(f"failed={len(failures)}\n")
    print("\n".join(summary))
    return 0


def headline(message: str) -> str:
    """The canary's own first line, skipping the test harness's panic preamble."""
    for line in message.splitlines():
        line = line.strip()
        if line and not line.startswith("thread '"):
            return line[:200]
    return ""


def escape(text: str) -> str:
    return text.replace("|", "\\|").replace("\n", " ")


if __name__ == "__main__":
    junit = sys.argv[1] if len(sys.argv) > 1 else "target/nextest/live/junit.xml"
    label = sys.argv[2] if len(sys.argv) > 2 else "local build"
    sys.exit(main(junit, label))
