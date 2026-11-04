#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."
# WSL shells do not necessarily load the user's Node environment.
if test -f "${NVM_DIR:-$HOME/.nvm}/nvm.sh"; then
  source "${NVM_DIR:-$HOME/.nvm}/nvm.sh"
  nvm use --silent
fi
if command -v corepack >/dev/null; then
  pnpm_cmd=(corepack pnpm)
else
  pnpm_cmd=(pnpm)
fi
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$PWD/target}"
python3 deploy/native/test_upgrade.py
cargo fmt --all -- --check
cargo test --workspace --locked
cargo build --release --locked --target x86_64-unknown-linux-musl
(cd web && export CI=true && "${pnpm_cmd[@]}" install --frozen-lockfile && "${pnpm_cmd[@]}" typecheck && "${pnpm_cmd[@]}" build)
mkdir -p dist
# A fresh staging directory prevents removed assets from leaking into a release.
stage=$(mktemp -d "$PWD/dist/build.XXXXXXXX")
mkdir "$stage/web"
install -m 755 "$CARGO_TARGET_DIR/x86_64-unknown-linux-musl/release/tide" "$stage/tide"
cp -a web/out/. "$stage/web/"
cp -a deploy/native "$stage/deploy"
(cd "$stage" && find . -type f ! -name SHA256SUMS -print0 | sort -z | xargs -0 sha256sum > SHA256SUMS)
tar -czf dist/tide-linux-amd64.tgz.next -C "$stage" .
mv -f dist/tide-linux-amd64.tgz.next dist/tide-linux-amd64.tgz
echo 'Built dist/tide-linux-amd64.tgz (Rust binary + Next.js static assets)'
