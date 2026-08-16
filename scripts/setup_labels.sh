#!/usr/bin/env sh
# piCoreCDSP v2 — Create GitHub labels for upstream monitoring (roadmap §71, Gate 14)
#
# Run once after repository creation:
#   GITHUB_TOKEN=<token> ./scripts/setup_labels.sh owner/repo
#
# Requires: gh (GitHub CLI) authenticated as a repo admin.
# Idempotent — existing labels are updated if color/description differs.

set -eu

REPO="${1:-}"
if [ -z "$REPO" ]; then
  echo "Usage: $0 owner/repo" >&2
  exit 1
fi

create_or_update_label() {
  _name="$1"; _color="$2"; _desc="$3"
  if gh label list --repo "$REPO" --json name --jq '.[].name' \
     2>/dev/null | grep -qx "$_name"; then
    gh label edit "$_name" \
      --repo        "$REPO" \
      --color       "$_color" \
      --description "$_desc" \
      2>/dev/null || true
    echo "  updated: $_name"
  else
    gh label create "$_name" \
      --repo        "$REPO" \
      --color       "$_color" \
      --description "$_desc" \
      2>/dev/null || true
    echo "  created: $_name"
  fi
}

echo "Setting up upstream-monitoring labels on $REPO ..."

# Category: upstream source area
create_or_update_label "upstream"              "0075ca" "Relates to an upstream dependency"
create_or_update_label "upstream:camilladsp"   "0366d6" "Upstream: CamillaDSP engine"
create_or_update_label "upstream:camillagui"   "1d76db" "Upstream: CamillaGUI (frontend/backend)"
create_or_update_label "upstream:alsa"         "0099cc" "Upstream: ALSA / alsa-lib"
create_or_update_label "upstream:kernel"       "004080" "Upstream: Linux kernel / snd-aloop"
create_or_update_label "upstream:pcp"          "003366" "Upstream: piCorePlayer platform"

# Category: capability / workaround tracking
create_or_update_label "capability"            "e4e669" "Relates to a tracked upstream capability"
create_or_update_label "removal-candidate"     "d93f0b" "Local workaround that may now be removable"

# Category: signal severity
create_or_update_label "breaking-change"       "b60205" "Upstream change that may break piCoreCDSP"
create_or_update_label "canary-failure"        "e99695" "Upstream canary CI probe failed"
create_or_update_label "release-candidate"     "0e8a16" "Upstream project has a new release candidate"

echo "Done."
