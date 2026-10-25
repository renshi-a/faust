#!/bin/bash
# A stand-in for Claude Code in scripts/e2e.sh. It answers --version, reads the prompt on
# stdin, makes the change named by the next line of $FAKE_PLAN in the current directory (the
# user's copy of examples/hello), and prints a result event like `claude -p --output-format
# stream-json` does.
set -euo pipefail
if [[ "${1:-}" == "--version" ]]; then echo "9.9.9 (Claude Code)"; exit 0; fi
if [[ "${1:-}" == "--help" ]]; then echo "  --restricted    Restricted mode"; exit 0; fi
cat > /dev/null
mode="$(/usr/bin/head -1 "$FAKE_PLAN")"
/usr/bin/tail -n +2 "$FAKE_PLAN" > "$FAKE_PLAN.tmp" && /bin/mv "$FAKE_PLAN.tmp" "$FAKE_PLAN"
echo "$mode | $*" >> "$FAKE_LOG"
case "$mode" in
  break)
    printf '\nfn broken( {\n' >> src/main.rs
    msg="Added a function." ;;
  fix-greeting)
    /usr/bin/sed -i '' -e '$d' src/main.rs
    /usr/bin/sed -i '' -e '$d' src/main.rs
    /usr/bin/sed -i '' 's/"Hello! Ask me/"Hello, friend! Ask me/' src/main.rs
    msg="Removed the broken function and made the greeting friendlier." ;;
  crash)
    /usr/bin/sed -i '' 's/^    load_appkit();$/    std::process::exit(3);/' src/main.rs
    msg="Made it start faster." ;;
  touch-protected)
    echo "# changed" >> faust.toml
    printf '\n// A comment.\n' >> src/main.rs
    msg="Changed faust.toml and added a comment." ;;
  resolve-ours)
    # Keep "yours" in every conflict block (merge or diff3 style).
    for f in $(/usr/bin/grep -rl '^<<<<<<< ' src 2>/dev/null); do
      /usr/bin/awk '/^<<<<<<< /{s=1;next} /^\|\|\|\|\|\|\| /{if(s){s=2;next}} /^=======$/{if(s){s=3;next}} /^>>>>>>> /{if(s){s=0;next}} s==0||s==1' "$f" > "$f.tmp" && /bin/mv "$f.tmp" "$f"
    done
    msg="Kept your version where they conflicted." ;;
  *) msg="Changed nothing." ;;
esac
printf '{"type":"result","subtype":"success","is_error":false,"result":"%s","session_id":"fake-session"}\n' "$msg"
