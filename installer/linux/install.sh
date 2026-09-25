#!/bin/sh
# Installs OpenAtelier for you (no root needed) from this unpacked folder:
#
#   ./install.sh              install (or update) into ~/.local
#   ./install.sh --uninstall  remove it again (your projects and settings stay)
#
# The program goes to ~/.local/opt/openatelier, a link to ~/.local/bin/openatelier, and
# a menu entry with its icon to ~/.local/share. The app can update itself there.
# (On Debian, Ubuntu and relatives, the .deb from the same release installs it for
# everyone instead, and brings ffmpeg with it.)
set -eu

here=$(cd "$(dirname "$0")" && pwd)
data=${XDG_DATA_HOME:-$HOME/.local/share}
app=$HOME/.local/opt/openatelier
bin=$HOME/.local/bin
desktop=$data/applications/openatelier.desktop
icon=$data/icons/hicolor/256x256/apps/openatelier.png

if [ "${1:-}" = "--uninstall" ]; then
    rm -rf "$app"
    rm -f "$bin/openatelier" "$bin/oa" "$desktop" "$icon"
    command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$data/applications" || true
    echo "OpenAtelier is uninstalled. Your projects, settings and downloads (in $data/OpenAtelier) are still there."
    exit 0
fi

if [ ! -x "$here/openatelier" ]; then
    echo "Run this from the unpacked OpenAtelier folder (it has 'openatelier' in it)." >&2
    exit 1
fi

mkdir -p "$app" "$bin" "$(dirname "$desktop")" "$(dirname "$icon")"
# The folder itself, whatever was there before replaced (an update).
if [ "$here" != "$app" ]; then
    rm -rf "$app"
    cp -R "$here" "$app"
fi
ln -sf "$app/openatelier" "$bin/openatelier"
ln -sf "$app/oa" "$bin/oa"
cp "$here/openatelier.png" "$icon"
sed "s|^Exec=openatelier|Exec=\"$app/openatelier\"|" "$here/openatelier.desktop" > "$desktop"
command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$data/applications" || true
command -v gtk-update-icon-cache >/dev/null 2>&1 && gtk-update-icon-cache -q "$data/icons/hicolor" 2>/dev/null || true

echo "OpenAtelier is installed: find it in your applications menu, or run 'openatelier'."
case ":$PATH:" in
    *":$bin:"*) ;;
    *) echo "(Add $bin to your PATH to run it from a terminal by name.)" ;;
esac
if ! command -v ffmpeg >/dev/null 2>&1 || ! command -v ffprobe >/dev/null 2>&1; then
    echo
    echo "It needs ffmpeg to read and write video. Install it with your package manager:"
    echo "  sudo apt install ffmpeg      (Debian, Ubuntu)"
    echo "  sudo dnf install ffmpeg      (Fedora, with RPM Fusion)"
    echo "  sudo pacman -S ffmpeg        (Arch)"
fi
