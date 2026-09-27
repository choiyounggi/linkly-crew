#!/bin/sh
# fake-browser-cli.sh — deterministic stand-in for a real browser CLI, used
# only by crew-run/tests/m10_browser_dod.rs. It performs NO network access,
# drives NO real browser, and captures NO screen; it is a pure shell script
# that inspects its argv and exits.
#
# It is NOT a real tool. The argv contract it encodes
#     argv = [resolved_binary_path, flow, kind, value]
# (crew-lead/src/browser_exec.rs, t2 contract section 8) has never been
# checked against any real browser CLI, because none exists on this machine.
# Whoever first configures a real one owns that spot check.
#
# Exit codes: 0 / 3 are the two CONTRACT outcomes the suite asserts on
# (check passed / check failed). 64-67 are argv-contract violations — each
# distinct so a test failure names which part of the contract broke rather
# than collapsing into one generic non-zero.
set -eu

# The argument-count check comes FIRST, before the log line below: under
# `set -u`, expanding "$3" with only two arguments aborts the shell with
# "$3: unbound variable" and rc=1, so a log-first ordering could never
# report rc=64 for a short argv. Verified both ways.
if [ "$#" -ne 3 ]; then
  exit 64
fi

flow="$1"
kind="$2"
value="$3"

# Recorded beside this script (never in the repo tree: each test copies this
# file into its own scratch dir, so "$0"'s directory is that scratch dir).
# Tab-separated because a tab cannot occur in a URL or in this suite's
# payloads, so the ordered triple parses back unambiguously.
printf '%s\t%s\t%s\n' "$flow" "$kind" "$value" >> "$(dirname "$0")/argv.log"

# The working directory this was spawned in, recorded separately so the argv
# assertion stays purely about argv. User decision D3 requires the browser
# DoD to execute in the SAME per-role worktree the cmd DoD uses; recording
# the real cwd is what lets a test observe that, rather than re-asserting a
# path the test itself computed. `-P` so a symlinked path cannot make an
# equality check pass or fail for the wrong reason.
pwd -P >> "$(dirname "$0")/cwd.log"

case "$kind" in
  text|visible|url) ;;
  *) exit 65 ;;
esac

case "$flow" in
  http://localhost*|http://127.0.0.1*) ;;
  # Defence in depth only: production refuses a non-localhost flow before
  # any spawn (browser_exec.rs validate_localhost_url), so this branch
  # cannot fire from this suite.
  *) exit 66 ;;
esac

case "$value" in
  crew-browser-dod-pass) exit 0 ;;
  crew-browser-dod-fail) exit 3 ;;
  *) exit 67 ;;
esac
