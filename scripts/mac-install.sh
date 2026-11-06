#!/bin/sh
# Build Slipmat on a Mac and put a Slipmat app in ~/Applications.
# Run from the slipmat folder:  sh scripts/mac-install.sh
set -e
cd "$(dirname "$0")/.."

if ! xcode-select -p >/dev/null 2>&1; then
    echo "First install Apple's command line tools: xcode-select --install"
    echo "(a window opens; when it finishes, run this again)"
    exit 1
fi
if ! command -v cargo >/dev/null 2>&1; then
    if [ -x "$HOME/.cargo/bin/cargo" ]; then
        PATH="$HOME/.cargo/bin:$PATH"
    else
        echo "Installing Rust (rustup.rs)…"
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
        PATH="$HOME/.cargo/bin:$PATH"
    fi
fi

cargo install --path . --locked

# A tiny app wrapper so Slipmat starts from Launchpad or the Dock. It starts
# the server and opens the decks in the browser; Settings → Quit stops it.
app="$HOME/Applications/Slipmat.app"
mkdir -p "$app/Contents/MacOS"
cat > "$app/Contents/MacOS/Slipmat" <<SCRIPT
#!/bin/sh
exec "$HOME/.cargo/bin/slipmat"
SCRIPT
chmod +x "$app/Contents/MacOS/Slipmat"
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleName</key><string>Slipmat</string>
  <key>CFBundleIdentifier</key><string>com.jaspermeller.slipmat</string>
  <key>CFBundleExecutable</key><string>Slipmat</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>LSUIElement</key><true/>
</dict></plist>
PLIST
echo ""
echo "Slipmat is installed. Open it from Launchpad (or run: slipmat)."
