#!/bin/sh
# Compile et installe shsh dans /usr/local/bin, l'enregistre dans /etc/shells.
# Usage : outils/installer.sh [--defaut]   (--defaut : en fait aussi le shell de connexion)
set -e
cd "$(dirname "$0")/.."
cargo build --release
sudo install -m 755 target/release/shsh /usr/local/bin/shsh
if ! grep -qx /usr/local/bin/shsh /etc/shells; then
    echo /usr/local/bin/shsh | sudo tee -a /etc/shells >/dev/null
fi
if [ "$1" = "--defaut" ]; then
    sudo chsh -s /usr/local/bin/shsh "$(id -un)"
fi
echo "shsh $(/usr/local/bin/shsh --version | cut -d' ' -f2) installé dans /usr/local/bin/shsh"
