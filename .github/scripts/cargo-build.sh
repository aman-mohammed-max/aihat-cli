#!/bin/sh
# Build the release binary. When it fails, the end of the build output goes
# into a GitHub annotation, which can be read without access to the logs.
if cargo build --locked --release > cargo-build.log 2>&1; then
  tail -n 3 cargo-build.log
  exit 0
fi
cat cargo-build.log
message=$(tail -n 40 cargo-build.log | sed -e 's/%/%25/g' -e 's/\r//' | awk '{ printf "%s%%0A", $0 }')
echo "::error title=cargo build failed::${message}"
exit 1
