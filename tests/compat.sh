#!/bin/bash
# Compare shsh à bash sur des extraits : sortie standard + code de retour.
HERE="$(cd "$(dirname "$0")" && pwd)"
SHSH="${SHSH:-$HERE/../target/debug/shsh}"
CASES="$HERE/cases.txt"
pass=0; fail=0
tmp=$(mktemp -d)
cd "$tmp" || exit 1
n=0
while IFS= read -r -d $'\x1e' snippet; do
  snippet="${snippet#$'\n'}"
  [ -z "${snippet//[[:space:]]/}" ] && continue
  n=$((n+1))
  rm -rf "$tmp"/w; mkdir "$tmp"/w; cd "$tmp"/w
  touch a.txt b.txt c.md .cache; mkdir -p d/e
  exp=$(bash -c "$snippet" 2>/dev/null; echo "[rc=$?]")
  cd "$tmp"; rm -rf "$tmp"/w; mkdir "$tmp"/w; cd "$tmp"/w
  touch a.txt b.txt c.md .cache; mkdir -p d/e
  got=$(timeout 5 "$SHSH" -c "$snippet" 2>/dev/null; echo "[rc=$?]")
  if [ "$exp" == "$got" ]; then pass=$((pass+1)); else
    fail=$((fail+1))
    printf '\033[31m✗ cas %d\033[0m\n%s\n--- bash\n%s\n--- shsh\n%s\n\n' "$n" "$snippet" "$exp" "$got"
  fi
done < <(sed 's/^%%$/\x1e/' "$CASES"; printf '\x1e')
rm -rf "$tmp"
printf '\n%d réussis, %d échecs\n' "$pass" "$fail"
[ $fail -eq 0 ]
