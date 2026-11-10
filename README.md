# shsh — le shadow shell

Shell interactif écrit en Rust pour Shadow OS : compatible POSIX (et une bonne
partie de bash), messages en français, prompt aux couleurs Éclipse.

## Installation

```sh
outils/installer.sh            # compile, installe /usr/local/bin/shsh, l'ajoute à /etc/shells
outils/installer.sh --defaut   # … et en fait le shell de connexion (chsh)
```

Configuration : `~/.shshrc` (syntaxe POSIX). `outils/importer-zshrc.py` reprend
les alias et fonctions d'un `~/.zshrc`.

## Ce qu'il sait faire

**Langage** — pipes, redirections (`2>&1`, `&>`, `<<<`, here-docs), `&&` `||`,
`if/while/until/for/case`, `for ((…))`, fonctions et `local`, `[[ … ]]` avec
`=~`, `(( … ))`, tableaux (`a=(…)`, `${a[@]}`, `${!a[@]}`, creux), toutes les
expansions `${…}` (défaut, motifs, remplacement, casse, sous-chaînes,
indirection), `$(…)`, `$(< fichier)`, `<(…)`, accolades `{a,b}` `{1..10}`,
globbing avec `**` récursif, `time`, `set -euxo pipefail`, `shopt`
(nullglob, dotglob, nocasematch…), `trap`, `getopts`, `mapfile`.

**Interactif** — coloration syntaxique en direct (commande inconnue en rose,
chemins existants soulignés), suggestions grisées tirées de l'historique,
complétion Tab avec descriptions (fichiers, commandes, variables, sous-commandes
et branches git, hôtes ssh, cibles make, options `--…` lues dans les pages de
manuel), recherche Ctrl+R, abréviations (`abbr gs='git status'`), auto-cd,
« vouliez-vous dire … ? », contrôle des tâches complet (Ctrl+Z, `fg`, `bg`,
`jobs`, `kill %n`), saisie multiligne, `!!` / `!$`.

**Prompt Éclipse** — dossier, branche et état git, environnement Python, tâches
en arrière-plan, code de retour, durée des commandes longues, heure ; les
prompts passés se replient en `❯ commande`.

**Crochets** — `shsh_precmd`, `shsh_preexec "$ligne"`, `chpwd`.
`SHSH_HISTORY_IGNORE` (regex) exclut les lignes sensibles de l'historique.

## Tests

```sh
tests/compat.sh          # compare la sortie de shsh à celle de bash sur ~140 extraits
python3 tests/pty_test.py  # pilote un shsh interactif dans un pseudo-terminal
```
