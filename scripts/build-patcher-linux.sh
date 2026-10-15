set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
test -f "$root/resources/hdiffpatch/hpatchz-linux-x86_64" || {
	echo "resources/hdiffpatch/hpatchz-linux-x86_64 is missing: the patch program would not be built in." >&2
	exit 1
}
mkdir -p "$root/dist"

docker run --rm \
	--mount "type=bind,source=$root,target=/src,readonly" \
	--mount "type=bind,source=$root/dist,target=/out" \
	--mount "type=volume,source=openshores-patcher-cargo,target=/usr/local/cargo/registry" \
	rust:1-alpine sh -euc '
		apk add --no-cache musl-dev gcc g++ make cmake perl linux-headers > /dev/null
		# Only the sources: the build folders beside them are large and of no use here.
		mkdir -p /work/patcher /work/src-tauri /work/resources
		cp -r /src/patcher/Cargo.toml /src/patcher/Cargo.lock /src/patcher/build.rs /src/patcher/src /work/patcher/
		cp -r /src/src-tauri/src /work/src-tauri/
		cp -r /src/resources/hdiffpatch /work/resources/
		cp /src/package.json /work/
		cd /work/patcher
		cargo test --release 2>&1 | grep -E "^test result|FAILED|panicked|^error" || true
		cargo build --release 2>&1 | tail -2
		cp target/release/openshores-patcher /out/openshores-patcher-linux-x86_64
	'
cd "$root/dist"
chmod +x openshores-patcher-linux-x86_64 2>/dev/null || true
file openshores-patcher-linux-x86_64 2>/dev/null || true
sha256sum openshores-patcher-linux-x86_64 | tee openshores-patcher-linux-x86_64.sha256
