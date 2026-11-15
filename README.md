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
`=~` et `BASH_REMATCH`, `(( … ))`, tableaux (`a=(…)`, `${a[@]}`, `${!a[@]}`,
creux), tableaux associatifs (`declare -A`), entiers (`declare -i`), `case`
avec `;&` et `;;&`, `${x@Q}` `${x@U}`, `${!préfixe*}`, `{fd}>fichier`, `$LINENO`, toutes les
expansions `${…}` (défaut, motifs, remplacement, casse, sous-chaînes,
indirection), `$(…)`, `$(< fichier)`, `<(…)`, accolades `{a,b}` `{1..10}`,
globbing avec `**` récursif, `time`, `set -euxo pipefail`, `shopt`
(nullglob, dotglob, nocasematch…), `trap`, `getopts`, `mapfile`.

**Interactif** — coloration syntaxique en direct (commande inconnue en rose,
chemins existants soulignés), suggestions grisées tirées de l'historique,
complétion Tab avec descriptions (fichiers, commandes, variables, sous-commandes
et branches git, hôtes ssh, cibles make, paquets apt, unités systemd, processus
pour `kill`/`pkill`, sous-commandes de docker, cargo, npm, pip, tmux, snap et
flatpak, scripts `npm run`, options `--…` lues dans les pages de manuel), historique en liste filtrée (Ctrl+R), abréviations (`abbr gs='git status'`), auto-cd,
« vouliez-vous dire … ? » (ou le paquet apt qui fournit la commande absente), contrôle des tâches complet (Ctrl+Z, `fg`, `bg`,
`jobs`, `kill %n`), saisie multiligne, `!!` / `!$`, Alt+E pour retravailler la ligne dans `$EDITOR`, Alt+S pour ajouter `sudo`.

**Prompt Éclipse** — dossier (relatif au dépôt git), branche, état et avance/retard sur le distant, environnement Python, tâches
en arrière-plan, code de retour toujours affiché (`✔ 0`, `✘ 137 KILL`), durée des commandes longues, heure ; les
prompts passés se replient en `❯ commande`.

**Shadow OS** — shsh lit directement l'état du bureau, sans rien ajouter à
`~/.shshrc` : la cible de la mission en cours (`shadow cible 10.10.11.5 nom`)
s'affiche dans le prompt et `$CIBLE`, `$TARGET`, `$MISSION` sont posées dans
tous les terminaux ouverts, puis retirées à `shadow cible fin` ; le mode du
bureau (Pentest, Focus, Jeu, Nuit) apparaît à droite. Une `$CIBLE` posée à la
main n'est jamais écrasée. Tab complète `shadow …` avec ses sous-commandes et
les modes installés. `SHSH_SHADOW=0` coupe le tout.

**Journal** — chaque commande tapée est notée avec son heure, son dossier, son
code de retour et sa durée (`~/.local/share/shsh/journal.tsv`). `journal` affiche
les dernières ; `journal ici`, `journal echecs`, `journal lentes`,
`journal cherche MOTIF`, `journal aujourdhui` filtrent. Pendant une mission
Shadow OS, les mêmes lignes vont aussi dans `$MISSION/journal.md`, que
`shadow rapport` transforme en rapport. Les lignes exclues de l'historique
(espace en tête, `SHSH_HISTORY_IGNORE`) ne sont jamais écrites ; `SHSH_JOURNAL=0`
coupe tout.

**Crochets** — `shsh_precmd`, `shsh_preexec "$ligne"`, `chpwd`.
`SHSH_HISTORY_IGNORE` (regex) exclut les lignes sensibles de l'historique.

## Tests

```sh
tests/compat.sh          # compare la sortie de shsh à celle de bash sur ~160 extraits
python3 tests/pty_test.py  # pilote un shsh interactif dans un pseudo-terminal
```
