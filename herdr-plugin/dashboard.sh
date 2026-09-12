#!/bin/sh
# Optional launcher: the ordinary client TUI runs in a normal Herdr tab.
set -eu
command -v ssf >/dev/null 2>&1 || {
    echo 'Install the ssf client, then run ssf dashboard in a terminal.' >&2
    exit 1
}
command -v jq >/dev/null 2>&1 || {
    echo 'This optional launcher needs jq; run ssf dashboard directly instead.' >&2
    exit 1
}
# A tab inherits server context; explicitly carry an optional SSF remote route.
result=$(herdr tab create --label 'SSF dashboard' --focus --env "SSF_SERVER=${SSF_SERVER:-}")
pane=$(printf '%s' "$result" | jq -er '.result.root_pane.pane_id')
herdr pane run "$pane" 'ssf dashboard' >/dev/null
