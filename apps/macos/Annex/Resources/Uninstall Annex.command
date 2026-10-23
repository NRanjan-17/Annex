#!/bin/bash
# Removes Annex and everything it stores on this Mac: the app, its identity key (Keychain),
# trusted devices, preferences, caches and its privacy (TCC) permissions.
set -u
BUNDLE_ID="com.nranjan-17.annex"

echo "This will remove Annex, its settings, its identity key and the list of trusted devices."
read -r -p "Continue? [y/N] " answer
[[ "$answer" =~ ^[Yy]$ ]] || { echo "Cancelled."; exit 0; }

osascript -e 'tell application id "'"$BUNDLE_ID"'" to quit' >/dev/null 2>&1 || true
pkill -x Annex >/dev/null 2>&1 || true
sleep 1

# Every copy of the app Spotlight knows about, plus the usual location.
apps=$(mdfind "kMDItemCFBundleIdentifier == '$BUNDLE_ID'" 2>/dev/null | grep -E '\.app$' || true)
for app in "/Applications/Annex.app" $apps; do
  [ -d "$app" ] || continue
  case "$app" in /Volumes/*) continue ;; esac  # never touch the mounted installer image
  echo "Removing $app"
  rm -rf "$app" 2>/dev/null || sudo rm -rf "$app"
done

for path in \
  "$HOME/Library/Application Support/Annex" \
  "$HOME/Library/Preferences/$BUNDLE_ID.plist" \
  "$HOME/Library/Caches/$BUNDLE_ID" \
  "$HOME/Library/HTTPStorages/$BUNDLE_ID" \
  "$HOME/Library/Saved Application State/$BUNDLE_ID.savedState" \
  "$HOME/Library/Logs/Annex"; do
  if [ -e "$path" ]; then
    echo "Removing $path"
    rm -rf "$path"
  fi
done
defaults delete "$BUNDLE_ID" >/dev/null 2>&1 || true

# The identity private key(s) live in the login Keychain.
removed_keys=0
while security delete-generic-password -s "$BUNDLE_ID" >/dev/null 2>&1; do
  removed_keys=$((removed_keys + 1))
done
[ "$removed_keys" -gt 0 ] && echo "Removed $removed_keys identity key(s) from the Keychain"

# Forget Screen Recording / Accessibility / Local Network decisions for Annex.
tccutil reset All "$BUNDLE_ID" >/dev/null 2>&1 || true

echo "Annex has been removed."
