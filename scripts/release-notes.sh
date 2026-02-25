#!/bin/sh
set -eu

usage() {
  cat <<'EOF'
Usage:
  sh scripts/release-notes.sh --component <name> --prefix <tag-prefix> --output <path> [--series <series>]

Examples:
  sh scripts/release-notes.sh --component "tako" --prefix "tako-v" --output "dist/release-notes/tako.md"
  sh scripts/release-notes.sh --component "tako" --prefix "tako-v" --series "0.1.x" --output "dist/release-notes/tako-0.1.md"
EOF
}

require_value() {
  if [ "$#" -lt 2 ]; then
    echo "error: missing value for $1" >&2
    usage >&2
    exit 1
  fi
}

component=""
prefix=""
series=""
output=""

while [ "$#" -gt 0 ]; do
  case "$1" in
    --component)
      require_value "$@"
      component="$2"
      shift 2
      ;;
    --prefix)
      require_value "$@"
      prefix="$2"
      shift 2
      ;;
    --series)
      require_value "$@"
      series="$2"
      shift 2
      ;;
    --output)
      require_value "$@"
      output="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "error: unknown option '$1'" >&2
      usage >&2
      exit 1
      ;;
  esac
done

if [ -z "$component" ] || [ -z "$prefix" ] || [ -z "$output" ]; then
  echo "error: --component, --prefix, and --output are required" >&2
  usage >&2
  exit 1
fi

if ! git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  echo "error: not inside a git repository" >&2
  exit 1
fi

series_pattern=""
if [ -n "$series" ]; then
  series_pattern="$series"
  case "$series_pattern" in
    *.x)
      series_pattern="${series_pattern%.x}."
      ;;
  esac
fi

tag_pattern="${prefix}*"
if [ -n "$series_pattern" ]; then
  tag_pattern="${prefix}${series_pattern}*"
fi

last_tag="$(git tag --list "$tag_pattern" --sort=-v:refname | head -n 1 || true)"

if [ -n "$last_tag" ]; then
  commit_range="$last_tag..HEAD"
  git log --no-merges --pretty='%h%x09%s' "$commit_range" > /tmp/tako-release-notes-commits.$$ 2>/dev/null || true
else
  commit_range="full history"
  git log --no-merges --pretty='%h%x09%s' > /tmp/tako-release-notes-commits.$$ 2>/dev/null || true
fi

commit_file="/tmp/tako-release-notes-commits.$$"
trap 'rm -f "$commit_file"' EXIT INT TERM

if [ ! -s "$commit_file" ]; then
  commit_sections='## no-changes

- No commits found.
'
else
  commit_sections="$(
    awk '
      BEGIN {
        split("feat fix perf refactor docs test chore ci build style revert other", order, " ")
      }
      {
        hash=$1
        $1=""
        sub(/^\t/, "", $0)
        subject=$0

        type="other"
        header=subject
        sub(/: .*/, "", header)
        if (header ~ /^[a-z0-9]+(\([^)]+\))?(!)?$/) {
          type=header
          sub(/\(.*/, "", type)
          sub(/!$/, "", type)
        }

        entries[type]=entries[type] "- " subject " (" hash ")\n"
      }
      END {
        any=0
        for (i=1; i<=length(order); i++) {
          t=order[i]
          if (entries[t] != "") {
            printf "## %s\n\n%s\n", t, entries[t]
            any=1
          }
        }
        if (!any) {
          printf "## no-changes\n\n- No commits found.\n\n"
        }
      }
    ' "$commit_file"
  )"
fi

generated_at="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"
mkdir -p "$(dirname "$output")"

{
  printf '# %s Release Notes\n\n' "$component"
  printf -- '- Generated: %s\n' "$generated_at"
  printf -- '- Tag prefix: `%s`\n' "$prefix"
  if [ -n "$series" ]; then
    printf -- '- Series filter: `%s`\n' "$series"
  fi
  if [ -n "$last_tag" ]; then
    printf -- '- Baseline tag: `%s`\n' "$last_tag"
    printf -- '- Commit range: `%s`\n' "$last_tag..HEAD"
  else
    printf -- '- Baseline tag: `(none found for pattern %s)`\n' "$tag_pattern"
    printf -- '- Commit range: `%s`\n' "$commit_range"
  fi
  printf '\n%s' "$commit_sections"
} > "$output"

echo "Wrote $output"
