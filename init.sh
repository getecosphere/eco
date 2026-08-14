#!/bin/bash
# init.sh — Compose a new system by cloning/pulling selected subsystems.
# Lives in eco/init.sh; run from anywhere.

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="${ECOLOGY_WORKSPACE_ROOT:-$(cd "$SCRIPT_DIR/.." && pwd)}"
REPOS_JSON="$SCRIPT_DIR/repos.json"

BOLD='\033[1m'
CYAN='\033[0;36m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
RESET='\033[0m'

# ─── Deps check ──────────────────────────────────────────────────────────────

if ! command -v jq &>/dev/null; then
  echo -e "${RED}jq is required. Install with: brew install jq${RESET}"
  exit 1
fi

if [[ ! -f "$REPOS_JSON" ]]; then
  echo -e "${RED}repos.json not found at $REPOS_JSON${RESET}"
  exit 1
fi

# ─── Load subsystems from repos.json ─────────────────────────────────────────

declare -a sub_name sub_desc sub_git sub_branch sub_requires

while IFS=$'\t' read -r name desc git branch requires; do
  sub_name+=("$name")
  sub_desc+=("$desc")
  sub_git+=("$git")
  sub_branch+=("$branch")
  sub_requires+=("$requires")
done < <(jq -r '.subsystems[] | [.name, .description, .git, .branch, (.requires // [] | join(","))] | @tsv' "$REPOS_JSON")

COUNT=${#sub_name[@]}

# Skip eco (index 0) — orchestrator, not cloned by init
SKIP_INDEX=0

# ─── SSH host alias check ─────────────────────────────────────────────────────

check_ssh_hosts() {
  local seen=""
  for ((i=0; i<COUNT; i++)); do
    [[ $i -eq $SKIP_INDEX ]] && continue
    local url="${sub_git[$i]}"
    # extract host alias from git@<host>:...
    local host="${url%%:*}"
    host="${host#git@}"
    [[ -z "$host" ]] && continue
    # skip if already checked (seen is a colon-delimited list)
    [[ ":$seen:" == *":$host:"* ]] && continue
    seen="$seen:$host"
    if ! ssh -T -o BatchMode=yes -o ConnectTimeout=5 "git@$host" 2>&1 | grep -qiE "success|authenticated|welcome"; then
      # also accept a clean "permission denied" — that means host resolved but auth may differ
      if ! ssh -T -o BatchMode=yes -o ConnectTimeout=5 "git@$host" 2>&1 | grep -qiE "denied|repository"; then
        echo -e "  ${YELLOW}⚠ SSH host alias '${host}' unreachable or not in ~/.ssh/config${RESET}"
        echo -e "    Add an entry like:"
        echo -e "      Host ${host}"
        echo -e "        HostName github.com"
        echo -e "        User git"
        echo -e "        IdentityFile ~/.ssh/your_key"
        echo ""
        read -p "    Continue anyway? [y/N]: " ssh_cont
        ssh_cont="${ssh_cont:-n}"
        [[ "$ssh_cont" =~ ^[Yy] ]] || exit 1
      fi
    fi
  done
}

# ─── Checkbox helpers ─────────────────────────────────────────────────────────

declare -a checked locked
for ((i=0; i<COUNT; i++)); do
  checked[$i]=0
  locked[$i]=0
done

index_of() {
  local target="$1"
  for ((i=0; i<COUNT; i++)); do
    [[ "${sub_name[$i]}" == "$target" ]] && echo $i && return
  done
  echo -1
}

recompute_locks() {
  for ((i=0; i<COUNT; i++)); do locked[$i]=0; done
  for ((i=0; i<COUNT; i++)); do
    [[ "${checked[$i]}" -eq 0 ]] && continue
    [[ -z "${sub_requires[$i]}" ]] && continue
    IFS=',' read -ra deps <<< "${sub_requires[$i]}"
    for dep in "${deps[@]}"; do
      local dep_idx
      dep_idx=$(index_of "$dep")
      if [[ $dep_idx -ge 0 ]]; then
        checked[$dep_idx]=1
        locked[$dep_idx]=1
      fi
    done
  done
}

render_menu() {
  local cursor="$1"
  for ((i=0; i<COUNT; i++)); do
    [[ $i -eq $SKIP_INDEX ]] && continue
    local box dep_label=""
    if [[ "${checked[$i]}" -eq 1 ]]; then
      box="${GREEN}[x]${RESET}"
    else
      box="[ ]"
    fi
    if [[ "${locked[$i]}" -eq 1 ]]; then
      dep_label=" ${YELLOW}(required by dependency)${RESET}"
    fi
    local line="  ${box} ${sub_name[$i]} — ${sub_desc[$i]}${dep_label}"
    if [[ $i -eq $cursor ]]; then
      echo -e "  ${CYAN}❯${RESET}${line:2}"
    else
      echo -e "$line"
    fi
  done
}

declare -a selectable
for ((i=0; i<COUNT; i++)); do
  [[ $i -ne $SKIP_INDEX ]] && selectable+=($i)
done
SEL_COUNT=${#selectable[@]}

# ─── Header ───────────────────────────────────────────────────────────────────

echo ""
echo -e "${BOLD}========================================${RESET}"
echo -e "${BOLD}  System Composer${RESET}"
echo -e "${BOLD}========================================${RESET}"
echo ""

# ─── Project name ─────────────────────────────────────────────────────────────

read -p "Project name: " input_name
if [[ -z "$input_name" ]]; then
  echo -e "${RED}Project name is required.${RESET}"
  exit 1
fi
PROJECT_NAME="$input_name"

# ─── Branch override ──────────────────────────────────────────────────────────

echo ""
read -p "Use default branches from repos.json? [Y/n]: " use_default_branches
use_default_branches="${use_default_branches:-y}"

declare -a final_branch
for ((i=0; i<COUNT; i++)); do
  final_branch[$i]="${sub_branch[$i]}"
done

if [[ ! "$use_default_branches" =~ ^[Yy] ]]; then
  echo ""
  echo -e "  Enter branch for each subsystem (leave blank to keep default):"
  for ((i=0; i<COUNT; i++)); do
    [[ $i -eq $SKIP_INDEX ]] && continue
    read -p "    ${sub_name[$i]} [${sub_branch[$i]}]: " branch_input
    [[ -n "$branch_input" ]] && final_branch[$i]="$branch_input"
  done
fi

# ─── SSH check ────────────────────────────────────────────────────────────────

echo ""
echo -e "${BOLD}Checking SSH host aliases...${RESET}"
check_ssh_hosts
echo -e "  ${GREEN}✓ SSH hosts OK${RESET}"

# ─── Subsystem selection ──────────────────────────────────────────────────────

echo ""
echo -e "  ${BOLD}Select subsystems to clone/pull:${RESET}"
echo -e "  (↑↓ move, Space toggle, Enter confirm)"
echo ""

cursor_sel=0
render_menu "${selectable[$cursor_sel]}"

while true; do
  local_key=""
  IFS= read -s -n1 local_key 2>/dev/null
  if [[ "$local_key" == $'\033' ]]; then
    local_rest=""
    read -s -n2 local_rest 2>/dev/null
    local_key="$local_key$local_rest"
  fi

  case "$local_key" in
    $'\033[A')
      [[ $cursor_sel -gt 0 ]] && ((cursor_sel--))
      ;;
    $'\033[B')
      [[ $cursor_sel -lt $((SEL_COUNT - 1)) ]] && ((cursor_sel++))
      ;;
    " ")
      idx="${selectable[$cursor_sel]}"
      if [[ "${locked[$idx]}" -eq 1 ]]; then
        :
      elif [[ "${checked[$idx]}" -eq 1 ]]; then
        checked[$idx]=0
      else
        checked[$idx]=1
      fi
      recompute_locks
      ;;
    "")
      break
      ;;
  esac

  echo -en "\033[${SEL_COUNT}A"
  render_menu "${selectable[$cursor_sel]}"
