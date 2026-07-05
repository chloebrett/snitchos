#!/usr/bin/env python3
"""PreToolUse hook: auto-reject non-atomic Bash commands.

Enforces the atomic-Bash rule in .claude/CLAUDE.md: no ;-chained whole
commands and no for/while/until loops. Pipes (cmd | filter) and &&
stay allowed. Escaped and quoted `;` (e.g. find -exec ... \\;, awk
'a;b') are neutralised before inspection, so they never trip the check.
"""
import json
import re
import sys


def strip_quotes_and_escapes(s):
    """Blank out quoted spans and escaped chars, leaving real shell syntax."""
    out = []
    i, n, quote = 0, len(s), None
    while i < n:
        c = s[i]
        if quote:
            if quote == '"' and c == "\\":
                i += 2
                continue
            if c == quote:
                quote = None
            i += 1
            continue
        if c == "\\":
            out.append(" ")
            i += 2
            continue
        if c in ("'", '"'):
            quote = c
            i += 1
            continue
        out.append(c)
        i += 1
    return "".join(out)


def reasons_to_reject(cmd):
    bare = strip_quotes_and_escapes(cmd)
    reasons = []
    if re.search(r"(^|[^\w.])(for|while|until)(\s|$)", bare):
        reasons.append("a shell loop (for/while/until ... do ... done)")
    if ";" in bare:
        reasons.append("';'-chained commands")
    return reasons


def main():
    try:
        data = json.load(sys.stdin)
    except (json.JSONDecodeError, ValueError):
        sys.exit(0)
    cmd = (data.get("tool_input") or {}).get("command") or ""
    if not cmd.strip():
        sys.exit(0)

    reasons = reasons_to_reject(cmd)
    if not reasons:
        sys.exit(0)

    why = " and ".join(reasons)
    msg = (
        f"Blocked: this Bash command contains {why}, which violates the "
        "atomic-Bash rule in .claude/CLAUDE.md. Issue one logical command "
        "per Bash call. If the steps are independent, emit them as separate "
        "(parallel) Bash calls instead of one loop/;-chain. A single pipe "
        "into a filter (cmd | grep ...) is fine."
    )
    print(
        json.dumps(
            {
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": "deny",
                    "permissionDecisionReason": msg,
                }
            }
        )
    )
    sys.exit(0)


if __name__ == "__main__":
    main()
