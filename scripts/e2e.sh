#!/bin/bash
# Runs the whole repair loop on examples/hello with a packaged launcher, without clicking:
# dialogs are answered by FAUST_TEST_ANSWERS, the app's menu is stood in for by
# FAUST_TEST_MESSAGES, and Claude Code by scripts/e2e/fake-claude.sh. Windows of the app and
# the launcher appear for a few seconds in each scenario.
#
#   scripts/e2e.sh [work-dir]          # uses your rustup
#   FRESH=1 scripts/e2e.sh [work-dir]  # installs Rust into the work dir, as on a user's Mac (~540 MB)
#
# 1 setup, then a change whose first build fails; the assistant fixes it; switch
# 2 a change that builds but can't start: back to the previous version
# 3 a change to a protected file is put back
# 4 an official update that merges cleanly with the user's change
# 5 an official update that conflicts: the assistant resolves it
# 6 choosing an older version from the list
# 7 uninstalling (a copy of the app, which ends up in the Trash)
set -euo pipefail
cd "${0%/*}/.."
root="$PWD"
work="${1:-$root/target/e2e}"
rm -rf "$work"; mkdir -p "$work"
work="$(cd "$work" && pwd)"

echo "== build faust and package examples/hello"
cargo build --quiet --features faust-launcher/test-hooks
faust="$root/target/debug/faust"
(cd examples/hello && "$faust" build > "$work/hello-build.log" 2>&1)
"$faust" package --path examples/hello --worktree --out "$work/pkg" > /dev/null
app="$work/pkg/Hello.app"
L="$app/Contents/MacOS/hello"
res="$app/Contents/Resources/faust"

support="$work/Application Support/dev.faust.hello"
state_file="$support/faust/state.json"
plan="$work/fake-plan"; fakelog="$work/fake-claude.log"; : > "$fakelog"

fail() { echo "FAIL: $*"; echo "--- launcher.log"; tail -30 "$support/faust/logs/launcher.log" 2>/dev/null; exit 1; }
state() { /usr/bin/plutil -extract "$1" raw -o - "$state_file" 2>/dev/null || echo "?"; }
version_field() { # <id> <field>
  local n i; n=$(/usr/bin/plutil -extract versions raw -o - "$state_file")
  for ((i = 0; i < n; i++)); do
    if [[ "$(/usr/bin/plutil -extract "versions.$i.id" raw -o - "$state_file")" == "$1" ]]; then
      /usr/bin/plutil -extract "versions.$i.$2" raw -o - "$state_file"; return
    fi
  done
  echo "none"
}
git_() { /usr/bin/git -C "$support/faust/repo" "$@"; }
file_in() { /usr/bin/grep -q -- "$3" "$1/$2" || fail "$2 in $1 does not contain: $3"; }

run_launcher() { # <name> <answers> <messages> <plan, comma-separated> [resources]
  echo "== $1"
  printf '%s' "$4" | tr ',' '\n' > "$plan"
  local t0=$SECONDS
  local system_rust=1
  [[ "${FRESH:-}" == 1 ]] && system_rust=0
  env FAUST_LANG=en FAUST_SUPPORT_DIR="$support" FAUST_RESOURCES="${5:-$res}" FAUST_SYSTEM_RUST="$system_rust" \
    FAUST_ASSISTANT="$root/scripts/e2e/fake-claude.sh" FAKE_PLAN="$plan" FAKE_LOG="$fakelog" \
    FAUST_TEST_ANSWERS="$2" FAUST_TEST_MESSAGES="$3" FAUST_TEST_QUIT_AFTER=10 \
    "$L" 2> "$work/$1.stderr" &
  local pid=$!
  while kill -0 "$pid" 2>/dev/null; do
    if (( SECONDS - t0 > 900 )); then kill "$pid"; fail "$1 did not finish in 15 minutes"; fi
    sleep 1
  done
  wait "$pid" || fail "$1: the launcher exited with $?"
  echo "   $(( SECONDS - t0 )) s, current #$(state current)"
}

make_release() { # <dir> <release> <from>
  rm -rf "$1"; mkdir -p "$1"; cp -R "$3/source" "$1/source"
  printf '{"release":%s,"version":"0.1.0+e2e%s","commit":"","rust":"1.96.1","notes":"An e2e update."}\n' "$2" "$2" > "$1/release.json"
}

