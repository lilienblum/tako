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
  commits="$(git log --no-merges --pretty='- %s (%h)' "$commit_range" || true)"
else
  commit_range="full history"
  commits="$(git log --no-merges --pretty='- %s (%h)' || true)"
fi

if [ -z "$commits" ]; then
  commits="- No commits found."
fi

generated_at="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"
mkdir -p "$(dirname "$output")"

{
  printf '# %s Release Notes Draft\n\n' "$component"
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
  printf '\n## Commits\n\n'
  printf '%s\n' "$commits"
  printf '\n## AI Prompt\n\n'
  printf 'Use the commit list above to draft GitHub release notes for `%s`.\n\n' "$component"
  printf 'Return:\n'
  printf '1. Recommended semver bump (`patch`, `minor`, or `major`) with short rationale.\n'
  printf '2. Release title and one-paragraph summary.\n'
  printf '3. Categorized sections: Features, Fixes, Docs, Refactors, Chore.\n'
  printf '4. Breaking changes section (or `None`).\n'
  printf '5. Upgrade notes section (or `None`).\n'
} > "$output"

echo "Wrote $output"
