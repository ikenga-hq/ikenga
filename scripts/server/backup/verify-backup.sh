#!/bin/bash
# Verify a gzip-compressed pg_dump (plain format) before it is uploaded.
#
#   verify-backup.sh <backup.sql.gz> [--verbose]
#
# Ported from the rex-vps db-backup scripts (D-B10). Changes: no emoji or ANSI
# colour (the output goes to a log), no BSD `stat`, and the checks are
# stricter: a missing pg_dump header is now a failure (it was a warning), and
# the dump must END with pg_dump's completion marker, which catches a stream
# that was cut short but still gunzips cleanly.
#
# Exit 0 = every check passed. Output names the file and the check, never
# any content of the dump beyond the first lines under --verbose.
set -euo pipefail

VERBOSE=0
FILE=""
for arg in "$@"; do
  case "$arg" in
    --verbose) VERBOSE=1 ;;
    -*) echo "usage: $0 <backup.sql.gz> [--verbose]" >&2; exit 2 ;;
    *) if [[ -z "$FILE" ]]; then FILE="$arg"; else echo "usage: $0 <backup.sql.gz> [--verbose]" >&2; exit 2; fi ;;
  esac
done
[[ -n "$FILE" ]] || { echo "usage: $0 <backup.sql.gz> [--verbose]" >&2; exit 2; }

fail() { echo "FAIL: $*" >&2; exit 1; }

[[ -f "$FILE" ]] || fail "file not found: $FILE"
SIZE="$(stat -c %s -- "$FILE")"
echo "verify: $(basename -- "$FILE") ($SIZE bytes)"
# An empty database still dumps to well over this.
(( SIZE >= 100 )) || fail "file is suspiciously small (< 100 bytes)"

gzip -t -- "$FILE" 2>/dev/null || fail "gzip integrity check failed (corrupt archive)"
echo "verify: gzip integrity ok"

# `head` closes the pipe early; pipefail would then report gunzip's SIGPIPE.
HEAD="$(gunzip -c -- "$FILE" | head -n 30 || true)"
grep -q 'PostgreSQL database dump' <<<"$HEAD" || fail "no pg_dump header in the first lines"
grep -qE '^(SET|CREATE|COPY|INSERT|ALTER|SELECT|--)' <<<"$HEAD" || fail "no SQL statements in the first lines"
echo "verify: pg_dump header ok"

TAIL="$(gunzip -c -- "$FILE" | tail -n 10)"
grep -q 'PostgreSQL database dump complete' <<<"$TAIL" || fail "the dump does not end with pg_dump's completion marker (truncated?)"
echo "verify: completion marker ok"

if (( VERBOSE )); then
  echo "verify: $(gunzip -c -- "$FILE" | wc -l | tr -d ' ') lines"
  echo "verify: first 10 lines"
  head -n 10 <<<"$HEAD"
fi
echo "verify: ok"