done

echo ""

# ─── Collect selections ───────────────────────────────────────────────────────

declare -a to_clone
for ((i=0; i<COUNT; i++)); do
  [[ $i -eq $SKIP_INDEX ]] && continue
  [[ "${checked[$i]}" -eq 1 ]] && to_clone+=($i)
done

if [[ ${#to_clone[@]} -eq 0 ]]; then
  echo -e "${YELLOW}Nothing selected. Exiting.${RESET}"
  exit 0
fi

echo -e "${BOLD}Will clone/pull:${RESET}"
for idx in "${to_clone[@]}"; do
  echo -e "  ${CYAN}${sub_name[$idx]}${RESET} (${final_branch[$idx]}) — ${sub_git[$idx]}"
done
echo ""
read -p "Proceed? [Y/n]: " confirm
confirm="${confirm:-y}"
[[ "$confirm" =~ ^[Yy] ]] || { echo "Aborted."; exit 0; }

# ─── Clone or pull ────────────────────────────────────────────────────────────

PROJECT_DIR="$PROJECT_ROOT/$PROJECT_NAME"
mkdir -p "$PROJECT_DIR/index"

echo ""
for idx in "${to_clone[@]}"; do
  name="${sub_name[$idx]}"
  git_url="${sub_git[$idx]}"
  branch="${final_branch[$idx]}"
  target="$PROJECT_DIR/$name"

  echo -e "${BOLD}${CYAN}━━━ $name ━━━${RESET}"

  if [[ -d "$target/.git" ]]; then
    echo -e "  Already cloned — pulling ${branch}..."
    (cd "$target" && git checkout "$branch" && git pull) \
      && echo -e "  ${GREEN}✓ up to date${RESET}" \
      || echo -e "  ${RED}⚠ pull failed${RESET}"
  else
    echo -e "  Cloning into $target..."
    git clone --branch "$branch" "$git_url" "$target" \
      && echo -e "  ${GREEN}✓ cloned${RESET}" \
      || echo -e "  ${RED}⚠ clone failed${RESET}"
  fi
  echo ""
done


# ─── Offer to run configure.sh ────────────────────────────────────────────────

echo -e "${GREEN}${BOLD}Done.${RESET}"
echo ""
read -p "Run configure.sh now to wire up .env and PM2? [Y/n]: " run_configure
run_configure="${run_configure:-y}"
if [[ "$run_configure" =~ ^[Yy] ]]; then
  PROJECT_NAME="$PROJECT_NAME" PROJECT_DIR="$PROJECT_DIR" bash "$SCRIPT_DIR/configure.sh"
else
  echo -e "Run ${CYAN}bash eco/configure.sh${RESET} when ready."
fi
echo ""
