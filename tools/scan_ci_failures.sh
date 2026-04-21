#!/usr/bin/env bash
#
# scan_ci_failures.sh - scan GitLab CI integration-test traces for a pattern.
#
# Useful for bisecting when a specific test-failure signature first appeared
# (or stopped) in the integration-test job logs.
#
# Usage:
#   tools/scan_ci_failures.sh <pattern> [max_pipelines] [job_filter]
#
# Args:
#   pattern        ERE regex passed to grep -E (required, quote to protect shell)
#   max_pipelines  how many most-recent pipelines to scan (default: 100)
#   job_filter     job-name substring to include (default: "integration")
#
# Env:
#   OMNISH_GLAB_PROJECT   URL-encoded project path (default: "dev%2Fomnish")
#
# Output: TSV rows (header first), one row per matching job:
#   pipeline_id  created_at  pipe_status  ref  short_sha  commit_subject \
#       job_name  job_status  matches  first_match
#
# Tip: pipe to column -t -s $'\t' for a table view, or sort -k2 for chronology.

set -u

PROJECT="${OMNISH_GLAB_PROJECT:-dev%2Fomnish}"
PATTERN="${1:-}"
MAX="${2:-100}"
JOB_FILTER="${3:-integration}"

if [[ -z "$PATTERN" ]]; then
    echo "usage: $0 <pattern> [max_pipelines] [job_filter]" >&2
    exit 2
fi

command -v glab >/dev/null || { echo "error: glab not in PATH" >&2; exit 1; }
command -v jq   >/dev/null || { echo "error: jq not in PATH"   >&2; exit 1; }

log() { printf '[scan] %s\n' "$*" >&2; }

# --- fetch pipelines (paginated) ------------------------------------------
per_page=$(( MAX < 100 ? MAX : 100 ))
tmp=$(mktemp)
trap 'rm -f "$tmp" "$tmp.jobs" "$tmp.trace"' EXIT

log "fetching up to $MAX pipelines from $PROJECT"
count=0
page=1
while (( count < MAX )); do
    chunk=$(glab api "projects/$PROJECT/pipelines?per_page=$per_page&page=$page&order_by=id&sort=desc" 2>/dev/null || echo '[]')
    chunk_len=$(jq 'length' <<<"$chunk")
    [[ "$chunk_len" -eq 0 ]] && break
    jq -c '.[]' <<<"$chunk" >> "$tmp"
    count=$(( count + chunk_len ))
    (( chunk_len < per_page )) && break
    page=$(( page + 1 ))
done
head -n "$MAX" "$tmp" > "$tmp.head"
mv "$tmp.head" "$tmp"
log "scanning $(wc -l < "$tmp") pipelines"

# --- header ---------------------------------------------------------------
printf 'pipeline_id\tcreated_at\tpipe_status\tref\tshort_sha\tcommit_subject\tjob_name\tjob_status\tmatches\tfirst_match\n'

# --- iterate --------------------------------------------------------------
pidx=0
while IFS= read -r pipe; do
    pidx=$(( pidx + 1 ))
    pid=$(jq -r '.id'         <<<"$pipe")
    sha=$(jq -r '.sha'        <<<"$pipe")
    ref=$(jq -r '.ref'        <<<"$pipe")
    created=$(jq -r '.created_at' <<<"$pipe")
    pstat=$(jq -r '.status'   <<<"$pipe")
    short_sha="${sha:0:10}"

    subject=$(git log -1 --format=%s "$sha" 2>/dev/null | tr '\t' ' ')
    [[ -z "$subject" ]] && subject='(commit not local)'

    log "[$pidx] pipeline=$pid sha=$short_sha $created $pstat"

    glab api "projects/$PROJECT/pipelines/$pid/jobs?per_page=100" 2>/dev/null > "$tmp.jobs" || { echo '[]' > "$tmp.jobs"; }

    # Shortlist matching jobs
    jobs_tsv=$(jq -r --arg f "$JOB_FILTER" '.[] | select(.name | contains($f)) | "\(.id)\t\(.name)\t\(.status)"' "$tmp.jobs")
    [[ -z "$jobs_tsv" ]] && continue

    while IFS=$'\t' read -r jid jname jstat; do
        [[ -z "$jid" ]] && continue
        glab api "projects/$PROJECT/jobs/$jid/trace" 2>/dev/null > "$tmp.trace" || : > "$tmp.trace"

        n=$(grep -cE "$PATTERN" "$tmp.trace" 2>/dev/null || true)
        [[ -z "$n" ]] && n=0
        (( n == 0 )) && continue

        first=$(grep -m1 -E "$PATTERN" "$tmp.trace" | tr -d '\r' | tr '\t' ' ' | cut -c1-200)

        printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
            "$pid" "$created" "$pstat" "$ref" "$short_sha" "$subject" \
            "$jname" "$jstat" "$n" "$first"
    done <<<"$jobs_tsv"
done < "$tmp"

log "done"
