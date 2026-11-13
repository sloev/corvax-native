#!/bin/sh
# Copy the CORVAX node bundle, library and logo from npm into shared/corvax/, and shared/ into both apps.
set -e
V=${1:-0.1}
rm -rf shared/corvax && mkdir -p shared/corvax
curl -fsSL "https://registry.npmjs.org/@sloev/corvax/-/corvax-$(npm view @sloev/corvax@$V version).tgz" | tar xz -C shared/corvax --strip-components=1 package/dist package/src package/assets
cp shared/corvax/dist/corvax-node.min.js shared/corvax/
rm -rf desktop/web/shared android/app/src/main/assets/shared
cp -r shared desktop/web/shared
cp -r shared android/app/src/main/assets/shared
