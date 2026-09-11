#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

readonly max_home_bytes=$((50 * 1024))
readonly max_css_bytes=$((25 * 1024))
readonly max_js_bytes=$((20 * 1024))

fail() {
  echo "budget check failed: $*" >&2
  exit 1
}

sum_files() {
  local directory="$1"
  local pattern="$2"
  local total=0
  local file

  if [[ ! -d "$directory" ]]; then
    printf '0\n'
    return
  fi

  while IFS= read -r -d '' file; do
    total=$((total + $(wc -c < "$file")))
  done < <(find "$directory" -type f -name "$pattern" -print0)

  printf '%s\n' "$total"
}

home_document="${RILL3_HOME_BUDGET_FILE:-$repo_root/templates/home.html}"
creator_document="${RILL3_CREATOR_BUDGET_FILE:-$repo_root/templates/creator.html}"
[[ -f "$home_document" ]] || fail "home template not found at $home_document"
[[ -f "$creator_document" ]] || fail "creator template not found at $creator_document"

home_bytes="$(wc -c < "$home_document")"
css_bytes="$(sum_files "$repo_root/static" '*.css')"
js_bytes="$(sum_files "$repo_root/static" '*.js')"

((home_bytes <= max_home_bytes)) || fail "home source is ${home_bytes}B (limit ${max_home_bytes}B)"
((css_bytes <= max_css_bytes)) || fail "common CSS is ${css_bytes}B (limit ${max_css_bytes}B)"
((js_bytes <= max_js_bytes)) || fail "base JavaScript is ${js_bytes}B (limit ${max_js_bytes}B)"

if grep -Eiq '<[[:space:]]*iframe([[:space:]>])' "$home_document"; then
  fail "home template contains an iframe"
fi

creator_iframes="$(grep -Eio '<[[:space:]]*iframe([[:space:]>])' "$creator_document" | wc -l || true)"
((creator_iframes <= 1)) || fail "creator template contains ${creator_iframes} iframes (limit 1)"

grep -Fq 'player-frame--{{ embed_provider_class }}' "$creator_document" \
  || fail "creator iframe is missing its provider-specific viewport class"
grep -Fq 'player-fallback--{{ embed_provider_class }}' "$creator_document" \
  || fail "creator template is missing its provider-specific narrow fallback"
grep -Fq 'width="{{ embed_min_width }}" height="{{ embed_min_height }}"' "$creator_document" \
  || fail "creator iframe is missing explicit provider minimum dimensions"
if grep -Eiq '<script|on(load|resize)[[:space:]]*=' "$creator_document"; then
  fail "creator viewport fallback must not depend on JavaScript"
fi

app_css="$repo_root/static/css/app.css"
grep -Fq '.player-frame--twitch { min-width: 400px; min-height: 300px; }' "$app_css" \
  || fail "Twitch iframe minimum must remain 400x300"
grep -Fq '.player-frame--youtube { min-width: 200px; min-height: 200px; }' "$app_css" \
  || fail "YouTube iframe minimum must remain 200x200"
grep -Fq '@container player-panel (max-width: 400px)' "$app_css" \
  || fail "Twitch iframe is missing its narrow-container fallback"
grep -Fq '@container player-panel (max-width: 200px)' "$app_css" \
  || fail "YouTube iframe is missing its narrow-container fallback"

compact_css="$(tr -d '[:space:]' < "$app_css")"
[[ "$compact_css" == *'@containerplayer-panel(max-width:400px){.player-frame--twitch{display:none;}.link-only.player-fallback--twitch{display:grid;}}'* ]] \
  || fail "Twitch narrow container must show only the external-link fallback"
[[ "$compact_css" == *'@containerplayer-panel(max-width:200px){.player-frame--youtube{display:none;}.link-only.player-fallback--youtube{display:grid;}}'* ]] \
  || fail "YouTube narrow container must show only the external-link fallback"

if grep -ERiq --include='*.html' --include='*.css' \
  'fonts\.(googleapis|gstatic)\.com|use\.typekit\.net|@font-face' \
  "$repo_root/templates" "$repo_root/static"; then
  fail "external or bundled web-font declaration found"
fi

echo "browser budgets: home=${home_bytes}B css=${css_bytes}B js=${js_bytes}B home_iframe=0 creator_iframe=${creator_iframes} webfont=0"