# 1 -----------------------------------------------------------------------------
run_launcher setup-and-change "install|start|text:Make the greeting friendlier|0|0" change "break,fix-greeting"
[[ "$(state current)" == 2 ]] || fail "current should be #2"
[[ "$(version_field 1 status)" == good && "$(version_field 2 status)" == good ]] || fail "#1 and #2 should be good"
/usr/bin/grep -q "^fix-greeting .*--resume fake-session" "$fakelog" || fail "the fix round did not resume the assistant's session"
file_in "$support/faust/repo" src/main.rs "Hello, friend!"
[[ "$(git_ rev-parse main)" == "$(version_field 2 commit)" ]] || fail "main should be #2's commit"
git_ log -1 --format=%B main | /usr/bin/grep -q "^Request:" || fail "the commit message lacks the request"

# 2 -----------------------------------------------------------------------------
run_launcher crash-rolls-back "text:Start faster|0|0|0" change "crash"
[[ "$(state current)" == 2 ]] || fail "current should be back on #2"
[[ "$(version_field 3 status)" == bad ]] || fail "#3 should be bad"
[[ ! -d "$support/faust/versions/0003" ]] || fail "#3's folder should be gone"
/usr/bin/grep -q "process::exit(3)" "$support/faust/repo/src/main.rs" && fail "the work tree still has #3's change"
git_ rev-parse --verify -q try/0003 > /dev/null || fail "try/0003 should be kept"

# 3 -----------------------------------------------------------------------------
run_launcher protected-files "text:Add a comment|0|0" change "touch-protected"
[[ "$(state current)" == 4 ]] || fail "current should be #4"
/usr/bin/grep -q "# changed" "$support/faust/versions/0004/faust.toml" && fail "faust.toml should have been put back"
file_in "$support/faust/versions/0004" src/main.rs "// A comment."

# 4 -----------------------------------------------------------------------------
r0="$(/usr/bin/plutil -extract release raw -o - "$res/release.json")"
make_release "$work/res2" $((r0 + 1)) "$res"
/usr/bin/sed -i '' '1s|^//! Hello:|//! Hello (updated):|' "$work/res2/source/src/main.rs"
run_launcher update-merges "0|0" "" "" "$work/res2"
[[ "$(state current)" == 5 && "$(version_field 5 kind)" == update ]] || fail "current should be the update #5"
file_in "$support/faust/versions/0005" src/main.rs "Hello (updated)"
file_in "$support/faust/versions/0005" src/main.rs "Hello, friend!"
[[ "$(git_ rev-list --parents -n 1 main | wc -w | tr -d ' ')" == 3 ]] || fail "main should be a merge commit"

# 5 -----------------------------------------------------------------------------
make_release "$work/res3" $((r0 + 2)) "$work/res2"
/usr/bin/sed -i '' 's/"Hello! Ask me/"Hello, world! Ask me/' "$work/res3/source/src/main.rs"
run_launcher update-conflicts "0|0|0" "" "resolve-ours" "$work/res3"
[[ "$(state current)" == 6 ]] || fail "current should be the resolved update #6"
/usr/bin/grep -q "^resolve-ours" "$fakelog" || fail "the assistant was not asked to resolve"
file_in "$support/faust/versions/0006" src/main.rs "Hello, friend!"
/usr/bin/grep -rq '^<<<<<<< ' "$support/faust/versions/0006/src" && fail "conflict markers left in #6"

# 6 -----------------------------------------------------------------------------
# The list is newest first: #6 #5 #4 #2 #1, so #1 is item 4.
run_launcher choose-older "0:4" versions "" "$work/res3"
[[ "$(state current)" == 1 ]] || fail "current should be #1"
[[ "$(git_ rev-parse main)" == "$(version_field 1 commit)" ]] || fail "main should be #1's commit"
file_in "$support/faust/repo" src/main.rs '"Hello! Ask me'

echo "== git (for people who have it) sees a sound repository"
git_ fsck --no-progress 2>&1 | grep -v "^dangling" || true
[[ -z "$(git_ status --porcelain)" ]] || { git_ status --short | head; fail "git status is not clean"; }
git_ log --oneline --graph --all | head -20

# 7 -----------------------------------------------------------------------------
copy="$work/app copy/Hello.app"
mkdir -p "${copy%/*}"; cp -R "$app" "$copy"
L="$copy/Contents/MacOS/hello"
# #1 lacks the update, so it is offered first: "Later", then uninstall, then OK.
run_launcher uninstall "1|1|0" uninstall "" "$work/res3"
[[ ! -e "$support" ]] || fail "the support folder is still there"
[[ ! -e "$copy" ]] || fail "the app was not moved to the Trash"
echo "PASS"
