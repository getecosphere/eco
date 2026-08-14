#!/bin/bash
# git.sh — Run git commands for the current sibling repo, or prompt if ambiguous.
# Lives in eco/git.sh; run from anywhere.

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="${ECOLOGY_WORKSPACE_ROOT:-$(cd "$SCRIPT_DIR/.." && pwd)}"

BOLD='\033[1m'
CYAN='\033[0;36m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
RESET='\033[0m'

first=""
repo=""
branch=""
changes=""
msg=""
TARGET_REPOS=()
TOOL_REPO="$SCRIPT_DIR"

resolve_target_repos() {
  local found_dirs=()
  local found_names=()
  local d=""
  local bname=""
  local cwd_repo=""
  local cwd_git_root=""
  local cwd_parent=""
  local requested=""

  if [[ -n "$PROJECT_DIR" ]]; then
    requested="$PROJECT_DIR"
    if [[ ! "$requested" = /* ]]; then
      requested="$PROJECT_ROOT/$requested"
    fi
    requested="${requested%/}"
    if [[ ! -d "$requested/.git" ]]; then
      echo -e "${RED}PROJECT_DIR must point to a git repo. Got: $requested${RESET}"
      exit 1
    fi
    TARGET_REPOS=("$requested")
    if [[ "$requested" != "$TOOL_REPO" && -d "$TOOL_REPO/.git" ]]; then
      TARGET_REPOS+=("$TOOL_REPO")
    fi
    return
  fi

  cwd_git_root="$(git -C "$PWD" rev-parse --show-toplevel 2>/dev/null || true)"
  if [[ -n "$cwd_git_root" && "$cwd_git_root" == "$PROJECT_ROOT"* && -d "$cwd_git_root/.git" ]]; then
    cwd_repo="${cwd_git_root%/}"
    cwd_parent="$(dirname "$cwd_repo")"
  fi

  for d in "$PROJECT_ROOT"/*/; do
    bname="$(basename "$d")"
    [[ "$bname" == "core" || "$bname" == "eco" || ! -d "$d/.git" ]] && continue

    d="${d%/}"
    found_dirs+=("$d")
    found_names+=("$bname")
  done

  if [[ -n "$cwd_repo" ]]; then
    if [[ -n "$cwd_parent" ]]; then
      for d in "$cwd_parent"/*/; do
        d="${d%/}"
        [[ ! -d "$d/.git" ]] && continue
        TARGET_REPOS+=("$d")
      done
    fi
    if [[ ${#TARGET_REPOS[@]} -eq 0 ]]; then
      TARGET_REPOS=("$cwd_repo")
    fi
    if [[ "$TOOL_REPO" != "$cwd_repo" && -d "$TOOL_REPO/.git" ]]; then
      local already_added=false
      for d in "${TARGET_REPOS[@]}"; do
        if [[ "$d" == "$TOOL_REPO" ]]; then
          already_added=true
          break
        fi
      done
      $already_added || TARGET_REPOS+=("$TOOL_REPO")
    fi
    return
  fi

  if [[ ${#found_dirs[@]} -eq 0 ]]; then
    if [[ -d "$TOOL_REPO/.git" ]]; then
      TARGET_REPOS=("$TOOL_REPO")
      return
    fi
    echo -e "${RED}No sibling git repositories found under $PROJECT_ROOT.${RESET}"
    exit 1
  elif [[ ${#found_dirs[@]} -eq 1 ]]; then
    TARGET_REPOS=("${found_dirs[0]}")
    if [[ "${found_dirs[0]}" != "$TOOL_REPO" && -d "$TOOL_REPO/.git" ]]; then
      TARGET_REPOS+=("$TOOL_REPO")
    fi
    return
  fi

  TARGET_REPOS=("${found_dirs[@]}")
  if [[ -d "$TOOL_REPO/.git" ]]; then
    TARGET_REPOS+=("$TOOL_REPO")
  fi
}

usage() {
  cat <<EOF
Usage: $(basename "$0") <command> [args...]

Commands:
  status              git status in the resolved repo(s)
  diff                git diff in the resolved repo(s)
  log [opts]          git log (e.g. --oneline -5)
  add <path>          git add <path> in the resolved repo(s)
  commit -m "<msg>"   git add/commit/push in the resolved repo(s)
  pull                git pull in the resolved repo(s)
  push                git push in the resolved repo(s)
  branch              show current branch of the resolved repo(s)
  start <name>        create branch <name> from origin/main in all repos and
                      align composed-domain branch placement in ecompose.yml
  finish <name>       merge <name> back into main in all repos (pulls latest
                      origin/main first, merges, pushes main, and deletes the
                      local + remote feature branch). Pushing main triggers the
                      prod deploy webhook.

Examples:
  $(basename "$0") status
  $(basename "$0") log --oneline -5
  $(basename "$0") add package.json
  $(basename "$0") commit -m "fix: lint"
  $(basename "$0") start feature/social-login
  $(basename "$0") finish feature/social-login
  (cd assessment && ../eco/git.sh commit -m "generate creds")
                     # commits assessment + eco when both have changes
EOF
  exit 1
}

[[ $# -eq 0 ]] && usage

CMD="$1"
shift
resolve_target_repos

run_in_repos() {
  local first=true
  for repo_dir in "${TARGET_REPOS[@]}"; do
    local repo branch
    repo="$(basename "$repo_dir")"
    branch="$(cd "$repo_dir" && git branch --show-current 2>/dev/null || echo "N/A")"
    $first || echo ""
    first=false
    echo -e "${BOLD}${CYAN}━━━ $repo (${branch}) ━━━${RESET}"
    (cd "$repo_dir" && "$@") 2>&1 || echo -e "  ${RED}⚠ failed in $repo${RESET}"
  done
}

# A feature estate must resolve every composed repository from the same feature
# branch. Otherwise staging can pull the bootstrap feature branch but silently
# compose an older domain from main.
sync_feature_branch_manifest() {
  local manifest=""
  local repo_dir=""
  for repo_dir in "${TARGET_REPOS[@]}"; do
    if [[ -f "$repo_dir/ecompose.yml" ]]; then
      manifest="$repo_dir/ecompose.yml"
      break
    fi
  done
  [[ -n "$manifest" ]] || return 0

  node - "$manifest" "$1" <<'NODE'
const fs = require("fs");
const [manifest, branch] = process.argv.slice(2);
let lines = fs.readFileSync(manifest, "utf8").split(/\r?\n/);
let inComposition = false;
for (let i = 0; i < lines.length; i += 1) {
  if (/^composition:\s*$/.test(lines[i])) { inComposition = true; continue; }
  if (inComposition && /^[^\s#]/.test(lines[i])) { inComposition = false; }
  if (inComposition && /^  branch:\s*/.test(lines[i])) {
    lines[i] = `  branch: ${branch}`;
    inComposition = false;
  }
}
const domainsStart = lines.findIndex((line) => /^domains:\s*$/.test(line));
if (domainsStart < 0) process.exit(0);
let end = domainsStart + 1;
while (end < lines.length && !/^[^\s#]/.test(lines[end])) end += 1;
const next = [];
for (let i = domainsStart + 1; i < end;) {
  const match = lines[i].match(/^  - ([^:#\s]+)(?::\s*(.*))?\s*$/);
  if (!match) { next.push(lines[i]); i += 1; continue; }
  const [, domain] = match;
  const nested = [];
  i += 1;
  while (i < end && !/^  - /.test(lines[i])) {
    if (!/^\s*branch:\s*/.test(lines[i]) && !/^      branch:\s*/.test(lines[i])) nested.push(lines[i]);
    i += 1;
  }
  next.push(`  - ${domain}:`, `      branch: ${branch}`, ...nested);
}
lines.splice(domainsStart + 1, end - domainsStart - 1, ...next);
fs.writeFileSync(manifest, lines.join("\n"));
NODE
  echo -e "  ${GREEN}updated ecompose branch placement: $manifest${RESET}"
}

case "$CMD" in
  status)
    run_in_repos git status
    ;;
  diff)
    run_in_repos git diff "$@"
    ;;
  log)
    run_in_repos git log "$@"
    ;;
  add)
    [[ $# -eq 0 ]] && { echo -e "${RED}Missing path${RESET}"; exit 1; }
    run_in_repos git add "$@"
    ;;
  commit)
    msg=""
    if [[ "$1" == "-m" && -n "$2" ]]; then
      msg="$2"
    else
      echo -e "${RED}Usage: $(basename "$0") commit -m <message>${RESET}"
      exit 1
    fi
    first=true
    for repo_dir in "${TARGET_REPOS[@]}"; do
      repo="$(basename "$repo_dir")"
      changes="$(cd "$repo_dir" && git status --porcelain 2>/dev/null)"
      if [[ -n "$changes" ]]; then
        branch="$(cd "$repo_dir" && git branch --show-current 2>/dev/null || echo "N/A")"
        $first || echo ""
        first=false
        echo -e "${BOLD}${CYAN}━━━ $repo (${branch}) ━━━${RESET}"
        (cd "$repo_dir" && git add . && git commit -m "$msg" && git push) 2>&1 || echo -e "  ${RED}⚠ commit/push failed in $repo${RESET}"
      fi
    done
    if $first; then
      echo -e "${YELLOW}No repos have changes.${RESET}"
    fi
    ;;
  pull)
    run_in_repos git pull "$@"
    ;;
  push)
    run_in_repos git push "$@"
    ;;
  branch)
    first=true
    for repo_dir in "${TARGET_REPOS[@]}"; do
      repo="$(basename "$repo_dir")"
      branch="$(cd "$repo_dir" && git branch --show-current 2>/dev/null || echo "N/A")"
      $first || echo ""
      first=false
      echo -e "${CYAN}${repo}${RESET}: ${branch}"
    done
    ;;
  start)
    if [[ $# -eq 0 ]]; then
      echo -e "${RED}Usage: $(basename "$0") start <branch-name>${RESET}"
      exit 1
    fi
    start_name="$1"
    first=true
    failed_any=false
    for repo_dir in "${TARGET_REPOS[@]}"; do
      repo="$(basename "$repo_dir")"
      cur_branch="$(cd "$repo_dir" && git branch --show-current 2>/dev/null || echo "N/A")"
      $first || echo ""
      first=false
      echo -e "${BOLD}${CYAN}━━━ $repo (${cur_branch}) ━━━${RESET}"
      (cd "$repo_dir" \
        && git fetch origin >/dev/null 2>&1 \
        && if git show-ref --verify --quiet "refs/heads/$start_name"; then
             echo -e "  ${YELLOW}branch '$start_name' already exists, checking out${RESET}"
             git checkout "$start_name" 2>&1
           else
             # Create from origin/main but WITHOUT tracking it: `git checkout -b X
             # origin/main` sets upstream to origin/main, so a later `git push`
             # would try to push the feature branch to main. No upstream here means
             # `eco git push` pushes the feature branch to its own remote name.
             git checkout -b "$start_name" origin/main --no-track 2>&1
             echo -e "  ${GREEN}created branch '$start_name' from origin/main${RESET}"
           fi) 2>&1 || { echo -e "  ${RED}⚠ failed in $repo${RESET}"; failed_any=true; }
    done
    if ! $failed_any; then
      sync_feature_branch_manifest "$start_name"
    else
      echo -e "${YELLOW}Manifest branch placement was not updated because a repository branch failed to start.${RESET}"
    fi
    ;;
  finish)
    if [[ $# -eq 0 ]]; then
      echo -e "${RED}Usage: $(basename "$0") finish <branch-name>${RESET}"
      exit 1
    fi
    finish_name="$1"
    first=true
    failed_any=false
    for repo_dir in "${TARGET_REPOS[@]}"; do
      repo="$(basename "$repo_dir")"
      cur_branch="$(cd "$repo_dir" && git branch --show-current 2>/dev/null || echo "N/A")"
      $first || echo ""
      first=false
      echo -e "${BOLD}${CYAN}━━━ $repo (${cur_branch}) ━━━${RESET}"
      # Only finish in repos that actually have this branch; others are skipped.
      if ! (cd "$repo_dir" && git show-ref --verify --quiet "refs/heads/$finish_name"); then
        echo -e "  ${YELLOW}no local branch '$finish_name', skipped${RESET}"
        continue
      fi
      (cd "$repo_dir" && {
        set -e
        git fetch origin 2>&1
        # 1. Merge latest main INTO the feature branch first, so any conflict
        #    surfaces here (on the feature branch) and main is never touched
        #    by a half-finished merge.
        echo "  → merging origin/main into $finish_name"
        git checkout "$finish_name" 2>&1
        if git merge origin/main --no-edit >/dev/null 2>&1; then
          echo -e "  ${GREEN}✓ $finish_name is up to date with main${RESET}"
        else
          echo -e "  ${RED}⚠ conflict merging origin/main into $finish_name${RESET}"
          git merge --abort >/dev/null 2>&1 || true
          echo -e "  ${YELLOW}resolve manually in $repo, then re-run${RESET}"
          exit 1
        fi
        # 2. Move local main to latest origin/main.
        echo "  → updating main"
        git checkout main 2>&1
        git merge --ff-only origin/main 2>&1 || {
          echo -e "  ${YELLOW}local main diverged; fast-forward failed${RESET}"
          exit 1
        }
        # 3. Merge the feature branch into main (no-ff keeps a merge commit so
        #    the feature's history stays identifiable in main).
        echo "  → merging $finish_name into main"
        git merge --no-ff "$finish_name" -m "merge: $finish_name into main" 2>&1
        echo -e "  ${GREEN}✓ merged $finish_name into main${RESET}"
        # 4. Push main (triggers the prod deploy webhook) and remove the
        #    feature branch locally + remotely so finished branches never
        #    clutter the remote.
        echo "  → pushing main"
        git push origin main 2>&1
        echo -e "  ${GREEN}✓ pushed main to origin${RESET}"
        echo "  → removing remote branch $finish_name"
        if git push origin --delete "$finish_name" >/dev/null 2>&1; then
          echo -e "  ${GREEN}✓ removed remote branch $finish_name${RESET}"
        else
          echo -e "  ${YELLOW}no remote branch '$finish_name' to remove${RESET}"
        fi
        if git branch -d "$finish_name" >/dev/null 2>&1; then
          echo -e "  ${GREEN}✓ deleted local branch $finish_name${RESET}"
        fi
      }) 2>&1 || { echo -e "  ${RED}⚠ finish failed in $repo${RESET}"; failed_any=true; }
    done
    if $failed_any; then
      echo -e "${RED}finish completed with failures in some repos.${RESET}"
      exit 1
    fi
    echo -e "${GREEN}Done. main pushed to origin (prod deploy triggered where a webhook is configured) and feature branches removed.${RESET}"
    ;;
  *)
    echo -e "${RED}Unknown command: $CMD${RESET}"
    usage
    ;;
esac
