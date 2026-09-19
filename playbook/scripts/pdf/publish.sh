#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
export PATH="$root/.svelte-kit/pdf-tools/bin:$PATH"
npx --yes pnpm@11.5.3 --dir "$root" check
npx --yes pnpm@11.5.3 --dir "$root" pdf
uv run --no-project python "$root/scripts/pdf/check.py"
