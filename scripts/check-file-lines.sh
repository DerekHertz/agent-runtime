#!/usr/bin/env bash
# Fail if any authored file has more than MAX_LINES lines (default 1000).
#
# Authored files are the tracked files plus untracked files that are not ignored
# (`git ls-files --cached --others --exclude-standard`), so ignored paths such as
# target/ never count. Runs from any working directory.
#
# Exempt: Cargo.lock, because it is generated. Keep this list to generated files only.
set -euo pipefail

MAX_LINES="${MAX_LINES:-1000}"
EXEMPT=(Cargo.lock)

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

fail=0
while IFS= read -r -d '' file; do
  [ -f "$file" ] || continue # listed but deleted, or not a regular file
  skip=0
  for exempt in "${EXEMPT[@]}"; do
    if [ "$file" = "$exempt" ]; then
      skip=1
    fi
  done
  if [ "$skip" -eq 1 ]; then
    continue
  fi
  lines="$(wc -l < "$file" | tr -d ' ')"
  # wc -l counts newlines; a final line without one still counts as a line.
  if [ -s "$file" ] && [ -n "$(tail -c1 "$file")" ]; then
    lines=$((lines + 1))
  fi
  if [ "$lines" -gt "$MAX_LINES" ]; then
    printf '%s: %s lines (max %s)\n' "$file" "$lines" "$MAX_LINES" >&2
    fail=1
  fi
done < <(git ls-files --cached --others --exclude-standard -z | sort -zu)

if [ "$fail" -ne 0 ]; then
  echo "error: authored files over $MAX_LINES lines; split them." >&2
  exit 1
fi
echo "ok: no authored file exceeds $MAX_LINES lines"
