"""Pilote shsh dans un pseudo-terminal : contrôle des tâches, signaux, multiligne."""
import os, pty, sys, time, select, re, signal, tempfile, json, shutil

SHSH = os.path.join(os.path.dirname(os.path.abspath(__file__)), "../target/debug/shsh")
ANSI = re.compile(r"\x1b\[[0-9;?]*[A-Za-z]|\x1b\][^\x07\x1b]*(\x07|\x1b\\)|\x1b[=>]|\r")

ETAT = tempfile.mkdtemp(prefix="shsh-etat-")
SHADOW = os.path.join(ETAT, "shadow-os")

def run():
    pid, fd = pty.fork()
    if pid == 0:
        os.environ["TERM"] = "xterm-256color"
        os.environ["XDG_DATA_HOME"] = os.environ.get("TEST_DATA", "/tmp/shsh-test-data")
        os.environ["XDG_STATE_HOME"] = ETAT
        for v in ("CIBLE", "TARGET", "MISSION", "SHSH_CIBLE_POSEE", "SHSH_SHADOW"):
            os.environ.pop(v, None)
        os.execv(SHSH, [SHSH, "--norc"])
    # taille du terminal
    import fcntl, termios, struct
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 120, 0, 0))
    buf = b""
    def read(t=0.6):
        nonlocal buf
        end = time.time() + t
        while time.time() < end:
            r, _, _ = select.select([fd], [], [], 0.05)
            if r:
                try:
                    d = os.read(fd, 65536)
                except OSError:
                    return
                if not d:
                    return
                buf += d
                # réponse aux requêtes de position du curseur (crossterm)
                if b"\x1b[6n" in d:
                    os.write(fd, b"\x1b[1;1R")
    def send(s, t=0.8):
        os.write(fd, s.encode() if isinstance(s, str) else s)
        read(t)
    def text():
        return ANSI.sub("", buf.decode("utf-8", "replace"))
    def mark():
        nonlocal buf
        buf = b""
    results = []
    def check(name, cond):
        results.append((name, cond))
        print(("\033[32m✓\033[0m " if cond else "\033[31m✗\033[0m ") + name)

    read(1.5)
    check("prompt affiché", "❯" in text())
    mark(); send("echo bonjour-$((6*7))\r")
    check("commande simple", "bonjour-42" in text())

    mark(); send("sleep 30\r", 0.8); send(b"\x1a", 1.0)  # Ctrl+Z
    check("Ctrl+Z stoppe la tâche", "Stoppé" in text())
    mark(); send("jobs\r")
    check("jobs liste la tâche", "sleep 30" in text() and "Stoppé" in text())
    mark(); send("fg\r", 0.8); send(b"\x03", 1.0)  # Ctrl+C
    mark(); send("echo apres-int $?\r")
    check("fg puis Ctrl+C (statut 130)", "apres-int 130" in text())

    mark(); send("sleep 0.3 &\r", 0.5)
    check("tâche de fond annoncée", re.search(r"\[1\] \d+", text()) is not None)
    time.sleep(0.6); mark(); send("\r", 0.8)
    check("fin de tâche de fond notifiée", "Terminé" in text())

    mark(); send(b"\x03", 0.6); send("echo st=$?\r")
    check("Ctrl+C au prompt → $?=130", "st=130" in text())

    mark(); send("if true\r", 0.5); send("then echo multi-ok\r", 0.5); send("fi\r", 1.0)
    check("saisie multiligne (if … fi)", "multi-ok" in text())

    mark(); send("i=0; while true; do i=$((i+1)); done\r", 1.0); send(b"\x03", 1.0); send("echo boucle-$?\r")
    check("Ctrl+C interrompt une boucle interne", "boucle-130" in text())

    mark(); send("cat <<FIN\r", 0.4); send("ligne $((1+1))\r", 0.4); send("FIN\r", 1.0)
    check("here-doc interactif", "ligne 2" in text())

    mark(); send("ech hello\r", 1.2)
    check("suggestion « vouliez-vous dire »", "vouliez-vous dire" in text() and "echo" in text())

    mark(); send("cd /tmp && pwd\r")
    check("cd", "/tmp" in text())
    mark(); send("/usr\r", 0.8); send("pwd\r")
    check("auto-cd en tapant un dossier", "\n/usr" in text())

    mark(); send("vim_absent_xyz 2>/dev/null; echo code=$?\r")
    check("commande introuvable → 127", "code=127" in text())

    mark(); send("(sleep 30; echo x) \r", 0.8); send(b"\x1a", 1.0); send("kill %1\r", 0.8); send("\r", 0.8)
    check("sous-shell suspendu puis tué", "Stoppé" in text())

    # --- Intégration Shadow OS : mission et mode lus dans les fichiers d'état ---
    mark(); send("echo cible=[${CIBLE-}]\r")
    check("sans mission : $CIBLE vide", "cible=[]" in text())
    os.makedirs(os.path.join(SHADOW, "modes"))
    with open(os.path.join(SHADOW, "cible"), "w") as f:
        json.dump({"cible": "10.9.8.7", "nom": "banc", "dossier": "/tmp/banc"}, f, indent=1)
    with open(os.path.join(SHADOW, "modes", "actuel"), "w") as f:
        f.write("pentest\n")
    mark(); send("\r", 0.8)
    check("mission et mode dans le prompt", "10.9.8.7" in text() and "banc" in text() and "Pentest" in text())
    mark(); send("echo $CIBLE $TARGET $MISSION; sh -c 'echo fils=$CIBLE'\r")
    check("$CIBLE, $TARGET, $MISSION posées et exportées", "10.9.8.7 10.9.8.7 /tmp/banc" in text() and "fils=10.9.8.7" in text())
    os.unlink(os.path.join(SHADOW, "cible"))
    mark(); send("echo fin=[${CIBLE-}${MISSION-}]\r")
    check("fin de mission : variables retirées", "fin=[]" in text())
    mark(); send("CIBLE=perso\r", 0.6)
    with open(os.path.join(SHADOW, "cible"), "w") as f:
        json.dump({"cible": "10.9.8.7", "nom": "banc", "dossier": "/tmp/banc"}, f)
    send("echo garde=$CIBLE\r")
    check("une $CIBLE posée à la main est respectée", "garde=perso" in text())
    mark(); send("unset CIBLE; SHSH_SHADOW=0\r", 0.6); mark(); send("echo coupe=[${CIBLE-}]\r")
    check("SHSH_SHADOW=0 coupe l'intégration", "coupe=[]" in text() and "10.9.8.7" not in text())
    mark(); send("SHSH_SHADOW=1\r", 0.6); mark(); send("shadow mo\t", 1.0)
    check("complétion de « shadow »", "modes" in text() and "ambiances" in text())
    send(b"\x03", 0.6)

    # --- Complétions des commandes courantes ---
    mark(); send("apt ins\t", 1.2)
    check("complétion : candidat unique complété directement", "apt install" in text())
    send(b"\x03", 0.5); mark(); send("apt re\t", 1.2)
    check("complétion : sous-commandes d'apt décrites", "remove" in text() and "réinstalle des paquets" in text())
    send(b"\x03", 0.5); mark(); send("systemctl --user status pipew\t", 1.5)
    check("complétion : unités systemd", "pipewire" in text())
    send(b"\x03", 0.5); mark(); send("cargo cl\t", 1.2)
    check("complétion : sous-commandes de cargo", "clippy" in text() and "clean" in text())
    send(b"\x03", 0.5); mark(); send("pkill shs\t", 1.2)
    check("complétion : processus", "shsh" in text())
    send(b"\x03", 0.5)

    # --- Historique en liste (Ctrl+R) et raccourcis ---
    mark(); send("echo motif-rare-42\r", 0.8); mark(); send("motif-rare", 0.5); send(b"\x12", 1.2)
    check("Ctrl+R : retrouve la ligne dans l'historique", "echo motif-rare-42" in text())
    send(b"\x03", 0.5); mark(); send("echo ", 0.4); send(b"\x12", 1.2)
    check("Ctrl+R : liste filtrée (plusieurs résultats)", text().count("echo") >= 3)
    send(b"\x1b", 0.5); send(b"\x03", 0.5)
    mark(); send("id -u", 0.4); send(b"\x1bs", 0.6); send(b"\x03", 0.5)
    check("Alt+S ajoute sudo", "sudo id -u" in text())
    mark(); send("commande_absente_xyz\r", 1.5)
    check("commande introuvable : message clair", "commande introuvable" in text())

    # --- Code de retour dans le prompt ---
    mark(); send("true\r", 0.9)
    check("code de retour : succès affiché", "✔ 0" in text())
    mark(); send("(exit 42)\r", 0.9)
    check("code de retour : échec affiché", "✘ 42" in text())
    mark(); send("ligne abandonnee", 0.4); send(b"\x03", 0.8)
    check("Ctrl+C : code 130 discret", "130 INT" in text())

    mark(); send("exit 7\r", 1.0)
    try:
        _, status = os.waitpid(pid, 0)
        code = os.waitstatus_to_exitcode(status)
    except ChildProcessError:
        code = None
    check(f"exit 7 → code {code}", code == 7)
    shutil.rmtree(ETAT, ignore_errors=True)
    ok = sum(1 for _, c in results if c)
    print(f"\n{ok}/{len(results)} réussis")
    return ok == len(results)

sys.exit(0 if run() else 1)
