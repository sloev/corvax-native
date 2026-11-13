#!/bin/sh
# Copy the CORVAX node bundle, library and logo from npm into shared/corvax/, and shared/ into both apps.
#   ./fetch-corvax.sh [version]   (default: the latest release)
set -e
V=${1:-$(npm view @sloev/corvax version)}
rm -rf shared/corvax && mkdir -p shared/corvax
curl -fsSL "https://registry.npmjs.org/@sloev/corvax/-/corvax-$V.tgz" | tar xz -C shared/corvax --strip-components=1 package/dist package/src package/assets
cp shared/corvax/dist/corvax-node.min.js shared/corvax/
for app in desktop/web/shared android/app/src/main/assets/shared; do
  rm -rf "$app" && cp -r shared "$app" && rm -rf "$app/test"
done
echo "CORVAX $V"
