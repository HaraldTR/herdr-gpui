#!/usr/bin/env bash
# Fail when a tracked source file (Rust, Python, shell, Swift) exceeds MAX_LINES.
#
# Files that were already larger when the limit arrived are allowlisted at their
# size then. They may shrink but never grow, and an entry must be removed once
# its file is split below the limit or deleted, so the list only gets shorter.
# Split by responsibility instead of raising an entry.

set -euo pipefail

MAX_LINES=1000

# "<lines> <path>": the most lines each oversized file may keep.
ALLOW_LIST='
2006 crates/herdr-gpui/src/smoke.rs
1940 crates/herdr-gpui/src/config.rs
1442 crates/herdr-gpui/src/menu/workspace.rs
1193 crates/herdr-gpui/src/settings_window/themes.rs
1179 crates/herdr-gpui/src/sidebar/row.rs
1119 crates/herdr-gpui/src/menu/devices.rs
1108 crates/herdr-gpui/src/endpoint.rs
1105 crates/herdr-gpui/src/usage/providers/alibabatokenplan.rs
1104 crates/herdr-gpui/src/updater/install.rs
1097 crates/herdr-gpui/src/teleport/job.rs
1091 crates/herdr-gpui/src/sidebar/layout_tests/text_width.rs
1077 crates/herdr-gpui/src/settings_window.rs
1075 crates/herdr-gpui/src/usage/probe.rs
1062 crates/herdr-gpui/src/menu/chrome.rs
1047 crates/herdr-gpui/src/browser/annotate_view.rs
1030 crates/herdr-client/src/clipboard_tests.rs
1012 crates/herdr-gpui/src/browser/view.rs
1008 crates/herdr-gpui/src/settings_window/native.rs
'

cd "$(git rev-parse --show-toplevel)"

allowed_lines() {
  printf '%s\n' "$ALLOW_LIST" | awk -v path="$1" '$2 == path { print $1 }'
}

failures=0
fail() {
  echo "FAIL: $1"
  failures=$((failures + 1))
}

while IFS= read -r -d '' file; do
  # A path deleted in the working tree is still listed by git until staged.
  [[ -f "$file" ]] || continue
  lines=$(wc -l < "$file" | tr -d ' ')
  allowed=$(allowed_lines "$file")
  if [[ -z "$allowed" ]]; then
    if ((lines > MAX_LINES)); then
      fail "$file has $lines lines (limit $MAX_LINES)"
    fi
  elif ((lines <= MAX_LINES)); then
    fail "$file is down to $lines lines; remove it from the allowlist in $0"
  elif ((lines > allowed)); then
    fail "$file grew to $lines lines (allowlisted at $allowed; limit $MAX_LINES)"
  fi
done < <(git ls-files -z -- '*.rs' '*.py' '*.sh' '*.swift')

while read -r _ path; do
  [[ -n "$path" && ! -f "$path" ]] && fail "$path no longer exists; remove it from the allowlist in $0"
done <<< "$ALLOW_LIST"

if ((failures > 0)); then
  echo
  echo "$failures file-size problem(s). Split oversized files into modules by responsibility."
  exit 1
fi

count=$(printf '%s\n' "$ALLOW_LIST" | awk 'NF { n += 1 } END { print n + 0 }')
echo "All source files are within $MAX_LINES lines ($count allowlisted, none grown)."
