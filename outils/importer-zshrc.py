#!/usr/bin/env python3
"""Reprend les alias et fonctions d'un ~/.zshrc dans un fichier pour shsh.
Chaque fonction est vérifiée avec `shsh -n` ; celles qui utilisent une syntaxe
propre à zsh sont écartées (et listées)."""
import os, re, subprocess, sys

HOME = os.path.expanduser("~")
SHSH = sys.argv[1] if len(sys.argv) > 1 else os.path.join(os.path.dirname(__file__), "../target/release/shsh")
src = open(os.path.join(HOME, ".zshrc"), encoding="utf-8", errors="replace").read().splitlines()

# Crochets/prompt propres à zsh : remplacés par l'équivalent shsh
SKIP_FUNCS = {"zshaddhistory", "preexec", "precmd", "_exec_time_info", "_git_info", "_notify_long_cmd"}
SKIP_ALIASES = {"reload", "zshrc-restore", "zshrc-diff"}

aliases, funcs = [], []
i = 0
while i < len(src):
    line = src[i]
    m = re.match(r"^alias ([^=]+)=", line)
    if m:
        if m.group(1) not in SKIP_ALIASES:
            aliases.append(line)
        i += 1
        continue
    m = re.match(r"^(?:function\s+)?([A-Za-z_][\w-]*)\s*\(\)\s*\{(.*)$", line)
    if m:
        name = m.group(1)
        body = [line]
        if m.group(2).strip().endswith("}"):        # fonction sur une ligne
            i += 1
        else:
            i += 1
            while i < len(src) and not src[i].startswith("}"):
                body.append(src[i]); i += 1
            if i < len(src):
                body.append(src[i]); i += 1
        if name not in SKIP_FUNCS:
            funcs.append((name, "\n".join(body)))
        continue
    i += 1

def fix(code):
    # zsh accepte « … && echo }» ; en POSIX il faut « ; }»
    return re.sub(r"([^;{\s])\s*\}\s*$", r"\1; }", code, flags=re.M) if code.count("\n") == 0 else code

kept, dropped = [], []
for name, code in funcs:
    code = fix(code)
    r = subprocess.run([SHSH, "-n", "-c", code], capture_output=True, text=True)
    (kept if r.returncode == 0 and not r.stderr else dropped).append((name, code, r.stderr.strip()))

out = []
out.append("# ── Repris de ~/.zshrc par outils/importer-zshrc.py ─────────────────────────")
out.append("# Alias\n" + "\n".join(aliases))
out.append("\n# Fonctions")
for name, code, _ in kept:
    out.append(code + "\n")
if dropped:
    out.append("# Fonctions non reprises (syntaxe propre à zsh) : " + ", ".join(n for n, _, _ in dropped))
print("\n".join(out))
print(f"{len(aliases)} alias, {len(kept)} fonctions reprises, {len(dropped)} écartées", file=sys.stderr)
for n, _, e in dropped:
    print(f"  écartée : {n} — {e[:120]}", file=sys.stderr)
