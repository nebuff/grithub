#!/usr/bin/env python3
"""Fill package.yaml's artifact digest/size and the bundled icon's digest/size."""

from hashlib import sha256
from pathlib import Path
import re

root = Path(__file__).resolve().parent.parent
wasm = (root / "app.wasm").read_bytes()
manifest_path = root / "package.yaml"
manifest = manifest_path.read_text()
digest = sha256(wasm).hexdigest()

manifest, sha_count = re.subn(
    r"(?m)^(\s*sha256:\s*).*$", rf"\g<1>{digest}", manifest, count=1
)
manifest, size_count = re.subn(
    r"(?m)^(\s*size:\s*).*$", rf"\g<1>{len(wasm)}", manifest, count=1
)
if sha_count != 1 or size_count != 1:
    raise SystemExit("package.yaml must have one artifact sha256 and one size")
icon_path = root / "icon.ico"
if icon_path.is_file():
    icon = icon_path.read_bytes()
    icon_digest = sha256(icon).hexdigest()
    manifest, icon_sha_count = re.subn(
        r"(?m)^(\s*icon_sha256:\s*).*$", rf"\g<1>{icon_digest}", manifest, count=1
    )
    manifest, icon_size_count = re.subn(
        r"(?m)^(\s*icon_size:\s*).*$", rf"\g<1>{len(icon)}", manifest, count=1
    )
    if icon_sha_count != 1 or icon_size_count != 1:
        raise SystemExit("package.yaml must have one icon_sha256 and one icon_size")
    print(f"icon.ico: {len(icon)} bytes")
    print(f"icon sha: {icon_digest}")
manifest_path.write_text(manifest)
print(f"app.wasm: {len(wasm)} bytes")
print(f"app sha:  {digest}")
